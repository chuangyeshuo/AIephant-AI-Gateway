use std::{
    borrow::Cow,
    collections::HashMap,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use backon::{BackoffBuilder, ConstantBuilder, ExponentialBuilder, Retryable};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::{TryStreamExt, future::BoxFuture};
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, uri::PathAndQuery};
use http_body_util::BodyExt;
use opentelemetry::KeyValue;
use reqwest::RequestBuilder;
use rust_decimal::prelude::ToPrimitive;
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};
use tower::{Service, ServiceBuilder};
use tracing::{Instrument, info_span};
use uuid::Uuid;

use crate::{
    app_state::AppState,
    config::{fallback_bridge, retry::RetryConfig, router::RouterConfig},
    default_model::choose_default_gateway_model_excluding_provider,
    discover::monitor::metrics::EndpointMetricsRegistry,
    dispatcher::{
        client::{Client, ProviderClient},
        extensions::ExtensionsCopier,
    },
    endpoints::ApiEndpoint,
    error::{
        api::ApiError, init::InitError, internal::InternalError, invalid_req::InvalidRequestError,
    },
    logger::service::LoggerService,
    metrics::tfft::TFFTFuture,
    middleware::{
        add_extension::{AddExtensions, AddExtensionsLayer},
        mapper::{model::ModelMapper, registry::EndpointConverterRegistry},
        model_support::split_provider_model,
        prompt_compression,
    },
    session_headers::{SessionHeaders, parse_session_headers, remove_session_headers},
    types::{
        body::{BodyReader, TfftTrigger},
        extensions::{
            LargeContextDecision, MapperContext, MapperProfileContext, PromptCompressionTokenPair,
            PromptContext, PromptHeaderForRequestLog, RequestContext, RequestKind,
            RequestLogEmitted, UnifiedImplicitModelFallbackContext, VkPolicy,
        },
        model_id::ModelId,
        provider::InferenceProvider,
        rate_limit::RateLimitEvent,
        request::Request,
        router::RouterId,
    },
    utils::handle_error::{ErrorHandler, ErrorHandlerLayer},
    virtual_key::enforce::check_model_access,
};

pub type DispatcherFuture =
    BoxFuture<'static, Result<http::Response<crate::types::body::Body>, ApiError>>;
pub type DispatcherService =
    AddExtensions<ErrorHandler<crate::middleware::mapper::Service<Dispatcher>>>;
pub type DispatcherServiceWithoutMapper = AddExtensions<ErrorHandler<Dispatcher>>;

/// Leaf service that dispatches requests to the correct provider.
#[derive(Debug, Clone)]
pub struct Dispatcher {
    client: Client,
    app_state: AppState,
    provider: InferenceProvider,
    /// Is `Some` for load balanced routers, `None` for direct proxies.
    rate_limit_tx: Option<mpsc::Sender<RateLimitEvent>>,
}

type SyncDispatchResponse = (
    http::Response<crate::types::body::Body>,
    crate::types::body::BodyReader,
    oneshot::Receiver<()>,
);

struct SyncDispatchOutcome {
    response: http::Response<crate::types::body::Body>,
    response_body_for_logger: crate::types::body::BodyReader,
    tfft_rx: oneshot::Receiver<()>,
    effective_provider: InferenceProvider,
    effective_target_url: url::Url,
    effective_request_body: Bytes,
}

impl Dispatcher {
    async fn new_inner(
        app_state: AppState,
        router_id: &RouterId,
        provider: InferenceProvider,
        model_mapper: ModelMapper,
    ) -> Result<DispatcherService, InitError> {
        let client = Client::new(&app_state, provider.clone()).await?;
        let rate_limit_tx = app_state.get_rate_limit_tx(router_id).await?;

        let dispatcher = Self {
            client,
            app_state: app_state.clone(),
            provider: provider.clone(),
            rate_limit_tx: Some(rate_limit_tx),
        };
        let converter_registry = EndpointConverterRegistry::new(&model_mapper);

        let extensions_layer = AddExtensionsLayer::builder()
            .inference_provider(provider.clone())
            .router_id(Some(router_id.clone()))
            .build();

        Ok(ServiceBuilder::new()
            .layer(extensions_layer)
            .layer(ErrorHandlerLayer::new(app_state.clone()))
            .layer(crate::middleware::mapper::Layer::new(
                converter_registry,
                app_state.clone(),
            ))
            // other middleware: rate limiting, logging, etc, etc
            // will be added here as well
            .service(dispatcher))
    }

    pub async fn new(
        app_state: AppState,
        router_id: &RouterId,
        router_config: &Arc<RouterConfig>,
        provider: InferenceProvider,
    ) -> Result<DispatcherService, InitError> {
        let model_mapper = ModelMapper::new_for_router(app_state.clone(), router_config.clone());
        Self::new_inner(app_state, router_id, provider, model_mapper).await
    }

    pub async fn new_with_model_id(
        app_state: AppState,
        router_id: &RouterId,
        router_config: &Arc<RouterConfig>,
        provider: InferenceProvider,
        model_id: ModelId,
    ) -> Result<DispatcherService, InitError> {
        let model_mapper =
            ModelMapper::new_with_model_id(app_state.clone(), router_config.clone(), model_id);
        Self::new_inner(app_state, router_id, provider, model_mapper).await
    }

    pub async fn new_direct_proxy(
        app_state: AppState,
        provider: &InferenceProvider,
    ) -> Result<DispatcherService, InitError> {
        let client = Client::new(&app_state, provider.clone()).await?;

        let dispatcher = Self {
            client,
            app_state: app_state.clone(),
            provider: provider.clone(),
            rate_limit_tx: None,
        };
        let model_mapper = ModelMapper::new(app_state.clone());
        let converter_registry = EndpointConverterRegistry::new(&model_mapper);

        let extensions_layer = AddExtensionsLayer::builder()
            .inference_provider(provider.clone())
            .router_id(None)
            .build();

        Ok(ServiceBuilder::new()
            .layer(extensions_layer)
            .layer(ErrorHandlerLayer::new(app_state.clone()))
            .layer(crate::middleware::mapper::Layer::new(
                converter_registry,
                app_state.clone(),
            ))
            // other middleware: rate limiting, logging, etc, etc
            // will be added here as well
            .service(dispatcher))
    }

    pub async fn new_without_mapper(
        app_state: AppState,
        provider: &InferenceProvider,
    ) -> Result<DispatcherServiceWithoutMapper, InitError> {
        let client = Client::new(&app_state, provider.clone()).await?;

        let dispatcher = Self {
            client,
            app_state: app_state.clone(),
            provider: provider.clone(),
            rate_limit_tx: None,
        };

        let extensions_layer = AddExtensionsLayer::builder()
            .inference_provider(provider.clone())
            .router_id(None)
            .build();

        Ok(ServiceBuilder::new()
            .layer(extensions_layer)
            .layer(ErrorHandlerLayer::new(app_state))
            // other middleware: rate limiting, logging, etc, etc
            // will be added here as well
            .service(dispatcher))
    }
}

impl Service<Request> for Dispatcher {
    type Response = http::Response<crate::types::body::Body>;
    type Error = ApiError;
    type Future = DispatcherFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    #[tracing::instrument(name = "dispatcher", skip_all)]
    fn call(&mut self, req: Request) -> Self::Future {
        // see: https://docs.rs/tower/latest/tower/trait.Service.html#be-careful-when-cloning-inner-services
        let this = self.clone();
        let this = std::mem::replace(self, this);
        tracing::trace!(provider = ?this.provider, "dispatcher received request");
        Box::pin(async move { this.dispatch(req).await })
    }
}

impl Dispatcher {
    #[allow(clippy::too_many_lines)]
    async fn dispatch(
        &self,
        mut req: Request,
    ) -> Result<http::Response<crate::types::body::Body>, ApiError> {
        // Extract request context and extensions
        let (
            mapper_ctx,
            req_ctx,
            api_endpoint,
            extracted_path_and_query,
            inference_provider,
            router_id,
            mapper_profile_context,
            start_instant,
            start_time,
            request_kind,
            prompt_ctx,
            prompt_header_from_mapper,
            large_context_decision,
            mut prompt_compression_tokens,
        ) = Self::extract_request_context(&mut req)?;

        let auth_ctx = req_ctx.auth_context.as_ref();
        let target_provider = &self.provider;

        let provider_for_allowlist =
            if auth_ctx.is_some_and(|a| a.is_custom_provider && a.master_key_base_url.is_some()) {
                InferenceProvider::Custom
            } else {
                target_provider.clone()
            };
        enforce_workspace_provider_allowlist(&self.app_state, auth_ctx, &provider_for_allowlist)?;

        let headers_for_llm_cache = req.headers().clone();
        let session_ctx = parse_session_headers(req.headers()).map_err(ApiError::InvalidRequest)?;

        {
            sanitize_upstream_headers(req.headers_mut());
        }
        let method = req.method().clone();
        let headers = req.headers().clone();
        let mut extensions_snapshot = req.extensions().clone();
        let log_emitted_marker = extensions_snapshot.get::<RequestLogEmitted>().cloned();
        let vk_policy = req.extensions().get::<VkPolicy>().cloned();
        let implicit_model_fallback_ctx = req
            .extensions()
            .get::<UnifiedImplicitModelFallbackContext>()
            .cloned();
        let target_url =
            self.build_target_url(&req_ctx, target_provider, extracted_path_and_query.as_str())?;
        // TODO: could change request type of dispatcher to
        // http::Request<reqwest::Body>
        // to avoid collecting the body twice
        let mut req_body_bytes = req
            .into_body()
            .collect()
            .await
            .map_err(|e| InternalError::RequestBodyError(Box::new(e)))?
            .to_bytes();

        let direct_proxy_prompt_log: Option<PromptHeaderForRequestLog> = if matches!(
            request_kind,
            RequestKind::DirectProxy | RequestKind::CustomProvider
        ) {
            let workspace_id = auth_ctx.map(|a| a.org_id.to_string()).unwrap_or_default();
            let (b, pl) =
                crate::content_filter::prompt_cache::merge_prompt_cache_messages_into_body(
                    self.app_state.redis(),
                    &headers,
                    &workspace_id,
                    req_body_bytes,
                    &self.app_state.0.metrics.vk,
                )
                .await?;
            req_body_bytes = b;
            let prompt_log = pl;

            let filter_result = match crate::content_filter::evaluate::evaluate_for_vk_request(
                &self.app_state,
                &headers,
                &extensions_snapshot,
                &req_body_bytes,
            )
            .await
            {
                Ok(r) => r,
                Err(ApiError::InvalidRequest(InvalidRequestError::ContentPolicyDenied {
                    ref message,
                })) => {
                    self.emit_policy_deny_request_log(
                        &req_ctx,
                        start_time,
                        start_instant,
                        &mapper_ctx,
                        router_id.clone(),
                        &headers,
                        &req_body_bytes,
                        message,
                        prompt_ctx.clone(),
                        prompt_log.clone(),
                        session_ctx.clone(),
                        target_provider,
                        extracted_path_and_query.as_str(),
                        log_emitted_marker.as_ref(),
                    );
                    return Err(ApiError::InvalidRequest(
                        InvalidRequestError::ContentPolicyDenied {
                            message: message.clone(),
                        },
                    ));
                }
                Err(e) => return Err(e),
            };
            if let crate::content_filter::ContentFilterForwardBody::UseReplaced(b) =
                filter_result.forward_body
            {
                req_body_bytes = b;
            }
            if let Some(ref new_model) = filter_result.change_model {
                let (new_body, original) = crate::content_filter::evaluate::apply_model_downgrade(
                    req_body_bytes,
                    new_model,
                );
                req_body_bytes = new_body;
                let original_model = original.unwrap_or_default();
                tracing::info!(
                    original_model = %original_model,
                    downgraded_model = %new_model,
                    "content_filter: policy model downgrade applied (direct proxy)"
                );
                extensions_snapshot.insert(crate::content_filter::PolicyModelOverride {
                    original_model,
                    downgraded_model: new_model.clone(),
                });
            }

            if extracted_path_and_query
                .path()
                .ends_with("chat/completions")
            {
                let mut fake_parts = http::Request::new(()).into_parts().0;
                fake_parts.headers = headers.clone();
                req_body_bytes = prompt_compression::apply_chat_completions(
                    &mut fake_parts,
                    req_body_bytes,
                    target_provider,
                )?;
                if let Some(pair) = fake_parts.extensions.remove::<PromptCompressionTokenPair>() {
                    prompt_compression_tokens = Some(pair);
                }
            }
            prompt_log
        } else {
            None
        };

        let prompt_for_request_log: Option<PromptHeaderForRequestLog> = if matches!(
            request_kind,
            RequestKind::DirectProxy | RequestKind::CustomProvider
        ) {
            direct_proxy_prompt_log
        } else {
            prompt_header_from_mapper
        };

        enforce_direct_proxy_vk_model_policy(
            &self.app_state,
            vk_policy.as_ref(),
            request_kind,
            target_provider,
            extracted_path_and_query.as_str(),
            &req_body_bytes,
            &extensions_snapshot,
            auth_ctx,
        )?;

        let llm_cache_settings =
            match alephant_llm_kv_cache::CacheSettings::parse(&headers_for_llm_cache) {
                Err(msg) => {
                    tracing::error!(%msg, "llm kv: invalid Alephant-Cache-* headers");
                    return Err(ApiError::Internal(InternalError::Internal));
                }
                Ok(s) => s,
            };

        let llm_kv_read_ok = llm_cache_settings.should_read
            && auth_ctx.is_some()
            && req_ctx.llm_kv_cache_read_allowed;

        let cache_read_keys = if llm_kv_read_ok || llm_cache_settings.should_write {
            Some(llm_kv_slot_keys(
                &llm_cache_settings,
                &target_url,
                &req_body_bytes,
            ))
        } else {
            None
        };

        if llm_kv_read_ok
            && let Some(ref keys) = cache_read_keys
            && let Some((entry, bidx)) =
                alephant_llm_kv_cache::read_bucket(self.app_state.llm_kv().as_ref(), keys).await
        {
            let (mut hit_resp, body_reader, tfft_rx) =
                build_llm_cache_hit_response(&entry, bidx, &mapper_ctx)?;
            let response_received_at = Utc::now();
            tracing::info!(
                method = %method,
                target_url = %target_url,
                is_stream = %mapper_ctx.is_stream,
                response_status = %hit_resp.status(),
                "llm kv cache hit"
            );
            let response_log_id = Uuid::new_v4();
            let provider_request_id = {
                let h = hit_resp.headers_mut();
                h.insert(
                    "alephant-id",
                    HeaderValue::from_str(&response_log_id.to_string())
                        .expect("a uuid is always a valid header value"),
                );
                h.remove(http::header::CONTENT_LENGTH);
                h.remove("x-request-id")
            };
            let extensions_copier = ExtensionsCopier::builder()
                .inference_provider(inference_provider.clone())
                .router_id(router_id.clone())
                .auth_context(auth_ctx.cloned())
                .provider_request_id(provider_request_id)
                .mapper_ctx(mapper_ctx.clone())
                .mapper_profile_context(mapper_profile_context.clone())
                .build();
            extensions_copier.copy_extensions(hit_resp.extensions_mut());
            hit_resp.extensions_mut().insert(mapper_ctx.clone());
            if let Some(ref ep) = api_endpoint {
                hit_resp.extensions_mut().insert(ep.clone());
            }
            hit_resp
                .extensions_mut()
                .insert(extracted_path_and_query.clone());
            let llm_kv_cache_key = keys.get(bidx).cloned();
            self.handle_logging(
                &req_ctx,
                start_time,
                start_instant,
                target_url.clone(),
                headers.clone(),
                req_body_bytes.clone(),
                &hit_resp,
                body_reader,
                tfft_rx,
                &mapper_ctx,
                router_id.clone(),
                request_log_id_from_headers(&headers),
                response_log_id,
                response_received_at,
                prompt_ctx.clone(),
                prompt_for_request_log.clone(),
                large_context_decision.clone(),
                prompt_compression_tokens,
                session_ctx.clone(),
                None,
                llm_kv_cache_key,
                true,
                target_provider,
                log_emitted_marker.as_ref(),
            );
            return Ok(hit_resp);
        }

        let semantic_prepared = if llm_kv_read_ok {
            if let Some(semantic_cache) = self.app_state.semantic_cache() {
                match semantic_cache.prepare_request(
                    extracted_path_and_query.as_str(),
                    &headers_for_llm_cache,
                    &req_body_bytes,
                ) {
                    Ok(prepared) => prepared,
                    Err(err) => {
                        tracing::warn!(%err, "semantic cache bypassed");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        let mut semantic_write_context = None;
        if llm_kv_read_ok
            && let Some(semantic_cache) = self.app_state.semantic_cache()
            && let Some(prepared) = semantic_prepared.as_ref()
        {
            match semantic_cache.try_hit_prepared(prepared).await {
                Ok(outcome) => {
                    semantic_write_context = outcome.write;
                    if let Some(hit) = outcome.hit {
                        let entry = alephant_llm_kv_cache::LlmCacheEntry {
                            headers: std::collections::HashMap::new(),
                            latency: 0,
                            body: vec![String::from_utf8_lossy(&hit.response_bytes).to_string()],
                        };
                        let (mut hit_resp, body_reader, tfft_rx) =
                            build_llm_cache_hit_response(&entry, 0, &mapper_ctx)?;
                        let response_received_at = Utc::now();
                        tracing::info!(
                            method = %method,
                            target_url = %target_url,
                            is_stream = %mapper_ctx.is_stream,
                            response_status = %hit_resp.status(),
                            cache_reference_id = %hit.cache_reference_id,
                            "semantic cache hit"
                        );
                        let response_log_id = Uuid::new_v4();
                        let provider_request_id = {
                            let h = hit_resp.headers_mut();
                            h.insert(
                                "alephant-id",
                                HeaderValue::from_str(&response_log_id.to_string())
                                    .expect("a uuid is always a valid header value"),
                            );
                            h.remove(http::header::CONTENT_LENGTH);
                            h.remove("x-request-id")
                        };
                        let extensions_copier = ExtensionsCopier::builder()
                            .inference_provider(inference_provider.clone())
                            .router_id(router_id.clone())
                            .auth_context(auth_ctx.cloned())
                            .provider_request_id(provider_request_id)
                            .mapper_ctx(mapper_ctx.clone())
                            .mapper_profile_context(mapper_profile_context.clone())
                            .build();
                        extensions_copier.copy_extensions(hit_resp.extensions_mut());
                        hit_resp.extensions_mut().insert(mapper_ctx.clone());
                        if let Some(ref ep) = api_endpoint {
                            hit_resp.extensions_mut().insert(ep.clone());
                        }
                        hit_resp
                            .extensions_mut()
                            .insert(extracted_path_and_query.clone());
                        self.handle_logging(
                            &req_ctx,
                            start_time,
                            start_instant,
                            target_url.clone(),
                            headers.clone(),
                            req_body_bytes.clone(),
                            &hit_resp,
                            body_reader,
                            tfft_rx,
                            &mapper_ctx,
                            router_id.clone(),
                            request_log_id_from_headers(&headers),
                            response_log_id,
                            response_received_at,
                            prompt_ctx.clone(),
                            prompt_for_request_log.clone(),
                            large_context_decision.clone(),
                            prompt_compression_tokens,
                            session_ctx.clone(),
                            None,
                            Some(hit.cache_reference_id),
                            true,
                            target_provider,
                            log_emitted_marker.as_ref(),
                        );
                        return Ok(hit_resp);
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, "semantic cache bypassed");
                }
            }
        }

        let llm_kv_write_enabled = llm_cache_settings.should_write
            && auth_ctx.is_some()
            && req_ctx.llm_kv_cache_write_allowed;
        let semantic_write_enabled = self.app_state.semantic_cache().is_some();
        let (cache_tap_tx, cache_save_rx) = if llm_kv_write_enabled || semantic_write_enabled {
            let (tx, rx) = mpsc::unbounded_channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        let request_builder = self
            .client
            .as_ref()
            .request(method.clone(), target_url.clone())
            .headers(headers.clone());

        let request_builder = self
            .client
            .authenticate(
                &self.app_state,
                request_builder,
                &req_body_bytes,
                auth_ctx,
                self.provider.clone(),
            )
            .await?;

        let request_log_id = request_log_id_from_headers(&headers);

        tracing::info!(
            target_url = %target_url,
            body_len = req_body_bytes.len(),
            "dispatcher forward"
        );

        let metrics_for_stream = self.app_state.0.endpoint_metrics.clone();
        if let Some(ref api_endpoint) = api_endpoint {
            let endpoint_metrics = self
                .app_state
                .0
                .endpoint_metrics
                .health_metrics(api_endpoint.clone())?;
            endpoint_metrics.incr_req_count();
        }

        let (
            mut client_response,
            response_body_for_logger,
            tfft_rx,
            effective_provider,
            effective_target_url,
            effective_request_body,
        ) = if mapper_ctx.is_stream {
            let (response, body_reader, tfft_rx) = dispatch_stream_with_retry(
                &self.app_state,
                request_builder,
                req_body_bytes.clone(),
                api_endpoint.clone(),
                metrics_for_stream,
                &req_ctx,
                request_kind,
                cache_tap_tx.clone(),
            )
            .await?;
            (
                response,
                body_reader,
                tfft_rx,
                self.provider.clone(),
                target_url.clone(),
                req_body_bytes.clone(),
            )
        } else {
            let outcome = self
                .dispatch_sync_with_retry(
                    request_builder,
                    req_body_bytes.clone(),
                    &req_ctx,
                    request_kind,
                    cache_tap_tx,
                    &method,
                    &headers,
                    target_url.clone(),
                    extracted_path_and_query.as_str(),
                    vk_policy.as_ref(),
                    implicit_model_fallback_ctx.as_ref(),
                )
                .instrument(info_span!("dispatch_sync"))
                .await?;
            (
                outcome.response,
                outcome.response_body_for_logger,
                outcome.tfft_rx,
                outcome.effective_provider,
                outcome.effective_target_url,
                outcome.effective_request_body,
            )
        };
        if llm_kv_read_ok {
            let h = client_response.headers_mut();
            let _ = h.insert(
                HeaderName::from_static("alephant-cache"),
                HeaderValue::from_static("MISS"),
            );
        }

        let cache_write_keys = if cache_save_rx.is_some()
            && llm_cache_settings.should_write
            && auth_ctx.is_some()
            && req_ctx.llm_kv_cache_write_allowed
        {
            Some(llm_kv_write_slot_keys(
                &llm_cache_settings,
                &effective_target_url,
                &effective_request_body,
            ))
        } else {
            None
        };

        if let Some(mut rx) = cache_save_rx {
            let backend = self.app_state.llm_kv().clone();
            let ttl = llm_cache_settings.expiration_ttl_secs();
            let start = start_instant;
            let status = client_response.status();
            let resp_hdrs = client_response.headers().clone();
            let llm_kv_keys = cache_write_keys;
            let semantic_cache = self.app_state.semantic_cache().cloned();
            let semantic_prepared_for_write = semantic_prepared.clone();
            let semantic_write_context_for_write = semantic_write_context.clone();
            let semantic_path = extracted_path_and_query.to_string();
            let semantic_headers = headers_for_llm_cache.clone();
            let semantic_body = semantic_write_body_bytes(&req_body_bytes, &effective_request_body);
            tokio::spawn(async move {
                if !status.is_success() {
                    return;
                }
                let mut body_chunks = Vec::new();
                let mut body_bytes = Vec::new();
                while let Some(b) = rx.recv().await {
                    body_bytes.extend_from_slice(&b);
                    body_chunks.push(String::from_utf8_lossy(&b).into_owned());
                }
                if llm_kv_write_enabled {
                    if let Some(keys) = llm_kv_keys {
                        let mut headers_json = HashMap::new();
                        for (name, val) in &resp_hdrs {
                            if let Ok(vs) = val.to_str() {
                                headers_json.insert(name.to_string(), vs.to_string());
                            }
                        }
                        let entry = alephant_llm_kv_cache::LlmCacheEntry {
                            headers: headers_json,
                            latency: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                            body: body_chunks,
                        };
                        let _ = alephant_llm_kv_cache::try_save_to_first_free_slot(
                            backend.as_ref(),
                            &keys,
                            &entry,
                            ttl,
                        )
                        .await;
                    }
                }
                if let Some(svc) = semantic_cache {
                    let store_result = if let Some(write) = semantic_write_context_for_write {
                        svc.store_response_with_context(write, &body_bytes).await
                    } else if let Some(prepared) = semantic_prepared_for_write.as_ref() {
                        svc.store_response_prepared(prepared, &body_bytes).await
                    } else {
                        svc.store_response(
                            &semantic_path,
                            &semantic_headers,
                            &semantic_body,
                            &body_bytes,
                        )
                        .await
                    };
                    if let Err(err) = store_result {
                        tracing::warn!(%err, "semantic cache store bypassed");
                    }
                }
            });
        }

        let response_received_at = Utc::now();
        tracing::info!(
            method = %method,
            target_url = %effective_target_url,
            is_stream = %mapper_ctx.is_stream,
            response_status = %client_response.status(),
            "proxied request"
        );
        let response_log_id = Uuid::new_v4();
        let provider_request_id = {
            let headers = client_response.headers_mut();
            headers.insert(
                "alephant-id",
                HeaderValue::from_str(&response_log_id.to_string())
                    .expect("a uuid is always a valid header value"),
            );
            headers.remove(http::header::CONTENT_LENGTH);
            headers.remove("x-request-id")
        };
        tracing::debug!(provider_req_id = ?provider_request_id, status = %client_response.status(), "received response");
        let extensions_copier = ExtensionsCopier::builder()
            .inference_provider(effective_provider.clone())
            .router_id(router_id.clone())
            .auth_context(auth_ctx.cloned())
            .provider_request_id(provider_request_id)
            .mapper_ctx(mapper_ctx.clone())
            .mapper_profile_context(mapper_profile_context)
            .build();
        extensions_copier.copy_extensions(client_response.extensions_mut());
        client_response.extensions_mut().insert(mapper_ctx.clone());
        if let Some(api_endpoint) = api_endpoint.clone() {
            client_response.extensions_mut().insert(api_endpoint);
        }
        client_response
            .extensions_mut()
            .insert(extracted_path_and_query);

        let response_status = client_response.status();
        let response_headers = client_response.headers();
        self.handle_error_and_rate_limiting(
            response_status,
            response_headers,
            api_endpoint.clone(),
            &effective_provider,
        )
        .await?;

        // Handle logging
        self.handle_logging(
            &req_ctx,
            start_time,
            start_instant,
            effective_target_url,
            headers,
            effective_request_body,
            &client_response,
            response_body_for_logger,
            tfft_rx,
            &mapper_ctx,
            router_id,
            request_log_id,
            response_log_id,
            response_received_at,
            prompt_ctx,
            prompt_for_request_log,
            large_context_decision,
            prompt_compression_tokens,
            session_ctx,
            None,
            None,
            llm_kv_read_ok,
            &effective_provider,
            log_emitted_marker.as_ref(),
        );

        Ok(client_response)
    }

    // ... existing methods ...

    /// Extracts request context and extensions from the request
    #[allow(clippy::type_complexity)]
    fn extract_request_context(
        req: &mut Request,
    ) -> Result<
        (
            MapperContext,
            Arc<RequestContext>,
            Option<ApiEndpoint>,
            PathAndQuery,
            InferenceProvider,
            Option<RouterId>,
            Option<MapperProfileContext>,
            Instant,
            DateTime<Utc>,
            RequestKind,
            Option<PromptContext>,
            Option<PromptHeaderForRequestLog>,
            Option<LargeContextDecision>,
            Option<PromptCompressionTokenPair>,
        ),
        ApiError,
    > {
        let mapper_ctx = req
            .extensions_mut()
            .remove::<MapperContext>()
            .ok_or(InternalError::ExtensionNotFound("MapperContext"))?;
        let req_ctx = req
            .extensions_mut()
            .remove::<Arc<RequestContext>>()
            .ok_or(InternalError::ExtensionNotFound("RequestContext"))?;
        let api_endpoint = req.extensions().get::<ApiEndpoint>().cloned();
        let extracted_path_and_query =
            req.extensions_mut()
                .remove::<PathAndQuery>()
                .ok_or(ApiError::Internal(InternalError::ExtensionNotFound(
                    "PathAndQuery",
                )))?;
        let inference_provider = req
            .extensions()
            .get::<InferenceProvider>()
            .cloned()
            .ok_or(InternalError::ExtensionNotFound("InferenceProvider"))?;
        let router_id = req.extensions().get::<RouterId>().cloned();
        let mapper_profile_context = req.extensions_mut().remove::<MapperProfileContext>();
        let start_instant = req
            .extensions()
            .get::<Instant>()
            .copied()
            .unwrap_or_else(|| {
                tracing::warn!("did not find expected Instant in req extensions");
                Instant::now()
            });
        let start_time = req
            .extensions()
            .get::<DateTime<Utc>>()
            .copied()
            .unwrap_or_else(|| {
                tracing::warn!("did not find expected DateTime<Utc> in req extensions");
                Utc::now()
            });
        let request_kind = req
            .extensions()
            .get::<RequestKind>()
            .copied()
            .ok_or(InternalError::ExtensionNotFound("RequestKind"))?;
        let prompt_ctx = req.extensions_mut().remove::<PromptContext>();
        let prompt_header_from_mapper = req.extensions_mut().remove::<PromptHeaderForRequestLog>();
        let large_context_decision = req.extensions_mut().remove::<LargeContextDecision>();
        let prompt_compression_tokens = req.extensions_mut().remove::<PromptCompressionTokenPair>();

        Ok((
            mapper_ctx,
            req_ctx,
            api_endpoint,
            extracted_path_and_query,
            inference_provider,
            router_id,
            mapper_profile_context,
            start_instant,
            start_time,
            request_kind,
            prompt_ctx,
            prompt_header_from_mapper,
            large_context_decision,
            prompt_compression_tokens,
        ))
    }

    /// Handles error responses and rate limiting
    async fn handle_error_and_rate_limiting(
        &self,
        response_status: StatusCode,
        response_headers: &HeaderMap,
        api_endpoint: Option<ApiEndpoint>,
        effective_provider: &InferenceProvider,
    ) -> Result<(), ApiError> {
        if response_status.is_server_error() {
            if let Some(api_endpoint) = api_endpoint {
                let endpoint_metrics = self
                    .app_state
                    .0
                    .endpoint_metrics
                    .health_metrics(api_endpoint)?;
                endpoint_metrics.incr_remote_internal_error_count();
            }
        } else if response_status == StatusCode::TOO_MANY_REQUESTS
            && let Some(ref api_endpoint) = api_endpoint
        {
            let retry_after = extract_retry_after(response_headers);
            tracing::info!(
                provider = ?effective_provider,
                api_endpoint = ?api_endpoint,
                retry_after = ?retry_after,
                "Provider rate limited, signaling monitor"
            );

            if let Some(rate_limit_tx) = &self.rate_limit_tx
                && let Err(e) = rate_limit_tx
                    .send(RateLimitEvent::new(api_endpoint.clone(), retry_after))
                    .await
            {
                tracing::error!(error = %e, "failed to send rate limit event");
            }
        }
        Ok(())
    }

    /// Handles logging logic for both observability and metrics
    #[allow(clippy::too_many_arguments)]
    fn handle_logging(
        &self,
        req_ctx: &RequestContext,
        start_time: DateTime<Utc>,
        start_instant: Instant,
        target_url: url::Url,
        headers: HeaderMap,
        req_body_bytes: Bytes,
        client_response: &http::Response<crate::types::body::Body>,
        response_body_for_logger: BodyReader,
        tfft_rx: oneshot::Receiver<()>,
        mapper_ctx: &MapperContext,
        router_id: Option<RouterId>,
        request_log_id: Uuid,
        response_log_id: Uuid,
        response_received_at: DateTime<Utc>,
        prompt_ctx: Option<PromptContext>,
        prompt_header_for_request_log: Option<PromptHeaderForRequestLog>,
        large_context_decision: Option<LargeContextDecision>,
        prompt_compression_tokens: Option<PromptCompressionTokenPair>,
        session_ctx: Option<SessionHeaders>,
        ai_gateway_body_mapping: Option<String>,
        // LLM KV hit (e.g. Cloudflare KV): KV key written to request log
        // `cache_reference_id`.
        cache_reference_id: Option<String>,
        // Whether this request attempted LLM KV read per config (auth + route
        // switches).
        llm_kv_cache_read_enabled: bool,
        effective_provider: &InferenceProvider,
        log_emitted: Option<&RequestLogEmitted>,
    ) {
        let deployment_target = self.app_state.config().deployment_target.clone();
        if self.app_state.config().alephant.is_observability_enabled() {
            if let Some(auth_ctx) = req_ctx.auth_context.clone() {
                let cache_enabled_for_log = if llm_kv_cache_read_enabled {
                    Some(cache_reference_id.is_some())
                } else {
                    None
                };
                let response_logger = LoggerService::builder()
                    .app_state(self.app_state.clone())
                    .auth_ctx(auth_ctx)
                    .start_time(start_time)
                    .start_instant(start_instant)
                    .target_url(target_url)
                    .request_headers(headers)
                    .request_body(req_body_bytes)
                    .response_status(client_response.status())
                    .response_body(response_body_for_logger)
                    .provider(effective_provider.clone())
                    .tfft_rx(tfft_rx)
                    .mapper_ctx(mapper_ctx.clone())
                    .router_id(router_id)
                    .deployment_target(deployment_target)
                    .request_id(request_log_id)
                    .response_id(response_log_id)
                    .response_created_at(response_received_at)
                    .prompt_ctx(prompt_ctx)
                    .prompt_header_for_request_log(prompt_header_for_request_log)
                    .large_context_decision(large_context_decision)
                    .prompt_compression_tokens(prompt_compression_tokens)
                    .session_ctx(session_ctx)
                    .ai_gateway_body_mapping(ai_gateway_body_mapping)
                    .cache_enabled(cache_enabled_for_log)
                    .cache_reference_id(cache_reference_id)
                    .build();

                if let Some(marker) = log_emitted {
                    marker.mark();
                }

                let app_state = self.app_state.clone();
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
        } else {
            if let Some(marker) = log_emitted {
                marker.mark();
            }
            let app_state = self.app_state.clone();
            let model = mapper_ctx
                .model
                .as_ref()
                .map_or_else(|| "unknown".to_string(), std::string::ToString::to_string);
            let forward_url = target_url.to_string();
            let path = target_url.path().to_string();
            let provider_string = effective_provider.to_string();
            tokio::spawn(
                async move {
                    let tfft_future = TFFTFuture::new(start_instant, tfft_rx);
                    let collect_future = response_body_for_logger.collect();
                    let (collected, tfft_duration) = tokio::join!(collect_future, tfft_future);
                    let response_body = collected.expect("infallible never errors").to_bytes();
                    tracing::info!("dispatcher response target_url: {forward_url}");
                    tracing::info!(
                        "dispatcher response body ({} bytes): {}",
                        response_body.len(),
                        String::from_utf8_lossy(&response_body)
                    );
                    if let Ok(tfft_duration) = tfft_duration {
                        tracing::trace!(tfft_duration = ?tfft_duration, "tfft_duration");
                        let attributes = [
                            KeyValue::new("provider", provider_string),
                            KeyValue::new("model", model),
                            KeyValue::new("path", path),
                        ];
                        #[allow(clippy::cast_precision_loss)]
                        app_state
                            .0
                            .metrics
                            .tfft_duration
                            .record(tfft_duration.as_millis() as f64, &attributes);
                    } else {
                        tracing::error!("Failed to get TFFT signal")
                    }
                }
                .instrument(tracing::Span::current()),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_policy_deny_request_log(
        &self,
        req_ctx: &RequestContext,
        start_time: DateTime<Utc>,
        start_instant: Instant,
        mapper_ctx: &MapperContext,
        router_id: Option<RouterId>,
        headers: &HeaderMap,
        req_body_bytes: &Bytes,
        deny_message: &str,
        prompt_ctx: Option<PromptContext>,
        prompt_header_for_request_log: Option<PromptHeaderForRequestLog>,
        session_ctx: Option<SessionHeaders>,
        target_provider: &InferenceProvider,
        extracted_path_and_query: &str,
        log_emitted: Option<&RequestLogEmitted>,
    ) {
        let deployment_target = self.app_state.config().deployment_target.clone();
        if !self.app_state.config().alephant.is_observability_enabled() {
            return;
        }
        let Some(auth_ctx) = req_ctx.auth_context.clone() else {
            return;
        };

        let target_url =
            match self.build_target_url(req_ctx, target_provider, extracted_path_and_query) {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "policy deny log: failed to build target_url, skipping"
                    );
                    return;
                }
            };

        let response_body_bytes =
            crate::content_filter::evaluate::policy_denied_error_response_json(deny_message);

        let response_status = http::StatusCode::OK;
        let request_log_id = request_log_id_from_headers(headers);
        let response_log_id = Uuid::new_v4();
        let response_received_at = Utc::now();

        let (tx, rx) = mpsc::unbounded_channel();
        let _ = tx.send(Bytes::from(response_body_bytes));
        drop(tx);

        let (tfft_tx_for_body, _unused_rx) = oneshot::channel();
        let body_reader = BodyReader::new(
            rx,
            tfft_tx_for_body,
            hyper::body::SizeHint::default(),
            false,
            TfftTrigger::Never,
        );
        let (tfft_tx_for_log, tfft_rx) = oneshot::channel();
        let _ = tfft_tx_for_log.send(());

        let response_logger = LoggerService::builder()
            .app_state(self.app_state.clone())
            .auth_ctx(auth_ctx)
            .start_time(start_time)
            .start_instant(start_instant)
            .target_url(target_url)
            .request_headers(headers.clone())
            .request_body(req_body_bytes.clone())
            .response_status(response_status)
            .response_body(body_reader)
            .provider(target_provider.clone())
            .tfft_rx(tfft_rx)
            .mapper_ctx(mapper_ctx.clone())
            .router_id(router_id)
            .deployment_target(deployment_target)
            .request_id(request_log_id)
            .response_id(response_log_id)
            .response_created_at(response_received_at)
            .prompt_ctx(prompt_ctx)
            .prompt_header_for_request_log(prompt_header_for_request_log)
            .session_ctx(session_ctx)
            .build();

        if let Some(marker) = log_emitted {
            marker.mark();
        }

        let app_state = self.app_state.clone();
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

    fn build_target_url(
        &self,
        req_ctx: &RequestContext,
        target_provider: &InferenceProvider,
        extracted_path_and_query: &str,
    ) -> Result<url::Url, ApiError> {
        // Priority 1: master_keys.base_url (Cloud, per-key override).
        if let Some(master_url) =
            resolve_master_key_target_url(req_ctx.auth_context.as_ref(), extracted_path_and_query)
        {
            return Ok(master_url);
        }

        // Priority 2: router-level provider config.
        if let Some(router_config) = req_ctx.router_config.as_ref()
            && let Some(router_provider_config) = router_config.providers.as_ref()
            && let Some(provider_config) = router_provider_config.get(target_provider)
        {
            return Ok(join_provider_upstream_url(
                &provider_config.base_url,
                extracted_path_and_query,
            ));
        }

        // Priority 3: global provider config (hot-updatable in Cloud mode).
        let providers_config = self.app_state.get_providers_config();
        let selected_provider_config = providers_config
            .get(target_provider)
            .ok_or_else(|| InternalError::ProviderNotConfigured(target_provider.clone()))?;
        Ok(join_provider_upstream_url(
            &selected_provider_config.base_url,
            extracted_path_and_query,
        ))
    }

    /// We take a `&RequestBuilder` so that `dispatch_stream` implements `FnMut`
    /// so we can use the [`backon`] crate for retries.
    async fn dispatch_stream(
        request_builder: &RequestBuilder,
        req_body_bytes: Bytes,
        api_endpoint: Option<ApiEndpoint>,
        metrics_registry: EndpointMetricsRegistry,
        cache_tap: Option<mpsc::UnboundedSender<Bytes>>,
    ) -> Result<
        (
            http::Response<crate::types::body::Body>,
            crate::types::body::BodyReader,
            oneshot::Receiver<()>,
        ),
        ApiError,
    > {
        let request_builder = request_builder.try_clone().ok_or_else(|| {
            // in theory, this should never happen, as we'll have already
            // collected the request body
            tracing::error!("failed to clone request builder, cannot dispatch stream");
            ApiError::Internal(InternalError::Internal)
        })?;
        let response_stream = Client::sse_stream(
            request_builder,
            req_body_bytes,
            api_endpoint,
            &metrics_registry,
        )
        .await?;
        let mut resp_builder = http::Response::builder();
        *resp_builder.headers_mut().unwrap() = stream_response_headers();
        resp_builder = resp_builder.status(StatusCode::OK);

        let (user_resp_body, body_reader, tfft_rx) = BodyReader::wrap_stream(
            response_stream,
            true,
            TfftTrigger::FirstModelToken,
            cache_tap,
        );

        let response = resp_builder
            .body(user_resp_body)
            .map_err(InternalError::HttpError)?;
        Ok((response, body_reader, tfft_rx))
    }

    async fn dispatch_sync(
        request_builder: &RequestBuilder,
        req_body_bytes: Bytes,
        cache_tap: Option<mpsc::UnboundedSender<Bytes>>,
    ) -> Result<
        (
            http::Response<crate::types::body::Body>,
            crate::types::body::BodyReader,
            oneshot::Receiver<()>,
        ),
        ApiError,
    > {
        let request_builder = request_builder.try_clone().ok_or_else(|| {
            // in theory, this should never happen, as we'll have already
            // collected the request body
            tracing::error!("failed to clone request builder, cannot dispatch stream");
            ApiError::Internal(InternalError::Internal)
        })?;
        let response: reqwest::Response = request_builder
            .body(req_body_bytes)
            .send()
            .await
            .map_err(InternalError::ReqwestError)?;

        let status = response.status();
        let mut resp_builder = http::Response::builder().status(status);
        *resp_builder.headers_mut().unwrap() = response.headers().clone();

        // this is compiled out in release builds
        #[cfg(debug_assertions)]
        if status.is_server_error() || status.is_client_error() {
            let body = response.text().await.map_err(InternalError::ReqwestError)?;
            tracing::debug!(status_code = %status, error_resp = %body, "received error response");
            let bytes = bytes::Bytes::from(body);
            let stream = futures::stream::once(futures::future::ok::<_, ApiError>(bytes));
            let (error_body, error_reader, tfft_rx) =
                BodyReader::wrap_stream(stream, false, TfftTrigger::Never, cache_tap.clone());
            let response = resp_builder
                .body(error_body)
                .map_err(InternalError::HttpError)?;

            return Ok((response, error_reader, tfft_rx));
        }

        let (user_resp_body, body_reader, tfft_rx) = BodyReader::wrap_stream(
            response
                .bytes_stream()
                .map_err(|e| InternalError::ReqwestError(e).into()),
            false,
            TfftTrigger::Never,
            cache_tap,
        );
        let response = resp_builder
            .body(user_resp_body)
            .map_err(InternalError::HttpError)?;
        Ok((response, body_reader, tfft_rx))
    }

    #[allow(clippy::too_many_lines)]
    async fn dispatch_sync_with_retry(
        &self,
        request_builder: RequestBuilder,
        req_body_bytes: Bytes,
        req_ctx: &RequestContext,
        request_kind: RequestKind,
        cache_tap: Option<mpsc::UnboundedSender<Bytes>>,
        method: &http::Method,
        headers: &HeaderMap,
        target_url: url::Url,
        extracted_path_and_query: &str,
        vk_policy: Option<&VkPolicy>,
        implicit_model_fallback_ctx: Option<&UnifiedImplicitModelFallbackContext>,
    ) -> Result<SyncDispatchOutcome, ApiError> {
        let retry_config = get_retry_config(&self.app_state, request_kind, req_ctx);
        let fallback_policy_for_log = self.app_state.config().fallback_policy.clone();
        let provider_for_log = self.provider.clone();
        let fallback_cache_tap = cache_tap.clone();
        let retry_exhausted_before_fallback = retry_config
            .as_ref()
            .is_some_and(|config| retry_config_allows_retry_attempts(config.as_ref()));
        let result: Result<SyncDispatchResponse, ApiError> = if let Some(retry_config) =
            retry_config
        {
            match retry_config.as_ref() {
                RetryConfig::Exponential {
                    min_delay,
                    max_delay,
                    max_retries,
                    factor,
                } => {
                    let retry_strategy = ExponentialBuilder::default()
                        .with_max_delay(*max_delay)
                        .with_min_delay(*min_delay)
                        .with_max_times(usize::from(*max_retries))
                        .with_factor(
                            factor
                                .to_f32()
                                .unwrap_or(crate::config::retry::DEFAULT_RETRY_FACTOR),
                        )
                        .with_jitter()
                        .build();
                    let future_fn = || async {
                        let result = Self::dispatch_sync(
                            &request_builder,
                            req_body_bytes.clone(),
                            cache_tap.clone(),
                        )
                        .await?;

                        Ok(result)
                    };

                    crate::utils::retry::RetryWithResult::new(future_fn, retry_strategy)
                        .when(|result: &Result<_, _>| match result {
                            Ok(response) => response.0.status().is_server_error(),
                            Err(e) => match e {
                                ApiError::Internal(InternalError::ReqwestError(reqwest_error)) => {
                                    reqwest_error.is_connect()
                                        || reqwest_error
                                            .status()
                                            .is_some_and(|s| s.is_server_error())
                                }
                                _ => false,
                            },
                        })
                        .notify(|result: &Result<_, _>, dur: Duration| match result {
                            Ok(result) if result.0.status().is_server_error() => {
                                tracing::warn!(
                                    error = %result.0.status(),
                                    retry_in = ?dur,
                                    "got error dispatching sync request, retrying...",
                                );
                                crate::fallback::observability::log_decision(
                                    &fallback_policy_for_log,
                                    crate::fallback::observability::DecisionKind::Retry,
                                    None,
                                    &provider_for_log,
                                );
                            }
                            Err(ApiError::Internal(InternalError::ReqwestError(reqwest_error)))
                                if reqwest_error.is_connect()
                                    || reqwest_error
                                        .status()
                                        .is_some_and(|s| s.is_server_error()) =>
                            {
                                tracing::warn!(
                                    error = %reqwest_error,
                                    retry_in = ?dur,
                                    "got error dispatching sync request, retrying...",
                                );
                                crate::fallback::observability::log_decision(
                                    &fallback_policy_for_log,
                                    crate::fallback::observability::DecisionKind::Retry,
                                    None,
                                    &provider_for_log,
                                );
                            }
                            _ => {}
                        })
                        .await
                }
                RetryConfig::Constant { delay, max_retries } => {
                    let retry_strategy = ConstantBuilder::default()
                        .with_delay(*delay)
                        .with_max_times(usize::from(*max_retries))
                        .with_jitter()
                        .build();
                    let future_fn = || async {
                        Self::dispatch_sync(
                            &request_builder,
                            req_body_bytes.clone(),
                            cache_tap.clone(),
                        )
                        .await
                    };

                    crate::utils::retry::RetryWithResult::new(future_fn, retry_strategy)
                        .when(|result: &Result<_, _>| match result {
                            Ok(response) => response.0.status().is_server_error(),
                            Err(e) => match e {
                                ApiError::Internal(InternalError::ReqwestError(reqwest_error)) => {
                                    reqwest_error.is_connect()
                                        || reqwest_error
                                            .status()
                                            .is_some_and(|s| s.is_server_error())
                                }
                                _ => false,
                            },
                        })
                        .notify(|result: &Result<_, _>, dur: Duration| match result {
                            Ok(result) if result.0.status().is_server_error() => {
                                tracing::warn!(
                                    error = %result.0.status(),
                                    retry_in = ?dur,
                                    "got error dispatching sync request, retrying...",
                                );
                                crate::fallback::observability::log_decision(
                                    &fallback_policy_for_log,
                                    crate::fallback::observability::DecisionKind::Retry,
                                    None,
                                    &provider_for_log,
                                );
                            }
                            Err(ApiError::Internal(InternalError::ReqwestError(reqwest_error)))
                                if reqwest_error.is_connect()
                                    || reqwest_error
                                        .status()
                                        .is_some_and(|s| s.is_server_error()) =>
                            {
                                tracing::warn!(
                                    error = %reqwest_error,
                                    retry_in = ?dur,
                                    "got error dispatching sync request, retrying...",
                                );
                                crate::fallback::observability::log_decision(
                                    &fallback_policy_for_log,
                                    crate::fallback::observability::DecisionKind::Retry,
                                    None,
                                    &provider_for_log,
                                );
                            }
                            _ => {}
                        })
                        .await
                }
            }
        } else {
            Self::dispatch_sync(&request_builder, req_body_bytes.clone(), cache_tap).await
        };

        if should_attempt_cross_provider_default_model_fallback(
            retry_exhausted_before_fallback,
            request_kind,
            extracted_path_and_query,
            implicit_model_fallback_ctx,
            &result,
        ) {
            match self
                .try_cross_provider_default_model_fallback(
                    req_ctx,
                    method,
                    headers,
                    extracted_path_and_query,
                    vk_policy,
                    implicit_model_fallback_ctx,
                    &req_body_bytes,
                    fallback_cache_tap,
                )
                .await
            {
                Ok(Some(fallback_result)) => {
                    let (
                        response,
                        response_body_for_logger,
                        tfft_rx,
                        effective_provider,
                        effective_target_url,
                        effective_request_body,
                    ) = fallback_result;
                    return Ok(SyncDispatchOutcome {
                        response,
                        response_body_for_logger,
                        tfft_rx,
                        effective_provider,
                        effective_target_url,
                        effective_request_body,
                    });
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        path = %extracted_path_and_query,
                        provider = %self.provider,
                        "cross-provider fallback failed; returning original result"
                    );
                }
            }
        }

        let (response, response_body_for_logger, tfft_rx) = result?;
        Ok(SyncDispatchOutcome {
            response,
            response_body_for_logger,
            tfft_rx,
            effective_provider: self.provider.clone(),
            effective_target_url: target_url,
            effective_request_body: req_body_bytes,
        })
    }
}

impl Dispatcher {
    async fn try_cross_provider_default_model_fallback(
        &self,
        req_ctx: &RequestContext,
        method: &http::Method,
        headers: &HeaderMap,
        extracted_path_and_query: &str,
        vk_policy: Option<&VkPolicy>,
        implicit_model_fallback_ctx: Option<&UnifiedImplicitModelFallbackContext>,
        req_body_bytes: &Bytes,
        cache_tap: Option<mpsc::UnboundedSender<Bytes>>,
    ) -> Result<
        Option<(
            http::Response<crate::types::body::Body>,
            crate::types::body::BodyReader,
            oneshot::Receiver<()>,
            InferenceProvider,
            url::Url,
            Bytes,
        )>,
        ApiError,
    > {
        let Some(auth_ctx) = req_ctx.auth_context.as_ref() else {
            return Ok(None);
        };
        let Some(vk_policy) = vk_policy else {
            return Ok(None);
        };
        let Some(implicit_ctx) = implicit_model_fallback_ctx else {
            return Ok(None);
        };
        let Ok(parsed_current) = split_provider_model(&implicit_ctx.selected_model) else {
            return Ok(None);
        };
        let fallback_model = match choose_default_gateway_model_excluding_provider(
            &self.app_state,
            "chat/completions",
            auth_ctx,
            vk_policy,
            Some(parsed_current.provider_raw),
        )
        .await
        {
            Ok(model) => model,
            Err(ApiError::InvalidRequest(InvalidRequestError::NoModelAvailable)) => {
                return Ok(None);
            }
            Err(err) => return Err(err),
        };
        if fallback_model.eq_ignore_ascii_case(&implicit_ctx.selected_model) {
            return Ok(None);
        }
        enforce_workspace_provider_allowlist(
            &self.app_state,
            req_ctx.auth_context.as_ref(),
            &inference_provider_from_gateway_model(&fallback_model)?,
        )?;
        let (fallback_provider, fallback_target_url, fallback_body) = self
            .cross_provider_fallback_request_details(
                req_ctx,
                extracted_path_and_query,
                req_body_bytes,
                &fallback_model,
            )?;
        let fallback_client = Client::new(&self.app_state, fallback_provider.clone())
            .await
            .map_err(|err| {
                tracing::error!(
                    error = %err,
                    provider = %fallback_provider,
                    "failed to build fallback client"
                );
                ApiError::Internal(InternalError::Internal)
            })?;
        let fallback_request_builder = fallback_client
            .as_ref()
            .request(method.clone(), fallback_target_url.clone())
            .headers(headers.clone());
        let fallback_request_builder = fallback_client
            .authenticate(
                &self.app_state,
                fallback_request_builder,
                &fallback_body,
                req_ctx.auth_context.as_ref(),
                fallback_provider.clone(),
            )
            .await?;
        crate::fallback::observability::log_decision(
            &self.app_state.config().fallback_policy,
            crate::fallback::observability::DecisionKind::CrossProviderFallback,
            None,
            &fallback_provider,
        );
        let (response, response_body_for_logger, tfft_rx) =
            Self::dispatch_sync(&fallback_request_builder, fallback_body.clone(), cache_tap)
                .await?;
        Ok(Some((
            response,
            response_body_for_logger,
            tfft_rx,
            fallback_provider,
            fallback_target_url,
            fallback_body,
        )))
    }

    fn cross_provider_fallback_request_details(
        &self,
        req_ctx: &RequestContext,
        extracted_path_and_query: &str,
        req_body_bytes: &Bytes,
        fallback_model: &str,
    ) -> Result<(InferenceProvider, url::Url, Bytes), ApiError> {
        let fallback_provider = inference_provider_from_gateway_model(fallback_model)?;
        let fallback_target_url =
            self.build_target_url(req_ctx, &fallback_provider, extracted_path_and_query)?;
        let fallback_body = rewrite_chat_completion_model(req_body_bytes, fallback_model)?;
        Ok((fallback_provider, fallback_target_url, fallback_body))
    }
}

fn sync_dispatch_result_is_retryable(result: &Result<SyncDispatchResponse, ApiError>) -> bool {
    match result {
        Ok(response) => response.0.status().is_server_error(),
        Err(ApiError::Internal(InternalError::ReqwestError(reqwest_error))) => {
            reqwest_error.is_connect()
                || reqwest_error
                    .status()
                    .is_some_and(|status| status.is_server_error())
        }
        _ => false,
    }
}

fn should_attempt_cross_provider_default_model_fallback(
    retry_exhausted_before_fallback: bool,
    request_kind: RequestKind,
    extracted_path_and_query: &str,
    implicit_model_fallback_ctx: Option<&UnifiedImplicitModelFallbackContext>,
    result: &Result<SyncDispatchResponse, ApiError>,
) -> bool {
    retry_exhausted_before_fallback
        && matches!(request_kind, RequestKind::UnifiedApi)
        && unified_chat_completions_path(extracted_path_and_query)
        && implicit_model_fallback_ctx.is_some()
        && sync_dispatch_result_is_retryable(result)
}

fn retry_config_allows_retry_attempts(retry_config: &RetryConfig) -> bool {
    match retry_config {
        RetryConfig::Exponential { max_retries, .. }
        | RetryConfig::Constant { max_retries, .. } => *max_retries > 0,
    }
}

fn unified_chat_completions_path(extracted_path_and_query: &str) -> bool {
    extracted_path_and_query
        .split('?')
        .next()
        .is_some_and(|path| path.ends_with("chat/completions"))
}

fn rewrite_chat_completion_model(body: &Bytes, new_model: &str) -> Result<Bytes, ApiError> {
    let mut value: serde_json::Value =
        serde_json::from_slice(body).map_err(InvalidRequestError::InvalidRequestBody)?;
    value["model"] = serde_json::Value::String(new_model.to_string());
    serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(|err| InvalidRequestError::InvalidRequestBody(err).into())
}

fn inference_provider_from_gateway_model(model: &str) -> Result<InferenceProvider, ApiError> {
    let source_model = ModelId::from_str(model).map_err(InternalError::MapperError)?;
    match source_model {
        ModelId::ModelIdWithVersion { provider, .. } => Ok(provider),
        ModelId::Bedrock(_) => Ok(InferenceProvider::Bedrock),
        ModelId::Ollama(_) => Ok(InferenceProvider::Ollama),
        ModelId::Unknown(_) => Err(InvalidRequestError::UnsupportedEndpoint(format!(
            "provider for the given model: '{source_model}' not supported"
        ))
        .into()),
    }
}

fn llm_kv_slot_keys(
    settings: &alephant_llm_kv_cache::CacheSettings,
    target_url: &url::Url,
    body: &Bytes,
) -> Vec<String> {
    let body_str = String::from_utf8_lossy(body).into_owned();
    let mut keys = Vec::new();
    for i in 0..settings.bucket_size {
        let k = alephant_llm_kv_cache::kv_key_sha256_hex(
            settings.cache_seed.as_deref().unwrap_or(""),
            target_url.as_str(),
            &body_str,
            &[],
            i,
        );
        keys.push(k);
    }
    keys
}

fn llm_kv_write_slot_keys(
    settings: &alephant_llm_kv_cache::CacheSettings,
    effective_target_url: &url::Url,
    effective_request_body: &Bytes,
) -> Vec<String> {
    llm_kv_slot_keys(settings, effective_target_url, effective_request_body)
}

fn semantic_write_body_bytes(
    original_request_body: &Bytes,
    _effective_request_body: &Bytes,
) -> Vec<u8> {
    original_request_body.to_vec()
}

fn build_llm_cache_hit_response(
    entry: &alephant_llm_kv_cache::LlmCacheEntry,
    bucket_idx: usize,
    mapper_ctx: &MapperContext,
) -> Result<
    (
        http::Response<crate::types::body::Body>,
        BodyReader,
        oneshot::Receiver<()>,
    ),
    ApiError,
> {
    let entry = entry.clone();
    let mut resp_builder = http::Response::builder().status(StatusCode::OK);
    let hm = resp_builder.headers_mut().unwrap();
    let hdrs: HashMap<String, String> = entry.headers.clone();
    alephant_llm_kv_cache::merge_cached_headers(hm, &hdrs);
    alephant_llm_kv_cache::apply_alephant_cache_hit_headers(hm, bucket_idx, entry.latency);
    let chunks: Vec<Bytes> = entry
        .body
        .iter()
        .map(|s| Bytes::copy_from_slice(s.as_bytes()))
        .collect();
    let stream = futures::stream::iter(chunks.into_iter().map(Ok::<_, ApiError>));
    let append_nl = mapper_ctx.is_stream;
    let tfft = if mapper_ctx.is_stream {
        TfftTrigger::FirstModelToken
    } else {
        TfftTrigger::Never
    };
    let (body, reader, tfft_rx) = BodyReader::wrap_stream(stream, append_nl, tfft, None);
    let response = resp_builder.body(body).map_err(InternalError::HttpError)?;
    Ok((response, reader, tfft_rx))
}

fn enforce_direct_proxy_vk_model_policy(
    app_state: &AppState,
    vk_policy: Option<&VkPolicy>,
    request_kind: RequestKind,
    provider: &InferenceProvider,
    path_and_query: &str,
    req_body_bytes: &Bytes,
    extensions: &http::Extensions,
    auth_ctx: Option<&crate::types::extensions::AuthContext>,
) -> Result<(), ApiError> {
    if extensions
        .get::<crate::content_filter::PolicyModelOverride>()
        .is_some()
    {
        return Ok(());
    }
    if auth_ctx.is_some_and(|a| a.is_custom_provider && a.master_key_base_url.is_some()) {
        return Ok(());
    }
    if !matches!(
        request_kind,
        RequestKind::DirectProxy | RequestKind::CustomProvider
    ) {
        return Ok(());
    }

    if provider != &InferenceProvider::OpenAI {
        return Ok(());
    }

    // Only OpenAI chat/completions has a stable source request schema with
    // model in body for this path.
    if !path_and_query.ends_with("chat/completions") {
        return Ok(());
    }

    let req =
        serde_json::from_slice::<async_openai::types::CreateChatCompletionRequest>(req_body_bytes)
            .map_err(crate::error::invalid_req::InvalidRequestError::InvalidRequestBody)?;

    let mut ext = http::Extensions::new();
    if let Some(policy) = vk_policy.cloned() {
        ext.insert(policy);
    }

    if let Err(e) = check_model_access(&ext, &req.model) {
        app_state.0.metrics.vk.model_denied.add(1, &[]);
        tracing::warn!(
            provider = %provider,
            path = %path_and_query,
            model = %req.model,
            "virtual key model policy denied direct proxy request"
        );
        return Err(e);
    }
    Ok(())
}

fn enforce_workspace_provider_allowlist(
    app_state: &AppState,
    auth_ctx: Option<&crate::types::extensions::AuthContext>,
    target_provider: &InferenceProvider,
) -> Result<(), ApiError> {
    let Some(workspace_id) = allowlist_workspace_id_for_request(auth_ctx) else {
        return Ok(());
    };

    // F-10: enforce workspace provider allowlist in Cloud mode.
    if !app_state.is_provider_allowed_for_workspace(workspace_id, target_provider) {
        tracing::warn!(
            provider = %target_provider,
            workspace_id = %workspace_id,
            "provider not in workspace allowlist — rejecting request (F-10)"
        );
        crate::fallback::observability::log_decision(
            &app_state.config().fallback_policy,
            crate::fallback::observability::DecisionKind::ProviderDenied,
            None,
            target_provider,
        );
        return Err(
            crate::error::internal::InternalError::ProviderNotAllowedForWorkspace(
                target_provider.clone(),
            )
            .into(),
        );
    }
    Ok(())
}

fn allowlist_workspace_id_for_request(
    auth_ctx: Option<&crate::types::extensions::AuthContext>,
) -> Option<uuid::Uuid> {
    auth_ctx.map(|ctx| *ctx.org_id.as_ref())
}

/// True when the last non-empty path segment is `v` + ASCII digits (e.g.
/// `v1`, `V4`), so the upstream base already carries an API revision.
fn base_path_ends_with_v_version_segment(url: &url::Url) -> bool {
    let path = url.path().trim_matches('/');
    if path.is_empty() {
        return false;
    }
    let last = path.rsplit('/').next().unwrap_or("");
    segment_is_v_digit_version(last)
}

fn segment_is_v_digit_version(segment: &str) -> bool {
    let b = segment.as_bytes();
    if b.len() < 2 {
        return false;
    }
    if !matches!(b[0], b'v' | b'V') {
        return false;
    }
    b[1..].iter().all(|x| x.is_ascii_digit())
}

/// If `path` (no query) starts with `v{digits}/` after trimming leading `/`,
/// return the remainder; otherwise `None`.
fn strip_leading_numeric_v_revision_prefix(path: &str) -> Option<&str> {
    let path = path.trim_start_matches('/');
    let (first, rest) = path.split_once('/')?;
    if rest.is_empty() {
        return None;
    }
    if !segment_is_v_digit_version(first) {
        return None;
    }
    Some(rest)
}

/// When the provider `base_url` already ends with `.../vN`, avoid duplicating
/// another `v1/` (OpenAI-style) before `chat/completions`, etc.
fn adjust_upstream_path_for_versioned_base<'a>(
    base_url: &url::Url,
    path_and_query: &'a str,
) -> Cow<'a, str> {
    if !base_path_ends_with_v_version_segment(base_url) {
        return Cow::Borrowed(path_and_query);
    }
    let (path_only, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_and_query, None),
    };
    let Some(stripped) = strip_leading_numeric_v_revision_prefix(path_only) else {
        return Cow::Borrowed(path_and_query);
    };
    let mut out = stripped.to_string();
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Cow::Owned(out)
}

fn join_provider_upstream_url(base_url: &url::Url, path_and_query: &str) -> url::Url {
    let adjusted = adjust_upstream_path_for_versioned_base(base_url, path_and_query);
    let slice = match &adjusted {
        Cow::Borrowed(s) => *s,
        Cow::Owned(s) => s.as_str(),
    };
    let mut normalized_base = base_url.clone();
    if base_path_ends_with_v_version_segment(&normalized_base)
        && !normalized_base.path().ends_with('/')
    {
        let with_trailing_slash = format!("{}/", normalized_base.path());
        normalized_base.set_path(&with_trailing_slash);
    }
    normalized_base
        .join(slice)
        .expect("PathAndQuery joined with valid url will always succeed")
}

fn resolve_master_key_target_url(
    auth_ctx: Option<&crate::types::extensions::AuthContext>,
    extracted_path_and_query: &str,
) -> Option<url::Url> {
    let base_url_str = auth_ctx?.master_key_base_url.as_ref()?;
    if let Ok(base_url) = url::Url::parse(base_url_str) {
        Some(join_provider_upstream_url(
            &base_url,
            extracted_path_and_query,
        ))
    } else {
        tracing::warn!(
            base_url = %base_url_str,
            "master_key base_url is not a valid URL, falling through"
        );
        None
    }
}

fn remove_internal_gateway_auth_headers(headers: &mut HeaderMap) {
    headers.remove(HeaderName::from_static("alephant-api-key"));
}

fn sanitize_upstream_headers(headers: &mut HeaderMap) {
    headers.remove(http::header::HOST);
    headers.remove(http::header::AUTHORIZATION);
    headers.remove(http::header::CONTENT_LENGTH);
    remove_internal_gateway_auth_headers(headers);
    headers.remove("Alephant-Embeddings-Key");
    headers.remove("Alephant-Embeddings-Model");
    headers.remove("Alephant-Cache-Semantic-Threshold");
    headers.remove("Alephant-Cache-Ttl");
    remove_session_headers(headers);
    // TODO: properly support accept encoding
    headers.remove(http::header::ACCEPT_ENCODING);
    headers.insert(
        http::header::ACCEPT_ENCODING,
        HeaderValue::from_static("identity"),
    );
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn dispatch_stream_with_retry(
    app_state: &AppState,
    request_builder: RequestBuilder,
    req_body_bytes: Bytes,
    api_endpoint: Option<ApiEndpoint>,
    metrics_registry: EndpointMetricsRegistry,
    request_ctx: &RequestContext,
    request_kind: RequestKind,
    cache_tap: Option<mpsc::UnboundedSender<Bytes>>,
) -> Result<
    (
        http::Response<crate::types::body::Body>,
        crate::types::body::BodyReader,
        oneshot::Receiver<()>,
    ),
    ApiError,
> {
    let retry_config = get_retry_config(app_state, request_kind, request_ctx);
    let fallback_policy_for_log = app_state.config().fallback_policy.clone();
    let provider_for_log = api_endpoint
        .as_ref()
        .map(|e| e.provider().to_string())
        .unwrap_or_default();

    if let Some(retry_config) = retry_config {
        match retry_config.as_ref() {
            RetryConfig::Exponential {
                min_delay,
                max_delay,
                max_retries,
                factor,
            } => {
                let retry_strategy = ExponentialBuilder::default()
                    .with_max_delay(*max_delay)
                    .with_min_delay(*min_delay)
                    .with_max_times(usize::from(*max_retries))
                    .with_factor(
                        factor
                            .to_f32()
                            .unwrap_or(crate::config::retry::DEFAULT_RETRY_FACTOR),
                    )
                    .with_jitter()
                    .build();
                (|| async {
                    Dispatcher::dispatch_stream(
                        &request_builder,
                        req_body_bytes.clone(),
                        api_endpoint.clone(),
                        metrics_registry.clone(),
                        cache_tap.clone(),
                    )
                    .await
                })
                .retry(retry_strategy)
                .sleep(tokio::time::sleep)
                .when(|e: &ApiError| match e {
                    ApiError::StreamError(s) => s.is_retryable(),
                    _ => false,
                })
                .notify(|err: &ApiError, dur: Duration| {
                    if let ApiError::StreamError(_s) = err {
                        tracing::warn!(
                            error = %err,
                            retry_in = ?dur,
                            "upstream server error in stream, retrying...",
                        );
                        crate::fallback::observability::log_decision(
                            &fallback_policy_for_log,
                            crate::fallback::observability::DecisionKind::Retry,
                            None,
                            &provider_for_log,
                        );
                    }
                })
                .await
            }
            RetryConfig::Constant { delay, max_retries } => {
                let retry_strategy = ConstantBuilder::default()
                    .with_delay(*delay)
                    .with_max_times(usize::from(*max_retries))
                    .with_jitter()
                    .build();
                (|| async {
                    Dispatcher::dispatch_stream(
                        &request_builder,
                        req_body_bytes.clone(),
                        api_endpoint.clone(),
                        metrics_registry.clone(),
                        cache_tap.clone(),
                    )
                    .await
                })
                .retry(retry_strategy)
                .sleep(tokio::time::sleep)
                .when(|e: &ApiError| match e {
                    ApiError::StreamError(s) => s.is_retryable(),
                    _ => false,
                })
                .notify(|err: &ApiError, dur: Duration| {
                    if let ApiError::StreamError(_s) = err {
                        tracing::warn!(
                            error = %err,
                            retry_in = ?dur,
                            "upstream server error in stream, retrying...",
                        );
                        crate::fallback::observability::log_decision(
                            &fallback_policy_for_log,
                            crate::fallback::observability::DecisionKind::Retry,
                            None,
                            &provider_for_log,
                        );
                    }
                })
                .await
            }
        }
    } else {
        Dispatcher::dispatch_stream(
            &request_builder,
            req_body_bytes.clone(),
            api_endpoint,
            metrics_registry,
            cache_tap,
        )
        .await
    }
}

fn extract_retry_after(headers: &HeaderMap) -> Option<u64> {
    let retry_after_str = headers
        .get(http::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())?;

    // First try to parse as seconds (u64)
    if let Ok(seconds) = retry_after_str.parse::<u64>() {
        // The value is in seconds, return seconds from now
        return Some(seconds);
    }

    // If that fails, try to parse as HTTP date format
    if let Ok(datetime) = DateTime::parse_from_str(retry_after_str, "%a, %d %b %Y %H:%M:%S GMT") {
        // Convert to seconds from now
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch is always earlier than now")
            .as_secs();
        let target = u64::try_from(datetime.to_utc().timestamp()).unwrap_or(0);
        if target > now {
            return Some(target - now);
        }
    }

    None
}

fn stream_response_headers() -> HeaderMap {
    HeaderMap::from_iter([
        (
            http::header::CONTENT_TYPE,
            HeaderValue::from_str("text/event-stream; charset=utf-8").unwrap(),
        ),
        (
            http::header::CONNECTION,
            HeaderValue::from_str("keep-alive").unwrap(),
        ),
        (
            http::header::TRANSFER_ENCODING,
            HeaderValue::from_str("chunked").unwrap(),
        ),
    ])
}

fn request_log_id_from_headers(headers: &HeaderMap) -> Uuid {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
        .unwrap_or_else(Uuid::new_v4)
}

fn get_retry_config<'a>(
    app_state: &'a AppState,
    request_kind: RequestKind,
    _req_ctx: &RequestContext,
) -> Option<std::borrow::Cow<'a, RetryConfig>> {
    if matches!(
        request_kind,
        RequestKind::DirectProxy | RequestKind::CustomProvider
    ) {
        return None;
    }
    fallback_bridge::resolved_global_retry(app_state.config())
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, collections::HashMap, sync::Arc};

    use http::HeaderValue;
    use indexmap::IndexSet;
    use uuid::Uuid;

    use super::*;
    use crate::{
        app::build_test_app,
        config::{
            Config,
            providers::GlobalProviderConfig,
            router::{RouterConfig, RouterProviderConfig},
        },
        session_headers::ALEPHANT_SESSION_ID_HEADER,
        types::{
            extensions::{
                AuthContext, LargeContextAction, LargeContextDecision, PromptCompressionTokenPair,
                PromptContext, UnifiedImplicitModelFallbackContext, VkPolicy,
            },
            org::OrgId,
            secret::Secret,
            user::UserId,
        },
    };

    fn auth_ctx_with_base_url(base_url: Option<&str>) -> AuthContext {
        AuthContext {
            api_key: Secret::from("sk-test".to_string()),
            user_id: UserId::new(Uuid::new_v4()),
            org_id: OrgId::new(Uuid::new_v4()),
            virtual_key_id: Some(Uuid::new_v4()),
            virtual_key_prefix: String::new(),
            master_key_id: Some(Uuid::new_v4()),
            master_key_base_url: base_url.map(ToOwned::to_owned),
            department_id: Uuid::nil(),
            entity_type: String::new(),
            entity_id: Uuid::nil(),
            entity_name: String::new(),
            body_ttl_days: 90,
            is_custom_provider: false,
            master_key_allowed_providers: None,
        }
    }

    #[test]
    fn resolve_master_key_target_url_uses_valid_override() {
        let auth = auth_ctx_with_base_url(Some("https://example.com"));
        let url =
            resolve_master_key_target_url(Some(&auth), "/v1/chat").expect("valid url expected");
        assert_eq!(url.as_str(), "https://example.com/v1/chat");
    }

    #[test]
    fn resolve_master_key_target_url_returns_none_for_invalid_override() {
        let auth = auth_ctx_with_base_url(Some("not a url"));
        let url = resolve_master_key_target_url(Some(&auth), "/v1/chat");
        assert!(url.is_none());
    }

    #[test]
    fn resolve_master_key_target_url_returns_none_when_absent() {
        let auth = auth_ctx_with_base_url(None);
        let url = resolve_master_key_target_url(Some(&auth), "/v1/chat");
        assert!(url.is_none());
    }

    #[test]
    fn remove_internal_gateway_auth_headers_removes_alephant_key() {
        let mut headers = HeaderMap::new();
        headers.insert("alephant-api-key", HeaderValue::from_static("new"));

        remove_internal_gateway_auth_headers(&mut headers);

        assert!(!headers.contains_key("alephant-api-key"));
    }

    #[test]
    fn remove_internal_gateway_auth_headers_keeps_other_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer provider-key"),
        );

        remove_internal_gateway_auth_headers(&mut headers);

        assert!(headers.contains_key(http::header::AUTHORIZATION));
    }

    #[test]
    fn sanitize_upstream_headers_removes_session_headers_and_keeps_other_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer provider-key"),
        );
        headers.insert(
            ALEPHANT_SESSION_ID_HEADER,
            HeaderValue::from_static("session-123"),
        );
        headers.insert(
            "alephant-session-path",
            HeaderValue::from_static("/workflow"),
        );
        headers.insert(
            "Alephant-Embeddings-Key",
            HeaderValue::from_static("sk-secret"),
        );
        headers.insert(
            "Alephant-Embeddings-Model",
            HeaderValue::from_static("openai/text-embedding-3-small"),
        );
        headers.insert(
            "Alephant-Cache-Semantic-Threshold",
            HeaderValue::from_static("0.95"),
        );
        headers.insert("Alephant-Cache-Ttl", HeaderValue::from_static("60"));
        headers.insert("x-keep-me", HeaderValue::from_static("alive"));

        sanitize_upstream_headers(&mut headers);

        assert!(!headers.contains_key(http::header::AUTHORIZATION));
        assert!(!headers.contains_key(ALEPHANT_SESSION_ID_HEADER));
        assert!(!headers.contains_key("alephant-session-path"));
        assert!(!headers.contains_key("Alephant-Embeddings-Key"));
        assert!(!headers.contains_key("Alephant-Embeddings-Model"));
        assert!(!headers.contains_key("Alephant-Cache-Semantic-Threshold"));
        assert!(!headers.contains_key("Alephant-Cache-Ttl"));
        assert_eq!(
            headers
                .get("x-keep-me")
                .and_then(|value| value.to_str().ok()),
            Some("alive")
        );
        assert_eq!(
            headers
                .get(http::header::ACCEPT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("identity")
        );
    }

    fn direct_body_with_model(model: &str) -> Bytes {
        Bytes::from(
            serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        )
    }

    #[test]
    fn direct_proxy_policy_denies_blocked_openai_chat() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let app = rt
            .block_on(build_test_app(Config::default()))
            .expect("build app");
        let mut ext = http::Extensions::new();
        ext.insert(VkPolicy {
            virtual_key_id: Uuid::new_v4(),
            allowed_models: None,
            blocked_models: Some(vec!["gpt-4".to_string()]),
        });
        let err = enforce_direct_proxy_vk_model_policy(
            &app.state,
            ext.get::<VkPolicy>(),
            RequestKind::DirectProxy,
            &InferenceProvider::OpenAI,
            "/v1/chat/completions",
            &direct_body_with_model("gpt-4"),
            &ext,
            None,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ApiError::InvalidRequest(
                crate::error::invalid_req::InvalidRequestError::ModelAccessDenied(_)
            )
        ));
    }

    #[test]
    fn direct_proxy_policy_skips_non_openai_provider() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let app = rt
            .block_on(build_test_app(Config::default()))
            .expect("build app");
        let ext = http::Extensions::new();
        assert!(
            enforce_direct_proxy_vk_model_policy(
                &app.state,
                ext.get::<VkPolicy>(),
                RequestKind::DirectProxy,
                &InferenceProvider::Anthropic,
                "/v1/messages",
                &Bytes::from("{}"),
                &ext,
                None,
            )
            .is_ok()
        );
    }

    #[test]
    fn direct_proxy_policy_skips_when_not_direct_proxy_kind() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let app = rt
            .block_on(build_test_app(Config::default()))
            .expect("build app");
        let ext = http::Extensions::new();
        assert!(
            enforce_direct_proxy_vk_model_policy(
                &app.state,
                ext.get::<VkPolicy>(),
                RequestKind::UnifiedApi,
                &InferenceProvider::OpenAI,
                "/v1/chat/completions",
                &direct_body_with_model("gpt-4"),
                &ext,
                None,
            )
            .is_ok()
        );
    }

    fn empty_request_ctx() -> RequestContext {
        RequestContext {
            auth_context: None,
            router_config: None,
            llm_kv_cache_read_allowed: true,
            llm_kv_cache_write_allowed: true,
        }
    }

    fn request_ctx(
        auth_context: Option<AuthContext>,
        router_config: Option<RouterConfig>,
    ) -> RequestContext {
        RequestContext {
            auth_context,
            router_config: router_config.map(Arc::new),
            llm_kv_cache_read_allowed: true,
            llm_kv_cache_write_allowed: true,
        }
    }

    fn build_test_dispatcher(config: Config, provider: InferenceProvider) -> Dispatcher {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let app = rt.block_on(build_test_app(config)).expect("build app");
        let client = rt
            .block_on(Client::new(&app.state, provider.clone()))
            .expect("client");
        Dispatcher {
            client,
            app_state: app.state,
            provider,
            rate_limit_tx: None,
        }
    }

    fn sync_result_with_status(
        status: StatusCode,
    ) -> Result<
        (
            http::Response<crate::types::body::Body>,
            crate::types::body::BodyReader,
            oneshot::Receiver<()>,
        ),
        ApiError,
    > {
        let stream = futures::stream::once(futures::future::ok::<_, ApiError>(Bytes::new()));
        let (body, reader, rx) = BodyReader::wrap_stream(stream, false, TfftTrigger::Never, None);
        Ok((
            http::Response::builder()
                .status(status)
                .body(body)
                .expect("response"),
            reader,
            rx,
        ))
    }

    #[test]
    fn extract_request_context_reads_large_context_decision() {
        let mut req = http::Request::builder()
            .uri("https://example.com/ai/chat/completions")
            .body(crate::types::body::Body::empty())
            .expect("request");
        req.extensions_mut().insert(MapperContext {
            is_stream: false,
            model: None,
            anthropic_openai_usage: None,
            unified_responses_bridge_chat_completions_sse: false,
        });
        req.extensions_mut().insert(Arc::new(empty_request_ctx()));
        req.extensions_mut().insert(
            "/chat/completions"
                .parse::<PathAndQuery>()
                .expect("path and query"),
        );
        req.extensions_mut().insert(InferenceProvider::OpenAI);
        req.extensions_mut().insert(RequestKind::UnifiedApi);
        req.extensions_mut().insert(PromptContext {
            prompt_id: "prompt-1".to_string(),
            prompt_version_id: Some("version-1".to_string()),
            inputs: None,
        });
        req.extensions_mut().insert(PromptCompressionTokenPair {
            origin_prompt_token: 4096,
            compression_prompt_token: 2048,
        });
        req.extensions_mut().insert(LargeContextDecision {
            handler:
                crate::middleware::large_context::headers::TokenLimitExceptionHandler::Fallback,
            action: LargeContextAction::FallbackApplied,
            original_model: Some("openai/gpt-4o-mini,openai/gpt-4o".to_string()),
            effective_model: Some("openai/gpt-4o".to_string()),
            estimated_input_tokens: Some(120_000),
            model_context_limit: Some(128_000),
            input_budget_tokens: Some(115_200),
        });

        let (
            _mapper_ctx,
            _req_ctx,
            _api_endpoint,
            _path_and_query,
            _provider,
            _router_id,
            _mapper_profile_context,
            _start_instant,
            _start_time,
            _request_kind,
            _prompt_ctx,
            _prompt_header_for_log,
            large_context_decision,
            prompt_compression_tokens,
        ) = Dispatcher::extract_request_context(&mut req).expect("extract context");

        assert_eq!(
            prompt_compression_tokens,
            Some(PromptCompressionTokenPair {
                origin_prompt_token: 4096,
                compression_prompt_token: 2048,
            })
        );
        assert!(
            req.extensions()
                .get::<PromptCompressionTokenPair>()
                .is_none(),
            "prompt compression tokens should be removed from request \
             extensions"
        );
        let large_context_decision = large_context_decision.expect("large context decision");
        assert_eq!(large_context_decision.handler.as_str(), "fallback");
        assert_eq!(large_context_decision.action.as_str(), "fallback-applied");
        assert_eq!(
            large_context_decision.effective_model.as_deref(),
            Some("openai/gpt-4o")
        );
    }

    #[test]
    fn build_target_url_prefers_master_key_base_url_over_router_and_global() {
        let mut config = Config::default();
        config
            .providers
            .get_mut(&InferenceProvider::OpenAI)
            .expect("openai config")
            .base_url = "https://global-provider.test/".parse().unwrap();
        let dispatcher = build_test_dispatcher(config, InferenceProvider::OpenAI);

        let req_ctx = request_ctx(
            Some(auth_ctx_with_base_url(Some("https://master-key.test/"))),
            Some(RouterConfig {
                providers: Some(HashMap::from([(
                    InferenceProvider::OpenAI,
                    RouterProviderConfig {
                        base_url: "https://router-provider.test/".parse().unwrap(),
                        version: None,
                    },
                )])),
                ..RouterConfig::default()
            }),
        );

        let target_url = dispatcher
            .build_target_url(&req_ctx, &InferenceProvider::OpenAI, "/v1/chat/completions")
            .expect("target url");

        assert_eq!(
            target_url.as_str(),
            "https://master-key.test/v1/chat/completions"
        );
    }

    #[test]
    fn build_target_url_uses_router_provider_base_url_when_no_master_key_override() {
        let mut config = Config::default();
        config
            .providers
            .get_mut(&InferenceProvider::OpenAI)
            .expect("openai config")
            .base_url = "https://global-provider.test/".parse().unwrap();
        let dispatcher = build_test_dispatcher(config, InferenceProvider::OpenAI);

        let req_ctx = request_ctx(
            Some(auth_ctx_with_base_url(None)),
            Some(RouterConfig {
                providers: Some(HashMap::from([(
                    InferenceProvider::OpenAI,
                    RouterProviderConfig {
                        base_url: "https://router-provider.test/".parse().unwrap(),
                        version: None,
                    },
                )])),
                ..RouterConfig::default()
            }),
        );

        let target_url = dispatcher
            .build_target_url(&req_ctx, &InferenceProvider::OpenAI, "/v1/chat/completions")
            .expect("target url");

        assert_eq!(
            target_url.as_str(),
            "https://router-provider.test/v1/chat/completions"
        );
    }

    #[test]
    fn build_target_url_falls_back_to_global_provider_config_when_router_absent() {
        let mut config = Config::default();
        config
            .providers
            .get_mut(&InferenceProvider::OpenAI)
            .expect("openai config")
            .base_url = "https://global-provider.test/".parse().unwrap();
        let dispatcher = build_test_dispatcher(config, InferenceProvider::OpenAI);

        let req_ctx = request_ctx(Some(auth_ctx_with_base_url(None)), None);

        let target_url = dispatcher
            .build_target_url(&req_ctx, &InferenceProvider::OpenAI, "/v1/chat/completions")
            .expect("target url");

        assert_eq!(
            target_url.as_str(),
            "https://global-provider.test/v1/chat/completions"
        );
    }

    #[test]
    fn build_target_url_strips_leading_api_revision_when_base_path_ends_with_vn() {
        let mut config = Config::default();
        config
            .providers
            .get_mut(&InferenceProvider::OpenAI)
            .expect("openai config")
            .base_url = "https://open.bigmodel.cn/api/paas/v4/".parse().unwrap();
        let dispatcher = build_test_dispatcher(config, InferenceProvider::OpenAI);
        let req_ctx = request_ctx(Some(auth_ctx_with_base_url(None)), None);

        for path in ["v1/chat/completions", "/v1/chat/completions"] {
            let target_url = dispatcher
                .build_target_url(&req_ctx, &InferenceProvider::OpenAI, path)
                .expect("target url");
            assert_eq!(
                target_url.as_str(),
                "https://open.bigmodel.cn/api/paas/v4/chat/completions",
                "path={path:?}",
            );
        }
    }

    #[test]
    fn build_target_url_strips_v_prefix_for_versioned_master_key_base_url() {
        let mut config = Config::default();
        config
            .providers
            .get_mut(&InferenceProvider::OpenAI)
            .expect("openai config")
            .base_url = "https://global-provider.test/".parse().unwrap();
        let dispatcher = build_test_dispatcher(config, InferenceProvider::OpenAI);

        let req_ctx = request_ctx(
            Some(auth_ctx_with_base_url(Some(
                "https://mk.example/api/service/v2/",
            ))),
            None,
        );

        let target_url = dispatcher
            .build_target_url(&req_ctx, &InferenceProvider::OpenAI, "v1/embeddings?user=a")
            .expect("target url");
        assert_eq!(
            target_url.as_str(),
            "https://mk.example/api/service/v2/embeddings?user=a"
        );
    }

    #[test]
    fn build_target_url_keeps_base_revision_when_vn_has_no_trailing_slash() {
        let mut config = Config::default();
        config
            .providers
            .get_mut(&InferenceProvider::OpenAI)
            .expect("openai config")
            .base_url = "https://open.bigmodel.cn/api/paas/v4".parse().unwrap();
        let dispatcher = build_test_dispatcher(config, InferenceProvider::OpenAI);
        let req_ctx = request_ctx(Some(auth_ctx_with_base_url(None)), None);

        let target_url = dispatcher
            .build_target_url(&req_ctx, &InferenceProvider::OpenAI, "v1/chat/completions")
            .expect("target url");
        assert_eq!(
            target_url.as_str(),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
    }

    #[test]
    fn get_retry_config_direct_proxy_is_none() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let app = rt
            .block_on(build_test_app(Config::default()))
            .expect("build app");
        let req_ctx = empty_request_ctx();
        let retry = get_retry_config(&app.state, RequestKind::DirectProxy, &req_ctx);
        assert!(retry.is_none(), "direct proxy should skip global retry");
    }

    #[test]
    fn get_retry_config_uses_fallback_policy_when_enabled() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let mut config = Config::default();
        config.fallback_policy.enabled = true;
        config.fallback_policy.retry = RetryConfig::Constant {
            delay: Duration::from_millis(7),
            max_retries: 1,
        };
        config.global.retries = Some(RetryConfig::Constant {
            delay: Duration::from_secs(99),
            max_retries: 1,
        });
        let app = rt.block_on(build_test_app(config)).expect("build app");
        let req_ctx = empty_request_ctx();
        let retry = get_retry_config(&app.state, RequestKind::UnifiedApi, &req_ctx)
            .expect("retry config should exist");
        assert_eq!(
            retry,
            Cow::Owned(RetryConfig::Constant {
                delay: Duration::from_millis(7),
                max_retries: 1,
            }),
            "fallback-policy retry should take precedence"
        );
    }

    #[test]
    fn get_retry_config_falls_back_to_global_when_policy_disabled() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let mut config = Config::default();
        config.fallback_policy.enabled = false;
        config.global.retries = Some(RetryConfig::Constant {
            delay: Duration::from_millis(11),
            max_retries: 2,
        });
        let app = rt.block_on(build_test_app(config)).expect("build app");
        let req_ctx = empty_request_ctx();
        let retry = get_retry_config(&app.state, RequestKind::Router, &req_ctx)
            .expect("global retry should be used");
        assert_eq!(
            retry,
            Cow::Owned(RetryConfig::Constant {
                delay: Duration::from_millis(11),
                max_retries: 2,
            })
        );
    }

    #[test]
    fn get_retry_config_returns_none_when_policy_and_global_disabled() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let mut config = Config::default();
        config.fallback_policy.enabled = false;
        config.global.retries = None;
        let app = rt.block_on(build_test_app(config)).expect("build app");
        let req_ctx = empty_request_ctx();
        let retry = get_retry_config(&app.state, RequestKind::Router, &req_ctx);
        assert!(retry.is_none(), "no retry expected");
    }

    #[test]
    fn cross_provider_fallback_requires_implicit_chat_retryable_result() {
        let ctx = UnifiedImplicitModelFallbackContext {
            selected_model: "openai/gpt-5.4".to_string(),
        };
        assert!(should_attempt_cross_provider_default_model_fallback(
            true,
            RequestKind::UnifiedApi,
            "/v1/chat/completions",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
        assert!(should_attempt_cross_provider_default_model_fallback(
            true,
            RequestKind::UnifiedApi,
            "/v1/chat/completions?user=test",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
        assert!(!should_attempt_cross_provider_default_model_fallback(
            true,
            RequestKind::UnifiedApi,
            "/v1/chat/completions",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_REQUEST),
        ));
        assert!(!should_attempt_cross_provider_default_model_fallback(
            true,
            RequestKind::DirectProxy,
            "/v1/chat/completions",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
        assert!(!should_attempt_cross_provider_default_model_fallback(
            true,
            RequestKind::UnifiedApi,
            "/v1/responses",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
        assert!(!should_attempt_cross_provider_default_model_fallback(
            true,
            RequestKind::UnifiedApi,
            "/v1/chat/completions",
            None,
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
    }

    #[test]
    fn cross_provider_fallback_requires_retry_to_have_occurred() {
        let ctx = UnifiedImplicitModelFallbackContext {
            selected_model: "openai/gpt-5.4".to_string(),
        };
        assert!(!should_attempt_cross_provider_default_model_fallback(
            false,
            RequestKind::UnifiedApi,
            "/v1/chat/completions",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
    }

    #[test]
    fn rewrite_chat_completion_model_replaces_model_field() {
        let body = Bytes::from(
            serde_json::json!({
                "model": "openai/gpt-5.4",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );
        let rewritten =
            rewrite_chat_completion_model(&body, "google/gemini-2.5-pro").expect("rewrite body");
        let value: serde_json::Value = serde_json::from_slice(&rewritten).expect("json body");
        assert_eq!(
            value.get("model").and_then(serde_json::Value::as_str),
            Some("google/gemini-2.5-pro")
        );
    }

    #[test]
    fn cache_write_keys_follow_effective_request_after_fallback() {
        let settings = alephant_llm_kv_cache::CacheSettings {
            should_read: false,
            should_write: true,
            cache_control_value: "public, max-age=60".to_string(),
            bucket_size: 2,
            cache_seed: Some("seed".to_string()),
        };
        let original_url =
            url::Url::parse("https://openai.test/v1/chat/completions").expect("original url");
        let effective_url =
            url::Url::parse("https://groq.test/v1/chat/completions").expect("effective url");
        let original_body = Bytes::from(
            serde_json::json!({
                "model": "openai/gpt-5.4",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );
        let effective_body = Bytes::from(
            serde_json::json!({
                "model": "groq/llama-3.1-8b",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );

        let original_keys = llm_kv_slot_keys(&settings, &original_url, &original_body);
        let effective_keys = llm_kv_write_slot_keys(&settings, &effective_url, &effective_body);

        assert_ne!(effective_keys, original_keys);
        assert_eq!(
            effective_keys,
            llm_kv_slot_keys(&settings, &effective_url, &effective_body,)
        );
    }

    #[test]
    fn semantic_write_body_uses_original_request_body() {
        let original_body = Bytes::from(
            serde_json::json!({
                "model": "openai/gpt-4",
                "messages": [{"role": "user", "content": "original"}]
            })
            .to_string(),
        );
        let effective_body = Bytes::from(
            serde_json::json!({
                "model": "google/gemini-2.5-pro",
                "messages": [{"role": "user", "content": "effective"}]
            })
            .to_string(),
        );

        let semantic_body = semantic_write_body_bytes(&original_body, &effective_body);
        assert_eq!(semantic_body, original_body.to_vec());
    }

    #[test]
    fn cross_provider_fallback_request_details_use_effective_provider_url_and_body() {
        let openai_provider = InferenceProvider::OpenAI;
        let groq_provider = InferenceProvider::Named("groq".into());
        let groq_model = "llama-3.1-8b";
        let mut config = Config::default();
        config
            .providers
            .get_mut(&openai_provider)
            .expect("openai config")
            .base_url = "https://openai.test/".parse().expect("openai url");
        config.providers.insert(
            groq_provider.clone(),
            GlobalProviderConfig {
                models: IndexSet::from([ModelId::from_str_and_provider(
                    groq_provider.clone(),
                    groq_model,
                )
                .expect("groq model")]),
                base_url: "https://groq.test/".parse().expect("groq url"),
                version: None,
                upstream_auth: Default::default(),
            },
        );
        let dispatcher = build_test_dispatcher(config, openai_provider.clone());
        let req_ctx = request_ctx(Some(auth_ctx_with_base_url(None)), None);
        let req_body_bytes = Bytes::from(
            serde_json::json!({
                "model": "openai/gpt-5.4",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );

        let (effective_provider, effective_target_url, effective_request_body) = dispatcher
            .cross_provider_fallback_request_details(
                &req_ctx,
                "/v1/chat/completions",
                &req_body_bytes,
                &format!("groq/{groq_model}"),
            )
            .expect("fallback request details");

        assert_eq!(effective_provider, groq_provider);
        assert_eq!(
            effective_target_url.as_str(),
            "https://groq.test/v1/chat/completions"
        );
        let effective_body: serde_json::Value =
            serde_json::from_slice(&effective_request_body).expect("effective body json");
        assert_eq!(
            effective_body
                .get("model")
                .and_then(serde_json::Value::as_str),
            Some("groq/llama-3.1-8b")
        );
    }

    #[test]
    fn allowlist_guard_allows_when_workspace_allowlist_empty() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let app = rt
            .block_on(build_test_app(Config::default()))
            .expect("build app");
        let auth = auth_ctx_with_base_url(None);
        assert!(
            enforce_workspace_provider_allowlist(
                &app.state,
                Some(&auth),
                &InferenceProvider::OpenAI,
            )
            .is_ok()
        );
    }

    #[test]
    fn allowlist_guard_skips_without_auth_context_in_cloud() {
        assert!(
            allowlist_workspace_id_for_request(None).is_none(),
            "cloud request without auth context must skip allowlist \
             enforcement"
        );
    }

    #[test]
    fn allowlist_guard_extracts_workspace_id_in_cloud_with_auth() {
        let auth = auth_ctx_with_base_url(None);
        assert_eq!(
            allowlist_workspace_id_for_request(Some(&auth)),
            Some(*auth.org_id.as_ref())
        );
    }
}
