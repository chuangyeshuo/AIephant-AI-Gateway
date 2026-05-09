use std::{
    str::FromStr,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::{BufMut, Bytes, BytesMut};
use futures::{TryStreamExt, future::BoxFuture};
use http::uri::PathAndQuery;
use tracing::{Instrument, info_span};

use crate::{
    app_state::AppState,
    endpoints::{
        ApiEndpoint,
        anthropic::Anthropic,
        openai::{OpenAI, OpenAICompatibleChatCompletionRequest},
    },
    error::{
        api::ApiError, internal::InternalError,
        invalid_req::InvalidRequestError, mapper::MapperError,
        stream::StreamError,
    },
    middleware::mapper::{
        chat_completion_role_normalize::lenient_openai_chat_roles_for_target_endpoint,
        envelope::RequestEnvelope,
        profile_resolver::resolve_mapper_metadata,
        registry::EndpointConverterRegistry,
        unified_responses_chat_compat::{
            BridgeStreamState, non_stream_responses_body_to_chat_completion,
        },
    },
    types::{
        extensions::{
            MapperContext, MapperProfileContext,
            MasterKeyUnifiedModelPassthrough, RequestContext,
            UnifiedChatCompletionsResponsesBridge,
        },
        model_id::ModelId,
        provider::InferenceProvider,
        request::Request,
        response::Response,
    },
    virtual_key::enforce::check_model_access,
};

#[derive(Debug, Clone)]
pub struct Service<S> {
    inner: S,
    endpoint_converter_registry: EndpointConverterRegistry,
    app_state: AppState,
}

impl<S> Service<S> {
    pub fn new(
        inner: S,
        endpoint_converter_registry: EndpointConverterRegistry,
        app_state: AppState,
    ) -> Self {
        Self {
            inner,
            endpoint_converter_registry,
            app_state,
        }
    }
}

impl<S> tower::Service<Request> for Service<S>
where
    S: tower::Service<
            Request,
            Response = http::Response<crate::types::body::Body>,
            Error = ApiError,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = ApiError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    #[inline]
    fn poll_ready(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    #[tracing::instrument(name = "mapper", skip_all)]
    fn call(&mut self, mut req: Request) -> Self::Future {
        // see: https://docs.rs/tower/latest/tower/trait.Service.html#be-careful-when-cloning-inner-services
        let mut inner = self.inner.clone();
        let converter_registry = self.endpoint_converter_registry.clone();
        let app_state = self.app_state.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        Box::pin(async move {
            let mut target_provider = req
                .extensions()
                .get::<InferenceProvider>()
                .cloned()
                .ok_or_else(|| {
                    ApiError::Internal(InternalError::ExtensionNotFound(
                        "InferenceProvider",
                    ))
                })?;
            if req
                .extensions()
                .get::<MasterKeyUnifiedModelPassthrough>()
                .is_some()
            {
                target_provider = InferenceProvider::Custom;
            }
            let extracted_path_and_query = req
                .extensions_mut()
                .remove::<PathAndQuery>()
                .ok_or(ApiError::Internal(InternalError::ExtensionNotFound(
                    "PathAndQuery",
                )))?;
            let source_endpoint =
                req.extensions().get::<ApiEndpoint>().cloned();
            let source_endpoint = source_endpoint.ok_or(ApiError::Internal(
                InternalError::ExtensionNotFound("ApiEndpoint"),
            ))?;
            let source_endpoint_cloned = source_endpoint.clone();
            let target_endpoint =
                ApiEndpoint::mapped(source_endpoint, &target_provider)?;
            let target_endpoint_cloned = target_endpoint.clone();
            // serialization/deserialization should be done on a dedicated
            // thread
            let converter_registry_cloned = converter_registry.clone();
            let source_endpoint_for_req = source_endpoint_cloned.clone();
            let target_endpoint_for_req = target_endpoint_cloned.clone();
            let req = tokio::task::spawn_blocking(move || async move {
                map_request(
                    app_state,
                    converter_registry_cloned,
                    source_endpoint_for_req,
                    target_endpoint_for_req,
                    &extracted_path_and_query,
                    req,
                )
                .instrument(info_span!("map_request"))
                .await
            })
            .await
            .map_err(InternalError::MappingTaskError)?
            .await?;
            let response = inner.call(req).await?;
            let response = tokio::task::spawn_blocking(move || async move {
                map_response(
                    converter_registry,
                    target_endpoint_cloned,
                    source_endpoint_cloned,
                    response,
                )
                .await
            })
            .instrument(info_span!("map_response"))
            .await
            .map_err(InternalError::MappingTaskError)?
            .await?;
            Ok(response)
        })
    }
}

#[allow(clippy::too_many_lines)]
async fn map_request(
    app_state: AppState,
    converter_registry: EndpointConverterRegistry,
    source_endpoint: ApiEndpoint,
    target_endpoint: ApiEndpoint,
    target_path_and_query: &PathAndQuery,
    req: Request,
) -> Result<Request, ApiError> {
    use http_body_util::BodyExt;
    let (mut parts, body) = req.into_parts();
    let body = body
        .collect()
        .await
        .map_err(InternalError::CollectBodyError)?
        .to_bytes();

    let workspace_id = parts
        .extensions
        .get::<std::sync::Arc<RequestContext>>()
        .and_then(|ctx| ctx.auth_context.as_ref())
        .map(|auth| auth.org_id.to_string())
        .unwrap_or_default();

    let (b, header_log) =
        crate::content_filter::prompt_cache::merge_prompt_cache_messages_into_body(
            app_state.redis(),
            &parts.headers,
            &workspace_id,
            body,
            &app_state.0.metrics.vk,
        )
        .await?;
    let body = b;
    if let Some(h) = header_log {
        parts.extensions.insert(h);
    }

    let filter_result =
        match crate::content_filter::evaluate::evaluate_for_vk_request(
            &app_state,
            &parts.headers,
            &parts.extensions,
            &body,
        )
        .await
        {
            Ok(r) => r,
            Err(ApiError::InvalidRequest(
                InvalidRequestError::ContentPolicyDenied { message },
            )) => {
                emit_mapper_policy_deny_log(
                    &app_state,
                    &parts,
                    &body,
                    &message,
                    target_path_and_query,
                );
                return Err(ApiError::InvalidRequest(
                    InvalidRequestError::ContentPolicyDenied { message },
                ));
            }
            Err(e) => return Err(e),
        };
    let mut body = match filter_result.forward_body {
        crate::content_filter::ContentFilterForwardBody::UseOriginal => body,
        crate::content_filter::ContentFilterForwardBody::UseReplaced(b) => b,
    };
    if let Some(ref new_model) = filter_result.change_model {
        let (new_body, original) =
            crate::content_filter::evaluate::apply_model_downgrade(
                body, new_model,
            );
        body = new_body;
        let original_model = original.unwrap_or_default();
        tracing::info!(
            original_model = %original_model,
            downgraded_model = %new_model,
            "content_filter: policy model downgrade applied (router/mapper)"
        );
        parts
            .extensions
            .insert(crate::content_filter::PolicyModelOverride {
                original_model,
                downgraded_model: new_model.clone(),
            });
    }

    if matches!(
        source_endpoint,
        ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_))
    ) {
        let provider = target_endpoint.provider();
        body = crate::middleware::prompt_compression::apply_chat_completions(
            &mut parts, body, &provider,
        )?;
    }

    let converter = converter_registry
        .get_converter(&source_endpoint, &target_endpoint)
        .ok_or_else(|| {
            InternalError::InvalidConverter(
                source_endpoint.clone(),
                target_endpoint.clone(),
            )
        })?;

    let master_key_model_passthrough = parts
        .extensions
        .get::<MasterKeyUnifiedModelPassthrough>()
        .is_some();

    if !master_key_model_passthrough {
        enforce_vk_model_policy_for_source_endpoint(
            &app_state,
            &parts.extensions,
            &source_endpoint,
            &body,
        )?;
    }

    let request_envelope = if master_key_model_passthrough {
        None
    } else {
        RequestEnvelope::from_source_request_bytes(
            &source_endpoint,
            target_endpoint.provider(),
            &body,
        )?
        .map(|request_envelope| {
            let resolved = resolve_mapper_metadata(
                &request_envelope.target_provider,
                Some(request_envelope.raw_model.as_str()),
            )
            .expect("request mapper metadata should resolve");

            request_envelope
                .with_target_capabilities(resolved.capabilities.clone())
                .with_target_rules(resolved.rules.clone())
                .with_resolved_metadata(resolved)
        })
    };

    let (body, request_envelope) = if let Some(request_envelope) =
        request_envelope
    {
        let request_envelope =
            crate::middleware::mapper::request_rule_engine::prepare_request_envelope(
                request_envelope,
            )
            .map_err(InternalError::MapperError)?;
        let body = Bytes::from(
            serde_json::to_vec(&request_envelope.openai_request).map_err(
                |error| InternalError::Serialize {
                    ty: std::any::type_name::<
                        async_openai::types::CreateChatCompletionRequest,
                    >(),
                    error,
                },
            )?,
        );

        (body, Some(request_envelope))
    } else {
        (body, None)
    };

    let unified_responses_bridge_chat_completions_sse = parts
        .extensions
        .remove::<UnifiedChatCompletionsResponsesBridge>()
        .is_some();

    let (body, mut mapper_ctx) = if master_key_model_passthrough {
        match (&source_endpoint, &target_endpoint) {
            (
                ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_)),
                ApiEndpoint::OpenAICompatible {
                    provider,
                    openai_endpoint,
                },
            ) if *openai_endpoint == OpenAI::chat_completions() => {
                master_key_unified_passthrough_chat_completions(
                    body,
                    provider.clone(),
                )?
            }
            _ => converter.convert_req_body(body)?,
        }
    } else {
        converter.convert_req_body(body)?
    };
    mapper_ctx.unified_responses_bridge_chat_completions_sse =
        unified_responses_bridge_chat_completions_sse;
    let base_path = target_endpoint
        .path(mapper_ctx.model.as_ref(), mapper_ctx.is_stream)?;

    let target_path_and_query =
        if let Some(query_params) = target_path_and_query.query() {
            format!("{base_path}?{query_params}")
        } else {
            base_path
        };
    let target_path_and_query = PathAndQuery::from_str(&target_path_and_query)
        .map_err(InternalError::InvalidUri)?;

    let mut req = Request::from_parts(parts, axum_core::body::Body::from(body));
    tracing::trace!(
        source_endpoint = ?source_endpoint,
        target_endpoint = ?target_endpoint,
        target_path_and_query = ?target_path_and_query,
        mapper_ctx = ?mapper_ctx,
        "mapped request"
    );
    if let Some(request_envelope) = request_envelope {
        if let Some(resolved) = request_envelope.resolved_metadata.as_ref() {
            req.extensions_mut().insert(MapperProfileContext {
                provider: request_envelope.target_provider.clone(),
                raw_model: request_envelope.raw_model.clone(),
                non_stream_profile: resolved.non_stream_profile.clone(),
            });
        }
        req.extensions_mut().insert(request_envelope);
    }
    req.extensions_mut().insert(target_path_and_query);
    req.extensions_mut().insert(mapper_ctx);
    req.extensions_mut().insert(target_endpoint);
    Ok(req)
}

fn master_key_unified_passthrough_chat_completions(
    body: Bytes,
    provider: InferenceProvider,
) -> Result<(Bytes, MapperContext), ApiError> {
    use async_openai::types::CreateChatCompletionRequest;

    let req = serde_json::from_slice::<CreateChatCompletionRequest>(&body)
        .map_err(InvalidRequestError::InvalidRequestBody)?;
    let is_stream = req.stream.unwrap_or(false);
    let model = ModelId::from_str_and_provider(provider.clone(), &req.model)
        .map_err(InternalError::MapperError)?;
    let wrapped = OpenAICompatibleChatCompletionRequest {
        provider,
        inner: req,
    };
    let target_bytes = Bytes::from(serde_json::to_vec(&wrapped).map_err(
        |e| InternalError::Serialize {
            ty: std::any::type_name::<OpenAICompatibleChatCompletionRequest>(),
            error: e,
        },
    )?);
    let anthropic_openai_usage = is_stream.then(|| {
        std::sync::Arc::new(std::sync::Mutex::new(
            crate::types::extensions::AnthropicStreamOpenAiUsageState::default(
            ),
        ))
    });
    Ok((
        target_bytes,
        MapperContext {
            is_stream,
            model: Some(model),
            anthropic_openai_usage,
            unified_responses_bridge_chat_completions_sse: false,
        },
    ))
}

pub fn enforce_vk_model_policy_for_source_endpoint(
    app_state: &AppState,
    extensions: &http::Extensions,
    source_endpoint: &ApiEndpoint,
    body: &bytes::Bytes,
) -> Result<(), ApiError> {
    if extensions
        .get::<crate::content_filter::PolicyModelOverride>()
        .is_some()
    {
        return Ok(());
    }
    use anthropic_ai_sdk::types::message::CreateMessageParams;
    use async_openai::types::{
        CreateChatCompletionRequest, CreateCompletionRequest,
        CreateEmbeddingRequest, CreateImageRequest, ImageModel,
        responses::CreateResponse,
    };
    const EP: &str = "router/mapper";
    let deny = |model: &str| {
        if let Err(e) = check_model_access(extensions, model) {
            app_state.0.metrics.vk.model_denied.add(1, &[]);
            Err(e)
        } else {
            Ok(())
        }
    };
    match source_endpoint {
        ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_)) => {
            let req =
                serde_json::from_slice::<CreateChatCompletionRequest>(body)
                    .map_err(InvalidRequestError::InvalidRequestBody)?;
            if let Err(e) = deny(&req.model) {
                tracing::warn!(
                    model = %req.model,
                    endpoint = "router/openai/chat_completions",
                    "virtual key model policy denied router request"
                );
                return Err(e);
            }
        }
        ApiEndpoint::OpenAI(OpenAI::Completions(_)) => {
            let r = serde_json::from_slice::<CreateCompletionRequest>(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;
            if let Err(e) = deny(&r.model) {
                tracing::warn!(model = %r.model, endpoint = EP);
                return Err(e);
            }
        }
        ApiEndpoint::OpenAI(OpenAI::Embeddings(_)) => {
            let r = serde_json::from_slice::<CreateEmbeddingRequest>(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;
            if let Err(e) = deny(&r.model) {
                tracing::warn!(model = %r.model, endpoint = EP);
                return Err(e);
            }
        }
        ApiEndpoint::OpenAI(OpenAI::ImageGenerations(_)) => {
            let r = serde_json::from_slice::<CreateImageRequest>(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;
            let m = r
                .model
                .as_ref()
                .ok_or(InvalidRequestError::MissingModelId)?;
            let name = match m {
                ImageModel::DallE2 => "dall-e-2",
                ImageModel::DallE3 => "dall-e-3",
                ImageModel::Other(s) => s.as_str(),
            };
            if let Err(e) = deny(name) {
                tracing::warn!(model = %name, endpoint = EP);
                return Err(e);
            }
        }
        ApiEndpoint::OpenAI(OpenAI::Responses(_)) => {
            let r = serde_json::from_slice::<CreateResponse>(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;
            if let Err(e) = deny(&r.model) {
                tracing::warn!(model = %r.model, endpoint = EP);
                return Err(e);
            }
        }
        ApiEndpoint::Anthropic(Anthropic::Messages(_)) => {
            let r = serde_json::from_slice::<CreateMessageParams>(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;
            if let Err(e) = deny(&r.model) {
                tracing::warn!(model = %r.model, endpoint = EP);
                return Err(e);
            }
        }
        _ => {}
    }
    Ok(())
}

fn emit_mapper_policy_deny_log(
    app_state: &AppState,
    parts: &http::request::Parts,
    body: &bytes::Bytes,
    deny_message: &str,
    target_path_and_query: &http::uri::PathAndQuery,
) {
    use chrono::Utc;
    use opentelemetry::KeyValue;
    use tokio::time::Instant;
    use tracing::Instrument;
    use uuid::Uuid;

    use crate::{
        logger::service::LoggerService,
        session_headers::parse_session_headers,
        types::{
            body::{BodyReader, TfftTrigger},
            extensions::{
                MapperContext, PromptHeaderForRequestLog, RequestContext,
            },
            provider::InferenceProvider,
            router::RouterId,
        },
    };

    if !app_state.config().alephant.is_observability_enabled() {
        return;
    }
    let req_ctx = match parts.extensions.get::<std::sync::Arc<RequestContext>>()
    {
        Some(ctx) => ctx.clone(),
        None => return,
    };
    let Some(auth_ctx) = req_ctx.auth_context.clone() else {
        return;
    };

    let target_provider = match parts.extensions.get::<InferenceProvider>() {
        Some(p) => p.clone(),
        None => return,
    };

    let target_url = {
        let providers_config = app_state.get_providers_config();
        let Some(provider_config) = providers_config.get(&target_provider)
        else {
            tracing::warn!(
                "policy deny log (mapper): provider not configured, skipping"
            );
            return;
        };
        match provider_config
            .base_url
            .join(target_path_and_query.as_str())
        {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "policy deny log (mapper): failed to build target_url, \
                     skipping"
                );
                return;
            }
        }
    };

    let start_instant = parts
        .extensions
        .get::<Instant>()
        .copied()
        .unwrap_or_else(Instant::now);
    let start_time = parts
        .extensions
        .get::<chrono::DateTime<Utc>>()
        .copied()
        .unwrap_or_else(Utc::now);
    let router_id = parts.extensions.get::<RouterId>().cloned();
    let prompt_header =
        parts.extensions.get::<PromptHeaderForRequestLog>().cloned();
    let prompt_ctx = parts
        .extensions
        .get::<crate::types::extensions::PromptContext>()
        .cloned();
    let session_ctx = parse_session_headers(&parts.headers).ok().flatten();

    let mapper_ctx = MapperContext {
        is_stream: false,
        model: None,
        anthropic_openai_usage: None,
        unified_responses_bridge_chat_completions_sse: false,
    };

    let response_body_bytes =
        crate::content_filter::evaluate::policy_denied_error_response_json(
            deny_message,
        );

    let response_status = http::StatusCode::OK;
    let request_log_id = parts
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
        .unwrap_or_else(Uuid::new_v4);
    let response_log_id = Uuid::new_v4();
    let response_received_at = Utc::now();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = tx.send(bytes::Bytes::from(response_body_bytes));
    drop(tx);

    let (tfft_tx_for_body, _unused_rx) = tokio::sync::oneshot::channel();
    let body_reader = BodyReader::new(
        rx,
        tfft_tx_for_body,
        hyper::body::SizeHint::default(),
        false,
        TfftTrigger::Never,
    );
    let (tfft_tx_for_log, tfft_rx) = tokio::sync::oneshot::channel();
    let _ = tfft_tx_for_log.send(());

    let deployment_target = app_state.config().deployment_target.clone();
    let response_logger = LoggerService::builder()
        .app_state(app_state.clone())
        .auth_ctx(auth_ctx)
        .start_time(start_time)
        .start_instant(start_instant)
        .target_url(target_url)
        .request_headers(parts.headers.clone())
        .request_body(body.clone())
        .response_status(response_status)
        .response_body(body_reader)
        .provider(target_provider)
        .tfft_rx(tfft_rx)
        .mapper_ctx(mapper_ctx)
        .router_id(router_id)
        .deployment_target(deployment_target)
        .request_id(request_log_id)
        .response_id(response_log_id)
        .response_created_at(response_received_at)
        .prompt_ctx(prompt_ctx)
        .prompt_header_for_request_log(prompt_header)
        .session_ctx(session_ctx)
        .build();

    if let Some(marker) = parts
        .extensions
        .get::<crate::types::extensions::RequestLogEmitted>()
    {
        marker.mark();
    }

    let app_state = app_state.clone();
    tokio::spawn(
        async move {
            if let Err(e) = response_logger.log().await {
                let error_str = e.as_ref().to_string();
                app_state
                    .0
                    .metrics
                    .error_count
                    .add(1, &[KeyValue::new("type", error_str)]);
            }
        }
        .instrument(tracing::Span::current()),
    );
}

async fn map_response(
    converter_registry: EndpointConverterRegistry,
    source_endpoint: ApiEndpoint,
    target_endpoint: ApiEndpoint,
    resp: http::Response<crate::types::body::Body>,
) -> Result<Response, ApiError> {
    let mapper_ctx = resp
        .extensions()
        .get::<MapperContext>()
        .cloned()
        .ok_or(InternalError::ExtensionNotFound("MapperContext"))?;
    let is_stream = mapper_ctx.is_stream;
    let anthropic_openai_usage = mapper_ctx.anthropic_openai_usage.clone();
    let bridge_chat_completions =
        mapper_ctx.unified_responses_bridge_chat_completions_sse;
    let (parts, body) = resp.into_parts();

    if bridge_chat_completions && is_stream {
        tracing::trace!(
            "unified responses → Chat Completions SSE bridge (streaming)"
        );
        let state = Arc::new(Mutex::new(BridgeStreamState::default()));
        let mapped_stream = body
            .into_data_stream()
            .map_err(|e| ApiError::StreamError(StreamError::BodyError(e)))
            .try_filter_map(move |bytes| {
                let state = Arc::clone(&state);
                async move {
                    let opt = {
                        let mut guard = state
                            .lock()
                            .expect("responses bridge mutex poisoned");
                        guard.process_upstream_sse_json(&bytes)?
                    };
                    Ok(opt)
                }
            });
        let final_body = axum_core::body::Body::new(
            reqwest::Body::wrap_stream(mapped_stream),
        );
        let new_resp = Response::from_parts(parts, final_body);
        return Ok(new_resp);
    }

    let converter = converter_registry
        .get_converter(&target_endpoint, &source_endpoint)
        .ok_or_else(|| {
            InternalError::InvalidConverter(
                target_endpoint.clone(),
                source_endpoint.clone(),
            )
        })?;

    let lenient_roles =
        lenient_openai_chat_roles_for_target_endpoint(&target_endpoint);

    if is_stream {
        tracing::trace!(
            source_endpoint = ?target_endpoint,
            target_endpoint = ?source_endpoint,
            "mapped streaming response"
        );
        // because we are using our custom body type, and we know it was
        // constructed in the dispatcher from either an SSE stream or a
        // stream of bytes, we can safely assume each frame is a single
        // SSE event in this branch
        let mapped_stream = body
            .into_data_stream()
            .map_err(|e| ApiError::StreamError(StreamError::BodyError(e)))
            .try_filter_map({
                let captured_registry = converter_registry.clone();
                let resp_parts = parts.clone();
                let target_endpoint_cloned = target_endpoint.clone();
                let source_endpoint_cloned = source_endpoint.clone();
                let anthropic_openai_usage = anthropic_openai_usage.clone();
                let lenient_roles_stream = lenient_roles;
                move |bytes| {
                    let registry_for_future = captured_registry.clone();
                    let resp_parts = resp_parts.clone();
                    let target_endpoint = target_endpoint_cloned.clone();
                    let source_endpoint = source_endpoint_cloned.clone();
                    let anthropic_usage_for_chunk =
                        anthropic_openai_usage.clone();
                    async move {
                        let converter = registry_for_future
                            .get_converter(&target_endpoint, &source_endpoint)
                            .ok_or_else(|| {
                                InternalError::InvalidConverter(
                                    target_endpoint.clone(),
                                    source_endpoint.clone(),
                                )
                            })?;

                        let converted_data = converter.convert_resp_body(
                            resp_parts,
                            bytes,
                            is_stream,
                            anthropic_usage_for_chunk.as_ref(),
                            lenient_roles_stream,
                        )?;

                        // add the `data: ` prefix expected by the OpenAI SDK
                        if let Some(converted_data) = converted_data {
                            let mut new_bytes = BytesMut::new();
                            new_bytes.put("data: ".as_bytes());
                            new_bytes.put(converted_data);
                            new_bytes.put("\n\n".as_bytes());
                            let data = new_bytes.freeze();
                            Ok(Some(data))
                        } else {
                            Ok(converted_data)
                        }
                    }
                }
            });
        let final_body = axum_core::body::Body::new(
            reqwest::Body::wrap_stream(mapped_stream),
        );
        let new_resp = Response::from_parts(parts, final_body);
        Ok(new_resp)
    } else {
        use http_body_util::BodyExt;
        let body_bytes = body
            .collect()
            .await
            .map_err(InternalError::CollectBodyError)?
            .to_bytes();

        let mapped_body_bytes = if bridge_chat_completions {
            non_stream_responses_body_to_chat_completion(&body_bytes)?
        } else {
            converter
                .convert_resp_body(
                    parts.clone(),
                    body_bytes,
                    is_stream,
                    None,
                    lenient_roles,
                )?
                .ok_or(MapperError::EmptyResponseBody)
                .map_err(InternalError::MapperError)?
        };

        let final_body = axum_core::body::Body::from(mapped_body_bytes);
        let new_resp = Response::from_parts(parts, final_body);
        tracing::trace!(
            source_endpoint = ?target_endpoint,
            target_endpoint = ?source_endpoint,
            "mapped non-streaming response"
        );
        Ok(new_resp)
    }
}

#[derive(Debug, Clone)]
pub struct Layer {
    endpoint_converter_registry: EndpointConverterRegistry,
    app_state: AppState,
}

impl Layer {
    #[must_use]
    pub fn new(
        endpoint_converter_registry: EndpointConverterRegistry,
        app_state: AppState,
    ) -> Self {
        Self {
            endpoint_converter_registry,
            app_state,
        }
    }
}

impl<S> tower::Layer<S> for Layer {
    type Service = Service<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Service::new(
            inner,
            self.endpoint_converter_registry.clone(),
            self.app_state.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::uri::PathAndQuery;
    use http_body_util::BodyExt;
    use serde_json::{Value, json};

    use crate::{
        app::build_test_app,
        config::Config,
        endpoints::{ApiEndpoint, openai::OpenAI},
        middleware::mapper::{
            envelope::RequestEnvelope, model::ModelMapper,
            registry::EndpointConverterRegistry,
        },
        types::{
            extensions::{MapperProfileContext, PromptCompressionTokenPair},
            provider::InferenceProvider,
        },
    };

    #[tokio::test]
    async fn map_request_runs_post_policy_prompt_compression_on_chat_completions()
     {
        let app = build_test_app(Config::default()).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = EndpointConverterRegistry::new(&model_mapper);
        let source_endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let provider = InferenceProvider::Named("qwen".into());
        let target_endpoint =
            ApiEndpoint::mapped(source_endpoint.clone(), &provider)
                .expect("mapped endpoint");

        let request_body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "qwen/qwen3-32b",
                "messages": [
                    { "role": "user", "content": "  a   b  " },
                ],
            }))
            .expect("request body should serialize"),
        );

        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri("http://router.alephant.test/ai/chat/completions")
            .body(axum_core::body::Body::from(request_body))
            .expect("request should build");

        let mapped = super::map_request(
            app.state.clone(),
            registry,
            source_endpoint,
            target_endpoint,
            &PathAndQuery::from_static("/ai/chat/completions"),
            request,
        )
        .await
        .expect("map request should succeed");

        assert!(
            mapped
                .extensions()
                .get::<PromptCompressionTokenPair>()
                .is_some(),
            "post-policy compression should set PromptCompressionTokenPair"
        );

        let (_, body) = mapped.into_parts();
        let upstream_bytes = body
            .collect()
            .await
            .expect("mapped body should collect")
            .to_bytes();
        let upstream_body: Value = serde_json::from_slice(&upstream_bytes)
            .expect("mapped body should be valid json");
        assert_eq!(upstream_body["messages"][0]["content"], "a b");
    }

    #[tokio::test]
    async fn map_request_applies_shared_request_rules_before_converter() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = EndpointConverterRegistry::new(&model_mapper);
        let source_endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let provider = InferenceProvider::Named("qwen".into());
        let target_endpoint =
            ApiEndpoint::mapped(source_endpoint.clone(), &provider)
                .expect("mapped endpoint");
        let request_body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "qwen/qwen3-32b",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "reasoning_effort": "high"
            }))
            .expect("request body should serialize"),
        );
        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri("http://router.alephant.test/ai/chat/completions")
            .body(axum_core::body::Body::from(request_body))
            .expect("request should build");

        let mapped = super::map_request(
            app.state.clone(),
            registry,
            source_endpoint,
            target_endpoint,
            &PathAndQuery::from_static("/ai/chat/completions"),
            request,
        )
        .await
        .expect("map request should succeed");

        let envelope = mapped
            .extensions()
            .get::<RequestEnvelope>()
            .expect("request envelope should be attached");
        assert!(envelope.request_rule_context.is_some());
        assert!(envelope.openai_request.reasoning_effort.is_none());

        let (_, body) = mapped.into_parts();
        let upstream_bytes = body
            .collect()
            .await
            .expect("mapped body should collect")
            .to_bytes();
        let upstream_body: Value = serde_json::from_slice(&upstream_bytes)
            .expect("mapped body should be valid json");

        assert_eq!(upstream_body["model"], "qwen3-32b");
        assert!(upstream_body["reasoning_effort"].is_null());
    }

    #[tokio::test]
    async fn map_request_preserves_reasoning_effort_for_deepseek_reasoner() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = EndpointConverterRegistry::new(&model_mapper);
        let source_endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let provider = InferenceProvider::Named("deepseek".into());
        let target_endpoint =
            ApiEndpoint::mapped(source_endpoint.clone(), &provider)
                .expect("mapped endpoint");
        let request_body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "deepseek/deepseek-reasoner",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "reasoning_effort": "high"
            }))
            .expect("request body should serialize"),
        );
        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri("http://router.alephant.test/ai/chat/completions")
            .body(axum_core::body::Body::from(request_body))
            .expect("request should build");

        let mapped = super::map_request(
            app.state.clone(),
            registry,
            source_endpoint,
            target_endpoint,
            &PathAndQuery::from_static("/ai/chat/completions"),
            request,
        )
        .await
        .expect("map request should succeed");

        let envelope = mapped
            .extensions()
            .get::<RequestEnvelope>()
            .expect("request envelope should be attached");
        assert_eq!(
            envelope.openai_request.reasoning_effort,
            Some(async_openai::types::ReasoningEffort::High)
        );

        let (_, body) = mapped.into_parts();
        let upstream_bytes = body
            .collect()
            .await
            .expect("mapped body should collect")
            .to_bytes();
        let upstream_body: Value = serde_json::from_slice(&upstream_bytes)
            .expect("mapped body should be valid json");

        assert_eq!(upstream_body["model"], "deepseek-reasoner");
        assert_eq!(upstream_body["reasoning_effort"], json!("high"));
    }

    #[tokio::test]
    async fn map_request_attaches_mapper_profile_context() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = EndpointConverterRegistry::new(&model_mapper);
        let source_endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let provider = InferenceProvider::Named("deepseek".into());
        let target_endpoint =
            ApiEndpoint::mapped(source_endpoint.clone(), &provider)
                .expect("mapped endpoint");
        let request_body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "deepseek/deepseek-reasoner",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ]
            }))
            .expect("request body should serialize"),
        );
        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri("http://router.alephant.test/ai/chat/completions")
            .body(axum_core::body::Body::from(request_body))
            .expect("request should build");

        let mapped = super::map_request(
            app.state.clone(),
            registry,
            source_endpoint,
            target_endpoint,
            &PathAndQuery::from_static("/ai/chat/completions"),
            request,
        )
        .await
        .expect("map request should succeed");

        let profile_context = mapped
            .extensions()
            .get::<MapperProfileContext>()
            .expect("mapper profile context should be attached");

        assert_eq!(profile_context.provider, provider);
        assert_eq!(profile_context.raw_model, "deepseek/deepseek-reasoner");
    }
}
