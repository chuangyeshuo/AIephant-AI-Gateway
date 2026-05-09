use std::{
    future::Future,
    pin::{Pin, pin},
    str::FromStr,
    task::{Context, Poll},
};

use async_openai::types::{
    CreateChatCompletionRequest, CreateCompletionRequest, CreateEmbeddingRequest,
    CreateImageRequest, ImageModel, responses::CreateResponse,
};
use bytes::Bytes;
use futures::{future::BoxFuture, ready};
use http::uri::PathAndQuery;
use http_body_util::{BodyExt, combinators::Collect};
use pin_project_lite::pin_project;
use tower::Service as _;

use crate::{
    app_state::AppState,
    default_model::choose_default_gateway_model,
    endpoints::{ApiEndpoint, anthropic::Anthropic, openai::OpenAI},
    error::{
        api::ApiError, init::InitError, internal::InternalError, invalid_req::InvalidRequestError,
    },
    middleware::{
        large_context::maybe_transform_unified_api_chat_request,
        model_support::model_field_from_json_body,
    },
    router::{
        direct::{DirectProxies, DirectProxyService},
        target_provider_resolve::{provider_from_bare_model, resolve_unified_target_provider},
    },
    types::{
        extensions::{
            AuthContext, MasterKeyUnifiedModelPassthrough, UnifiedChatCompletionsResponsesBridge,
            UnifiedImplicitModelFallbackContext, VkPolicy,
        },
        provider::InferenceProvider,
        request::Request,
        response::Response,
    },
    virtual_key::enforce::check_model_access,
};

#[derive(Debug, Clone)]
pub struct Service {
    direct_proxies: DirectProxies,
    app_state: AppState,
}

impl Service {
    pub async fn new(app_state: &AppState) -> Result<Self, InitError> {
        let direct_proxies = DirectProxies::new(app_state).await?;
        Ok(Self {
            direct_proxies,
            app_state: app_state.clone(),
        })
    }
}

impl tower::Service<Request> for Service {
    type Response = Response;
    type Error = ApiError;
    type Future = ResponseFuture;

    #[inline]
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    #[tracing::instrument(name = "unified_api", skip_all)]
    fn call(&mut self, req: Request) -> Self::Future {
        if std::env::var_os("AI_GATEWAY_DEBUG_UNIFIED").is_some() {
            tracing::info!("[unified_api] call: body collection started");
        }
        let (parts, body) = req.into_parts();
        let direct_proxies = self.direct_proxies.clone();
        let app_state = self.app_state.clone();
        let collect_future = body.collect();
        ResponseFuture::new(collect_future, parts, direct_proxies, app_state)
    }
}

pin_project! {
    #[project = StateProj]
    enum State {
        CollectBody {
            #[pin]
            collect_future: Collect<axum_core::body::Body>,
            parts: Option<http::request::Parts>,
        },
        /// `pre_transformed`: when `true`, body already passed `maybe_transform` (and optional default `model` injection).
        DetermineProvider {
            collected_body: Option<Bytes>,
            parts: Option<http::request::Parts>,
            pre_transformed: bool,
        },
        AwaitDefaultModel {
            #[pin]
            fut: BoxFuture<'static, Result<Bytes, ApiError>>,
            parts: Option<http::request::Parts>,
        },
        InitProxy {
            request: Option<Request>,
            provider: InferenceProvider,
        },
        Proxy {
            #[pin]
            response_future: <DirectProxyService as tower::Service<Request>>::Future,
        },
    }
}

pin_project! {
    pub struct ResponseFuture {
        #[pin]
        state: State,
        direct_proxies: DirectProxies,
        app_state: AppState,
    }
}

impl ResponseFuture {
    pub fn new(
        collect_future: Collect<axum_core::body::Body>,
        parts: http::request::Parts,
        direct_proxies: DirectProxies,
        app_state: AppState,
    ) -> Self {
        Self {
            state: State::CollectBody {
                collect_future,
                parts: Some(parts),
            },
            direct_proxies,
            app_state,
        }
    }
}

pub enum UnifiedApi {
    ChatCompletions,
    Completions,
    Embeddings,
    ImageGenerations,
    Responses,
    Messages,
}

impl TryFrom<&str> for UnifiedApi {
    type Error = InvalidRequestError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "chat/completions" => Ok(Self::ChatCompletions),
            "completions" => Ok(Self::Completions),
            "embeddings" => Ok(Self::Embeddings),
            "images/generations" => Ok(Self::ImageGenerations),
            "responses" => Ok(Self::Responses),
            "messages" => Ok(Self::Messages),
            _ => Err(InvalidRequestError::UnsupportedEndpoint(value.to_string())),
        }
    }
}

/// Custom `master_key` with `base_url` and a model-resolved provider that has
/// no direct-proxy stack: skip VK allow/block lists; upstream validates model.
fn skip_model_access_custom_master_fallback(
    direct_proxies: &DirectProxies,
    extensions: &http::Extensions,
    compat: bool,
    model: &str,
) -> bool {
    if compat {
        return false;
    }
    let Some(auth) = extensions.get::<AuthContext>() else {
        return false;
    };
    if !auth.is_custom_provider || auth.master_key_base_url.is_none() {
        return false;
    }
    let Ok(p) = provider_from_bare_model(model) else {
        return false;
    };
    direct_proxies.get(&p).is_none()
}

fn image_model_routing_name(model: &ImageModel) -> String {
    match model {
        ImageModel::DallE2 => "dall-e-2".to_string(),
        ImageModel::DallE3 => "dall-e-3".to_string(),
        ImageModel::Other(s) => s.clone(),
    }
}

/// Inject `model` into the body; if a non-empty `model` already exists, return
/// unchanged.
async fn inject_default_model_into_body_unified(
    app: AppState,
    path: String,
    body: Bytes,
    auth: AuthContext,
    vk: VkPolicy,
) -> Result<Bytes, ApiError> {
    if std::env::var_os("AI_GATEWAY_DEBUG_UNIFIED").is_some() {
        tracing::info!("[unified_api] inject_default_model_into_body: entered path={path}");
    }
    tracing::warn!(path = %path, "inject_default_model_into_body_unified: entered");
    if model_field_from_json_body(&body).is_some() {
        return Ok(body);
    }
    let chosen = choose_default_gateway_model(&app, &path, &auth, &vk).await?;
    let mut v: serde_json::Value =
        serde_json::from_slice(&body).map_err(InvalidRequestError::InvalidRequestBody)?;
    v["model"] = serde_json::Value::String(chosen);
    let out = serde_json::to_vec(&v).map_err(InvalidRequestError::InvalidRequestBody)?;
    Ok(Bytes::from(out))
}

fn implicit_default_model_fallback_context(
    path: &str,
    body: &Bytes,
) -> Option<UnifiedImplicitModelFallbackContext> {
    if path != "chat/completions" {
        return None;
    }
    let selected_model = model_field_from_json_body(body)?.to_string();
    Some(UnifiedImplicitModelFallbackContext { selected_model })
}

/// For `POST .../chat/completions`, if the body looks like OpenAI Responses
/// (`input`, no top-level `messages`), route as `/responses`: update
/// `ApiEndpoint` and `PathAndQuery` in extensions so Chat Completions serde
/// does not fail (e.g. Cursor + `gpt-5.4` still POSTs to chat but sends
/// Responses JSON).
fn apply_chat_completions_body_redirect_if_needed(
    path: &str,
    body: &Bytes,
    parts: &mut http::request::Parts,
) -> Result<String, ApiError> {
    if path != "chat/completions" {
        return Ok(path.to_string());
    }
    let value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return Ok(path.to_string()),
    };
    let Some(obj) = value.as_object() else {
        return Ok(path.to_string());
    };
    if obj.contains_key("messages") {
        return Ok(path.to_string());
    }
    if !obj.contains_key("input") {
        return Ok(path.to_string());
    }
    parts
        .extensions
        .insert(UnifiedChatCompletionsResponsesBridge);
    parts
        .extensions
        .insert(ApiEndpoint::OpenAI(OpenAI::responses()));
    let pq = parts
        .extensions
        .get::<PathAndQuery>()
        .ok_or(InternalError::ExtensionNotFound("PathAndQuery"))?;
    let new_pq_str = match pq.query() {
        Some(q) => format!("responses?{q}"),
        None => "responses".to_string(),
    };
    let new_pq = PathAndQuery::from_str(&new_pq_str).map_err(InternalError::InvalidUri)?;
    parts.extensions.insert(new_pq);
    tracing::debug!(
        "unified_api: chat/completions body has Responses API shape (input, \
         no messages); routing as responses"
    );
    Ok("responses".to_string())
}

impl Future for ResponseFuture {
    type Output = Result<Response, ApiError>;

    #[allow(clippy::too_many_lines)]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        loop {
            match this.state.as_mut().project() {
                StateProj::CollectBody {
                    collect_future,
                    parts,
                } => {
                    let collected = match ready!(pin!(collect_future).poll(cx)) {
                        Ok(collected) => collected,
                        Err(e) => {
                            return Poll::Ready(Err(InternalError::CollectBodyError(e).into()));
                        }
                    };
                    let collected_bytes = collected.to_bytes();
                    if std::env::var_os("AI_GATEWAY_DEBUG_UNIFIED").is_some() {
                        tracing::info!(
                            "[unified_api] body collected, len={}",
                            collected_bytes.len()
                        );
                    }
                    let mut parts = parts.take().expect("future polled after completion");
                    let Some(extracted_path_and_query) = parts.extensions.get::<PathAndQuery>()
                    else {
                        return Poll::Ready(Err(
                            InternalError::ExtensionNotFound("PathAndQuery").into()
                        ));
                    };

                    let unified = UnifiedApi::try_from(extracted_path_and_query.path())?;
                    let api = match unified {
                        UnifiedApi::ChatCompletions => {
                            ApiEndpoint::OpenAI(OpenAI::chat_completions())
                        }
                        UnifiedApi::Completions => ApiEndpoint::OpenAI(OpenAI::completions()),
                        UnifiedApi::Embeddings => ApiEndpoint::OpenAI(OpenAI::embeddings()),
                        UnifiedApi::ImageGenerations => {
                            ApiEndpoint::OpenAI(OpenAI::image_generations())
                        }
                        UnifiedApi::Responses => ApiEndpoint::OpenAI(OpenAI::responses()),
                        UnifiedApi::Messages => ApiEndpoint::Anthropic(Anthropic::messages()),
                    };
                    parts.extensions.insert(api);

                    this.state.set(State::DetermineProvider {
                        collected_body: Some(collected_bytes),
                        parts: Some(parts),
                        pre_transformed: false,
                    });
                }
                StateProj::AwaitDefaultModel { mut fut, parts } => {
                    let res = if let std::task::Poll::Ready(r) = fut.as_mut().poll(cx) {
                        r
                    } else {
                        return std::task::Poll::Pending;
                    };
                    let body = match res {
                        Ok(b) => b,
                        Err(e) => {
                            return std::task::Poll::Ready(Err(e));
                        }
                    };
                    let mut parts = parts.take().expect("future polled after completion");
                    let path = parts
                        .extensions
                        .get::<PathAndQuery>()
                        .map(|value| value.path().to_string())
                        .unwrap_or_default();
                    if let Some(ctx) = implicit_default_model_fallback_context(&path, &body) {
                        parts.extensions.insert(ctx);
                    }
                    this.state.set(State::DetermineProvider {
                        collected_body: Some(body),
                        parts: Some(parts),
                        pre_transformed: true,
                    });
                }
                StateProj::DetermineProvider {
                    collected_body,
                    parts,
                    pre_transformed,
                } => {
                    let original_body = collected_body
                        .take()
                        .expect("future polled after completion");
                    let mut parts = parts.take().expect("future polled after completion");
                    let mut path = parts
                        .extensions
                        .get::<PathAndQuery>()
                        .ok_or(InternalError::ExtensionNotFound("PathAndQuery"))?
                        .path()
                        .to_string();
                    let body = if *pre_transformed {
                        original_body
                    } else {
                        maybe_transform_unified_api_chat_request(
                            this.app_state,
                            &mut parts,
                            original_body,
                        )?
                    };
                    if !*pre_transformed && model_field_from_json_body(&body).is_none() {
                        let Some(auth) = parts.extensions.get::<AuthContext>().cloned() else {
                            return Poll::Ready(Err(InvalidRequestError::NoModelAvailable.into()));
                        };
                        let Some(vk) = parts.extensions.get::<VkPolicy>().cloned() else {
                            return Poll::Ready(Err(InvalidRequestError::NoModelAvailable.into()));
                        };
                        let app = this.app_state.clone();
                        let path_c = path.clone();
                        this.state.set(State::AwaitDefaultModel {
                            fut: Box::pin(inject_default_model_into_body_unified(
                                app, path_c, body, auth, vk,
                            )),
                            parts: Some(parts),
                        });
                        // Must `continue` to the next `match` round and
                        // immediately `poll` the inject
                        // future. Returning `Pending` here without having
                        // polled the child future can
                        // strand the task on some executor paths once the body
                        // is fully read and no I/O
                        // remains (when `model` is present we skip
                        // `AwaitDefaultModel`, so this path matters).
                        continue;
                    }
                    path =
                        apply_chat_completions_body_redirect_if_needed(&path, &body, &mut parts)?;
                    let compat = this.app_state.config().compat_mode;
                    // Owned clone (cheap: typically 0–1 elements) so the borrow
                    // does not conflict with the later
                    // `parts.extensions.insert(provider.clone())`.
                    let allowed_owned = parts
                        .extensions
                        .get::<AuthContext>()
                        .and_then(|a| a.master_key_allowed_providers.clone());
                    let allowed = allowed_owned.as_deref();

                    let check_access = |model: &str| -> Result<(), ApiError> {
                        if !skip_model_access_custom_master_fallback(
                            &this.direct_proxies,
                            &parts.extensions,
                            compat,
                            model,
                        ) {
                            check_model_access(&parts.extensions, model)?;
                        }
                        Ok(())
                    };

                    let (provider, out_body) = match path.as_str() {
                        "chat/completions" => {
                            let d = serde_json::from_slice::<CreateChatCompletionRequest>(&body)
                                .map_err(InvalidRequestError::InvalidRequestBody)?;
                            if let Err(e) = check_access(&d.model) {
                                this.app_state.0.metrics.vk.model_denied.add(1, &[]);
                                tracing::debug!(
                                    model = %d.model,
                                    "virtual key model access denied"
                                );
                                return Poll::Ready(Err(e));
                            }
                            let provider = resolve_unified_target_provider(
                                compat,
                                InferenceProvider::OpenAI,
                                &d.model,
                                allowed,
                            )?;
                            (provider, body)
                        }
                        "completions" => {
                            let r = serde_json::from_slice::<CreateCompletionRequest>(&body)
                                .map_err(InvalidRequestError::InvalidRequestBody)?;
                            if let Err(e) = check_access(&r.model) {
                                this.app_state.0.metrics.vk.model_denied.add(1, &[]);
                                return Poll::Ready(Err(e));
                            }
                            let provider = resolve_unified_target_provider(
                                compat,
                                InferenceProvider::OpenAI,
                                &r.model,
                                allowed,
                            )?;
                            (provider, body)
                        }
                        "embeddings" => {
                            let r = serde_json::from_slice::<CreateEmbeddingRequest>(&body)
                                .map_err(InvalidRequestError::InvalidRequestBody)?;
                            if let Err(e) = check_access(&r.model) {
                                this.app_state.0.metrics.vk.model_denied.add(1, &[]);
                                return Poll::Ready(Err(e));
                            }
                            let provider = resolve_unified_target_provider(
                                compat,
                                InferenceProvider::OpenAI,
                                &r.model,
                                allowed,
                            )?;
                            (provider, body)
                        }
                        "images/generations" => {
                            let r = serde_json::from_slice::<CreateImageRequest>(&body)
                                .map_err(InvalidRequestError::InvalidRequestBody)?;
                            let model_s = r
                                .model
                                .as_ref()
                                .ok_or(InvalidRequestError::MissingModelId)?;
                            let name = image_model_routing_name(model_s);
                            if let Err(e) = check_access(&name) {
                                this.app_state.0.metrics.vk.model_denied.add(1, &[]);
                                return Poll::Ready(Err(e));
                            }
                            let provider = resolve_unified_target_provider(
                                compat,
                                InferenceProvider::OpenAI,
                                &name,
                                allowed,
                            )?;
                            (provider, body)
                        }
                        "responses" => {
                            let d = serde_json::from_slice::<CreateResponse>(&body)
                                .map_err(InvalidRequestError::InvalidRequestBody)?;
                            if let Err(e) = check_access(&d.model) {
                                this.app_state.0.metrics.vk.model_denied.add(1, &[]);
                                tracing::debug!(
                                    model = %d.model,
                                    "virtual key model access denied"
                                );
                                return Poll::Ready(Err(e));
                            }
                            let provider = resolve_unified_target_provider(
                                compat,
                                InferenceProvider::OpenAI,
                                &d.model,
                                allowed,
                            )?;
                            (provider, body)
                        }
                        "messages" => {
                            use anthropic_ai_sdk::types::message::CreateMessageParams;
                            let r = serde_json::from_slice::<CreateMessageParams>(&body)
                                .map_err(InvalidRequestError::InvalidRequestBody)?;
                            if let Err(e) = check_access(&r.model) {
                                this.app_state.0.metrics.vk.model_denied.add(1, &[]);
                                return Poll::Ready(Err(e));
                            }
                            let provider = resolve_unified_target_provider(
                                compat,
                                InferenceProvider::Anthropic,
                                &r.model,
                                allowed,
                            )?;
                            (provider, body)
                        }
                        _ => {
                            return Poll::Ready(Err(InvalidRequestError::NotFound(path).into()));
                        }
                    };
                    parts.extensions.insert(provider.clone());
                    let request = Request::from_parts(parts, axum_core::body::Body::from(out_body));
                    this.state.set(State::InitProxy {
                        request: Some(request),
                        provider,
                    });
                }
                StateProj::InitProxy { request, provider } => {
                    let mut request = request.take().expect("future polled after completion");
                    let mut direct_proxy = match this.direct_proxies.get(provider).cloned() {
                        Some(p) => p,
                        None => {
                            let custom_fallback = request
                                .extensions()
                                .get::<AuthContext>()
                                .is_some_and(|auth| {
                                    auth.is_custom_provider && auth.master_key_base_url.is_some()
                                });
                            if !custom_fallback {
                                tracing::warn!(
                                    provider = %provider,
                                    "requested provider is not configured for direct proxy"
                                );
                                return Poll::Ready(Err(InvalidRequestError::UnsupportedProvider(
                                    provider.clone(),
                                )
                                .into()));
                            }
                            let auth = request
                                .extensions()
                                .get::<AuthContext>()
                                .expect("custom_fallback implies AuthContext");
                            tracing::debug!(
                                parsed_provider = %provider,
                                master_key_id = ?auth.master_key_id,
                                vk_prefix = %auth.virtual_key_prefix,
                                "unified_api: direct proxy miss for parsed provider; \
                                 falling back via master_key base_url",
                            );
                            request
                                .extensions_mut()
                                .insert(MasterKeyUnifiedModelPassthrough);
                            let carrier = this
                                .direct_proxies
                                .get(&InferenceProvider::Custom)
                                .cloned()
                                .map(|p| ("custom", p))
                                .or_else(|| {
                                    this.direct_proxies
                                        .get(&InferenceProvider::OpenAI)
                                        .cloned()
                                        .map(|p| ("openai", p))
                                });
                            let Some((carrier_name, proxy)) = carrier else {
                                tracing::warn!(
                                    parsed_provider = %provider,
                                    "unified_api: custom master_key base_url set but \
                                     neither Custom nor OpenAI direct proxy stack exists"
                                );
                                return Poll::Ready(Err(InvalidRequestError::UnsupportedProvider(
                                    provider.clone(),
                                )
                                .into()));
                            };
                            tracing::debug!(fallback_carrier = carrier_name);
                            proxy
                        }
                    };
                    let response_future = direct_proxy.call(request);
                    this.state.set(State::Proxy { response_future });
                }
                StateProj::Proxy { response_future } => {
                    let response = ready!(response_future.poll(cx)).map_err(|_| {
                        tracing::error!(
                            "encountered error from what should be \
                                 infallible service"
                        );
                        InternalError::Internal
                    })?;
                    return Poll::Ready(Ok(response));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bytes::Bytes;
    use http::uri::PathAndQuery;

    use super::{
        apply_chat_completions_body_redirect_if_needed, implicit_default_model_fallback_context,
    };
    use crate::endpoints::{ApiEndpoint, openai::OpenAI};

    #[test]
    fn implicit_default_context_only_applies_to_chat_completions() {
        let body = Bytes::from(
            serde_json::json!({
                "model": "openai/gpt-5.4",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );

        let ctx = implicit_default_model_fallback_context("chat/completions", &body)
            .expect("chat completions should produce fallback context");
        assert_eq!(ctx.selected_model, "openai/gpt-5.4");

        assert!(implicit_default_model_fallback_context("responses", &body).is_none());
        assert!(implicit_default_model_fallback_context("embeddings", &body).is_none());
    }

    #[test]
    fn implicit_default_context_requires_model_in_body() {
        let body = Bytes::from(
            serde_json::json!({
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );

        assert!(implicit_default_model_fallback_context("chat/completions", &body).is_none());
    }

    #[test]
    fn chat_completions_redirects_to_responses_when_input_without_messages() {
        let body =
            Bytes::from(r#"{"model":"openai/gpt-5.4","input":[{"role":"user","content":"hi"}]}"#);
        let mut parts = http::Request::builder().body(()).unwrap().into_parts().0;
        parts
            .extensions
            .insert(PathAndQuery::from_str("chat/completions").unwrap());
        parts
            .extensions
            .insert(ApiEndpoint::OpenAI(OpenAI::chat_completions()));

        let out =
            apply_chat_completions_body_redirect_if_needed("chat/completions", &body, &mut parts)
                .unwrap();
        assert_eq!(out, "responses");
        assert_eq!(
            parts.extensions.get::<ApiEndpoint>(),
            Some(&ApiEndpoint::OpenAI(OpenAI::responses()))
        );
        let pq = parts.extensions.get::<PathAndQuery>().unwrap();
        assert_eq!(pq.path(), "responses");
        assert!(pq.query().is_none());
    }

    #[test]
    fn chat_completions_redirect_preserves_query() {
        let body = Bytes::from(r#"{"model":"m","input":[]}"#);
        let mut parts = http::Request::builder().body(()).unwrap().into_parts().0;
        parts
            .extensions
            .insert(PathAndQuery::from_str("chat/completions?trace=1").unwrap());

        apply_chat_completions_body_redirect_if_needed("chat/completions", &body, &mut parts)
            .unwrap();

        let pq = parts.extensions.get::<PathAndQuery>().unwrap();
        assert_eq!(pq.path(), "responses");
        assert_eq!(pq.query(), Some("trace=1"));
    }

    #[test]
    fn chat_completions_no_redirect_when_messages_present() {
        let body =
            Bytes::from(r#"{"model":"openai/x","messages":[{"role":"user","content":"hi"}]}"#);
        let mut parts = http::Request::builder().body(()).unwrap().into_parts().0;
        parts
            .extensions
            .insert(PathAndQuery::from_str("chat/completions").unwrap());

        let out =
            apply_chat_completions_body_redirect_if_needed("chat/completions", &body, &mut parts)
                .unwrap();
        assert_eq!(out, "chat/completions");
    }

    #[test]
    fn chat_completions_no_redirect_without_input() {
        let body = Bytes::from(r#"{"model":"openai/x"}"#);
        let mut parts = http::Request::builder().body(()).unwrap().into_parts().0;
        parts
            .extensions
            .insert(PathAndQuery::from_str("chat/completions").unwrap());

        let out =
            apply_chat_completions_body_redirect_if_needed("chat/completions", &body, &mut parts)
                .unwrap();
        assert_eq!(out, "chat/completions");
    }
}
