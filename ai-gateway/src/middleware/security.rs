//! Security plugin middleware layer.
//!
//! This module provides Tower middleware integration for the security plugin system.
//! Plugins are executed as part of the request/response middleware chain.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use bytes::Bytes;
use http::{Request, Response};
use tower::{Layer, Service};

use crate::plugin::{PluginLoader, SecurityContext};

/// Security middleware layer.
///
/// This layer integrates the plugin system into Tower's middleware stack,
/// executing security plugins before and after request processing.
#[derive(Debug, Clone)]
pub struct SecurityLayer {
    loader: Arc<PluginLoader>,
}

impl SecurityLayer {
    /// Create a new security layer from a plugin loader.
    #[must_use]
    pub fn new(loader: PluginLoader) -> Self {
        Self {
            loader: Arc::new(loader),
        }
    }

    /// Create a disabled security layer (passes all requests through).
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            loader: Arc::new(PluginLoader::default()),
        }
    }
}

impl<S> Layer<S> for SecurityLayer {
    type Service = SecurityService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SecurityService {
            inner,
            loader: self.loader.clone(),
        }
    }
}

/// Security middleware service.
#[derive(Debug, Clone)]
pub struct SecurityService<S> {
    inner: S,
    loader: Arc<PluginLoader>,
}

impl<S> SecurityService<S> {
    /// Extract request body for security checking.
    fn extract_body<B>(req: &Request<B>) -> Bytes {
        req.body()
            .map(|b| {
                if let Some(bytes) = b.as_u64_slice() {
                    Bytes::copy_from_slice(bytes)
                } else {
                    Bytes::new()
                }
            })
            .unwrap_or_default()
    }

    /// Build security context from request.
    fn build_context<B>(req: &Request<B>, vk_id: String) -> SecurityContext {
        SecurityContext {
            virtual_key_id: vk_id,
            provider: req
                .uri()
                .path()
                .split('/')
                .nth(2)
                .unwrap_or("unknown")
                .to_string(),
            request_body: Self::extract_body(req),
            workspace_id: None,
        }
    }
}

impl<S, B> Service<Request<B>> for SecurityService<S>
where
    S: Service<Request<B>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&self, req: Request<B>) -> Self::Future {
        // Extract virtual key from headers or use default
        let vk_id = req
            .headers()
            .get("x-virtual-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("default")
            .to_string();

        let ctx = Self::build_context(&req, vk_id);
        let loader = self.loader.clone();

        // Pre-check: validate request against plugins
        if let Err(e) = loader.check_request(&ctx) {
            // Return early with security error response
            let response = Response::builder()
                .status(403)
                .body(Body::from(format!("security check failed: {e}")))
                .unwrap();
            return Box::pin(async { Ok(response) });
        }

        // Call downstream service
        let fut = self.inner.call(req);

        Box::pin(async move {
            let mut res = fut.await?;

            // Post-process: mask response if needed
            // Note: In a full implementation, we'd extract response body here
            // and apply mask_response. For now, we just pass through.

            Ok(res)
        })
    }
}

/// Trait for extending services with security plugins.
pub trait SecurityExt {
    /// Wrap this service with the security plugin layer.
    fn with_security(self, loader: PluginLoader) -> SecurityService<Self>
    where
        Self: Sized;
}

impl<S: Clone> SecurityExt for S {
    fn with_security(self, loader: PluginLoader) -> SecurityService<Self>
    where
        Self: Sized,
    {
        SecurityService {
            inner: self,
            loader: Arc::new(loader),
        }
    }
}
