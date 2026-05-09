//! Route and HTTP-method checks **before** authentication.
//!
//! Ensures unknown paths return 404 and disallowed methods (e.g. GET on
//! POST-only inference paths) return 405 without consuming auth or request
//! bodies.

use std::{
    sync::Arc,
    task::{Context, Poll},
};

use http::{Method, uri::PathAndQuery};
use rustc_hash::FxHashSet;
use tower::{Layer, Service};

use crate::{
    endpoints::ApiEndpoint,
    error::{api::ApiError, internal::InternalError, invalid_req::InvalidRequestError},
    router::{router_details::RouteType, unified_api::UnifiedApi},
    types::{provider::InferenceProvider, request::Request, response::Response},
};

/// Providers that have a direct-proxy stack configured (same keys as
/// [`crate::router::direct::DirectProxiesWithoutMapper`]).
#[derive(Clone)]
pub struct RoutingPrecheckLayer {
    direct_proxy_providers: Arc<FxHashSet<InferenceProvider>>,
}

impl RoutingPrecheckLayer {
    #[must_use]
    pub fn new(direct_proxy_providers: Arc<FxHashSet<InferenceProvider>>) -> Self {
        Self {
            direct_proxy_providers,
        }
    }
}

impl<S> Layer<S> for RoutingPrecheckLayer {
    type Service = RoutingPrecheckService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RoutingPrecheckService {
            inner,
            direct_proxy_providers: self.direct_proxy_providers.clone(),
        }
    }
}

#[derive(Clone)]
pub struct RoutingPrecheckService<S> {
    inner: S,
    direct_proxy_providers: Arc<FxHashSet<InferenceProvider>>,
}

/// Paths that mirror upstream LLM HTTP APIs and expect a JSON **POST** body.
#[must_use]
pub(crate) fn path_requires_post(path: &str) -> bool {
    let p = path.trim_start_matches('/');
    if p == "chat/completions" || p.ends_with("/chat/completions") {
        return true;
    }
    if p == "v1/chat/completions" || p.ends_with("/v1/chat/completions") {
        return true;
    }
    // Unified API uses stripped subpaths: `messages`, not `v1/messages`.
    if p == "messages"
        || p == "v1/messages"
        || p.ends_with("/v1/messages")
        || p.ends_with("/messages")
    {
        return true;
    }
    if p.contains("/converse") {
        return true;
    }
    if p.contains("v1beta/openai/chat/completions") {
        return true;
    }
    if p == "embeddings"
        || p.ends_with("/v1/embeddings")
        || p.ends_with("v1/embeddings")
        || p.ends_with("/embeddings")
    {
        return true;
    }
    if p == "images/generations" || p.contains("images/generations") {
        return true;
    }
    if p == "responses"
        || p == "v1/responses"
        || p.ends_with("/v1/responses")
        || p.ends_with("/responses")
    {
        return true;
    }
    // Legacy completions: bare `completions` or `.../completions` (not
    // `.../chat/completions`, handled above).
    if p == "completions" || (p.ends_with("/completions") && !p.ends_with("/chat/completions")) {
        return true;
    }
    false
}

fn check_method(method: &Method, path: &str) -> Result<(), ApiError> {
    if *method == Method::OPTIONS {
        // CORS preflight: must not require POST or auth.
        return Ok(());
    }
    if path_requires_post(path) && *method != Method::POST {
        return Err(ApiError::InvalidRequest(
            InvalidRequestError::MethodNotAllowed {
                method: method.as_str().to_string(),
                path: path.to_string(),
            },
        ));
    }
    Ok(())
}

fn precheck(req: &Request, direct_allowed: &FxHashSet<InferenceProvider>) -> Result<(), ApiError> {
    let Some(route_type) = req.extensions().get::<RouteType>() else {
        return Err(ApiError::InvalidRequest(InvalidRequestError::NotFound(
            req.uri().path().to_string(),
        )));
    };

    let path_and_query = req
        .extensions()
        .get::<PathAndQuery>()
        .ok_or_else(|| ApiError::Internal(InternalError::ExtensionNotFound("PathAndQuery")))?;
    let path = path_and_query.path();

    match route_type {
        RouteType::UnifiedApi { .. } => {
            UnifiedApi::try_from(path).map_err(|_| {
                ApiError::InvalidRequest(InvalidRequestError::NotFound(
                    req.uri().path().to_string(),
                ))
            })?;
            check_method(req.method(), path)?;
        }
        RouteType::Router { .. } => {
            if ApiEndpoint::new(path).is_none() {
                return Err(ApiError::InvalidRequest(InvalidRequestError::NotFound(
                    path.to_string(),
                )));
            }
            check_method(req.method(), path)?;
        }
        RouteType::DirectProxy { provider, .. } => {
            if !direct_allowed.contains(provider) {
                return Err(ApiError::InvalidRequest(InvalidRequestError::NotFound(
                    req.uri().path().to_string(),
                )));
            }
            check_method(req.method(), path)?;
        }
    }
    Ok(())
}

impl<S> Service<Request> for RoutingPrecheckService<S>
where
    S: Service<Request, Response = Response, Error = ApiError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = ApiError;
    type Future =
        futures::future::Either<std::future::Ready<Result<Self::Response, Self::Error>>, S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        if let Err(e) = precheck(&req, &self.direct_proxy_providers) {
            return futures::future::Either::Left(std::future::ready(Err(e)));
        }
        futures::future::Either::Right(self.inner.call(req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_required_heuristics() {
        assert!(path_requires_post("chat/completions"));
        assert!(path_requires_post("/v1/chat/completions"));
        assert!(path_requires_post("prefix/v1/messages"));
        assert!(path_requires_post("messages"));
        assert!(path_requires_post("completions"));
        assert!(path_requires_post("embeddings"));
        assert!(path_requires_post("images/generations"));
        assert!(path_requires_post("responses"));
        assert!(path_requires_post("v1/responses"));
        assert!(path_requires_post("model/foo/converse"));
        assert!(path_requires_post("v1beta/openai/chat/completions"));
        assert!(!path_requires_post("v1/models"));
        assert!(!path_requires_post("openapi.json"));
    }
}
