use std::task::{Context, Poll};

use axum_core::body::Body;
use futures::future::BoxFuture;
use http::Method;
use http_body_util::{BodyExt, Full, Limited};
use tower::{Layer, Service};

use crate::{
    app_state::AppState,
    error::{api::ApiError, internal::InternalError, invalid_req::InvalidRequestError},
    middleware::{
        large_context::{
            headers::parse_large_context_headers, heuristics::extract_fallback_model_candidates,
        },
        model_support::parse::{
            MODEL_SUPPORT_MAX_BODY_BYTES, catalog_redis_key, model_field_from_json_body,
            split_provider_model,
        },
    },
    types::{
        extensions::RequestKind, provider::InferenceProvider, request::Request, response::Response,
    },
};

/// Every **POST** through `MetaRouter` may carry JSON with a top-level `model`.
/// We buffer the body once; if `model` is absent or body is not JSON, we
/// forward without catalog checks. Non-POST requests skip body collection.
#[inline]
fn should_inspect_post_body(method: &Method) -> bool {
    *method == Method::POST
}

fn canonical_provider_code(provider_raw: &str) -> Option<String> {
    InferenceProvider::from_provider_code(provider_raw)
        .ok()
        .map(|provider| provider.as_provider_code().to_string())
}

/// Used by `default_model` and this middleware; same Redis→DB resolution.
pub(crate) async fn gateway_model_supported(
    app_state: &AppState,
    provider_raw: &str,
    model_raw: &str,
) -> Result<bool, ApiError> {
    let canonical_provider = canonical_provider_code(provider_raw);

    let in_redis = if let Some(client) = app_state.redis() {
        let raw_key = catalog_redis_key(provider_raw, model_raw);
        let raw_exists = match client.key_exists(&raw_key).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    key = %raw_key,
                    "model_support: catalog key_exists failed; falling back to DB"
                );
                false
            }
        };
        if raw_exists {
            true
        } else if let Some(canonical_provider) = canonical_provider.as_deref() {
            if canonical_provider.eq_ignore_ascii_case(provider_raw) {
                false
            } else {
                let canonical_key = catalog_redis_key(canonical_provider, model_raw);
                match client.key_exists(&canonical_key).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            key = %canonical_key,
                            "model_support: catalog key_exists failed; falling back to DB"
                        );
                        false
                    }
                }
            }
        } else {
            false
        }
    } else {
        false
    };

    if in_redis {
        return Ok(true);
    }

    let Some(store) = app_state.router_store() else {
        return Ok(true);
    };

    let supported = store
        .gateway_model_pair_supported(provider_raw, model_raw)
        .await
        .map_err(ApiError::Internal)?;
    if supported {
        return Ok(true);
    }

    if let Some(canonical_provider) = canonical_provider.as_deref() {
        if canonical_provider.eq_ignore_ascii_case(provider_raw) {
            Ok(false)
        } else {
            store
                .gateway_model_pair_supported(canonical_provider, model_raw)
                .await
                .map_err(ApiError::Internal)
        }
    } else {
        Ok(false)
    }
}

/// Resolve a bare `model_id` (without `provider/` prefix) to a full
/// `provider/model_id` string. Looks up `BareModelExpandIndex` first,
/// falls back to DB.
///
/// Returns:
/// - `Ok(full_model)` when exactly one provider matches.
/// - `Err(UnsupportedGatewayModel)` when no provider matches.
/// - `Err(AmbiguousBareModel)` when multiple providers match.
async fn resolve_bare_model(app_state: &AppState, bare_model_id: &str) -> Result<String, ApiError> {
    let index = app_state.get_bare_model_expand_index();
    let mut candidates = index.gateway_models_for_bare_id(bare_model_id);

    if candidates.is_empty() {
        if let Some(store) = app_state.router_store() {
            let db_rows = store
                .find_providers_for_bare_model(bare_model_id)
                .await
                .map_err(ApiError::Internal)?;
            candidates = db_rows
                .into_iter()
                .map(|(code, model_id)| format!("{code}/{model_id}"))
                .collect();
        }
    }

    match candidates.len() {
        0 => Err(ApiError::InvalidRequest(
            InvalidRequestError::UnsupportedGatewayModel(bare_model_id.to_string()),
        )),
        1 => Ok(candidates.into_iter().next().expect("len checked")),
        _ => Err(ApiError::InvalidRequest(
            InvalidRequestError::AmbiguousBareModel {
                model_id: bare_model_id.to_string(),
                candidates,
            },
        )),
    }
}

/// Replace the `"model"` field in a JSON body with `new_model`, returning
/// the re-serialized bytes. Returns `None` if the body is not valid JSON
/// or has no `"model"` field.
fn rewrite_model_in_body(body: &[u8], new_model: &str) -> Option<bytes::Bytes> {
    let mut v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("model")?;
    v["model"] = serde_json::Value::String(new_model.to_string());
    serde_json::to_vec(&v).ok().map(bytes::Bytes::from)
}

#[cfg(test)]
mod tests {
    use http::Method;

    use super::{canonical_provider_code, rewrite_model_in_body, should_inspect_post_body};

    #[test]
    fn inspects_post_only() {
        assert!(should_inspect_post_body(&Method::POST));
        assert!(!should_inspect_post_body(&Method::GET));
        assert!(!should_inspect_post_body(&Method::PUT));
        assert!(!should_inspect_post_body(&Method::PATCH));
    }

    #[test]
    fn canonical_provider_code_maps_gemini_to_google() {
        assert_eq!(canonical_provider_code("gemini").as_deref(), Some("google"));
    }

    #[test]
    fn rewrite_model_replaces_field() {
        let body = br#"{"model":"gpt-4o-mini","messages":[]}"#;
        let rewritten = rewrite_model_in_body(body, "openai/gpt-4o-mini").expect("should rewrite");
        let v: serde_json::Value = serde_json::from_slice(&rewritten).expect("valid json");
        assert_eq!(v["model"], "openai/gpt-4o-mini");
        assert!(v["messages"].is_array());
    }

    #[test]
    fn rewrite_model_no_model_field_returns_none() {
        let body = br#"{"messages":[]}"#;
        assert!(rewrite_model_in_body(body, "openai/gpt-4o-mini").is_none());
    }

    #[test]
    fn rewrite_model_invalid_json_returns_none() {
        let body = b"not json";
        assert!(rewrite_model_in_body(body, "openai/gpt-4o-mini").is_none());
    }

    #[test]
    fn rewrite_model_preserves_other_fields() {
        let body = br#"{"model":"GPT-5","temperature":0.7,"max_tokens":100}"#;
        let rewritten = rewrite_model_in_body(body, "openai/gpt-5").expect("should rewrite");
        let v: serde_json::Value = serde_json::from_slice(&rewritten).expect("valid json");
        assert_eq!(v["model"], "openai/gpt-5");
        assert_eq!(v["temperature"], 0.7);
        assert_eq!(v["max_tokens"], 100);
    }
}

#[derive(Clone)]
pub struct ModelSupportLayer {
    pub app_state: AppState,
}

impl<S> Layer<S> for ModelSupportLayer {
    type Service = ModelSupportService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ModelSupportService {
            inner,
            app_state: self.app_state.clone(),
        }
    }
}

#[derive(Clone)]
pub struct ModelSupportService<S> {
    inner: S,
    app_state: AppState,
}

impl<S> Service<Request> for ModelSupportService<S>
where
    S: Service<Request, Response = Response, Error = ApiError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = ApiError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    #[tracing::instrument(name = "model_support", skip_all)]
    fn call(&mut self, req: Request) -> Self::Future {
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        let app_state = self.app_state.clone();

        Box::pin(async move {
            let (parts, body) = req.into_parts();
            let need_validate = should_inspect_post_body(&parts.method);

            if !need_validate {
                let req = Request::from_parts(parts, body);
                return inner.call(req).await;
            }

            let bytes = match Limited::new(body, MODEL_SUPPORT_MAX_BODY_BYTES)
                .collect()
                .await
            {
                Ok(c) => c.to_bytes(),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "model_support: failed to collect request body"
                    );
                    return Err(ApiError::Internal(InternalError::Internal));
                }
            };

            let request_kind = parts.extensions.get::<RequestKind>().copied();
            let large_context_headers = if matches!(request_kind, Some(RequestKind::UnifiedApi)) {
                Some(
                    parse_large_context_headers(&parts.headers)
                        .map_err(ApiError::InvalidRequest)?,
                )
            } else {
                None
            };
            let body_model = model_field_from_json_body(&bytes);
            let handler_enabled = large_context_headers
                .as_ref()
                .and_then(|headers| headers.handler)
                .is_some();

            let model_candidates = if handler_enabled {
                body_model
                    .as_deref()
                    .or_else(|| {
                        large_context_headers
                            .as_ref()
                            .and_then(|headers| headers.model_override.as_deref())
                    })
                    .map(extract_fallback_model_candidates)
                    .unwrap_or_default()
            } else {
                body_model
                    .clone()
                    .map(|model| vec![model])
                    .unwrap_or_default()
            };

            if model_candidates.is_empty() {
                let req = Request::from_parts(parts, Body::new(Full::new(bytes)));
                return inner.call(req).await;
            }

            if matches!(request_kind, Some(RequestKind::CustomProvider)) {
                let req = Request::from_parts(parts, Body::new(Full::new(bytes)));
                return inner.call(req).await;
            }

            let is_direct_proxy = matches!(request_kind, Some(RequestKind::DirectProxy));
            let mut bytes = bytes;

            for candidate in &model_candidates {
                if is_direct_proxy || candidate.contains('/') {
                    let Ok(parsed) = split_provider_model(candidate) else {
                        return Err(ApiError::InvalidRequest(
                            InvalidRequestError::UnsupportedGatewayModel(candidate.clone()),
                        ));
                    };
                    let supported =
                        gateway_model_supported(&app_state, parsed.provider_raw, parsed.model_raw)
                            .await?;
                    if !supported {
                        return Err(ApiError::InvalidRequest(
                            InvalidRequestError::UnsupportedGatewayModel(candidate.clone()),
                        ));
                    }
                } else {
                    let resolved = resolve_bare_model(&app_state, candidate).await?;
                    if let Some(rewritten) = rewrite_model_in_body(&bytes, &resolved) {
                        bytes = rewritten;
                    }
                    let Ok(parsed) = split_provider_model(&resolved) else {
                        return Err(ApiError::InvalidRequest(
                            InvalidRequestError::UnsupportedGatewayModel(resolved),
                        ));
                    };
                    let supported =
                        gateway_model_supported(&app_state, parsed.provider_raw, parsed.model_raw)
                            .await?;
                    if !supported {
                        return Err(ApiError::InvalidRequest(
                            InvalidRequestError::UnsupportedGatewayModel(resolved),
                        ));
                    }
                }
            }

            let req = Request::from_parts(parts, Body::new(Full::new(bytes)));
            inner.call(req).await
        })
    }
}
