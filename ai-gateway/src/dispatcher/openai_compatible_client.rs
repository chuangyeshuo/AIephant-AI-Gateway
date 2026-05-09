use http::{HeaderMap, HeaderName, HeaderValue};
use reqwest::ClientBuilder;

use crate::{
    app_state::AppState,
    config::providers::UpstreamAuthStyle,
    error::{init::InitError, provider::ProviderError},
    types::{
        provider::{InferenceProvider, ProviderKey},
        secret::Secret,
    },
    utils::host_header,
};

#[derive(Debug, Clone)]
pub struct Client {
    pub(super) inner: reqwest::Client,
    pub(super) upstream_auth: UpstreamAuthStyle,
}

impl Client {
    pub fn new(
        app_state: &AppState,
        client_builder: ClientBuilder,
        provider: InferenceProvider,
        provider_key: Option<&ProviderKey>,
    ) -> Result<Self, InitError> {
        let provider_cfg = app_state
            .0
            .config
            .providers
            .get(&provider)
            .ok_or_else(|| ProviderError::ProviderNotConfigured(provider))?;
        let base_url = provider_cfg.base_url.clone();
        let upstream_auth = provider_cfg.upstream_auth;

        let mut default_headers = HeaderMap::new();
        if let Some(ProviderKey::Secret(key)) = provider_key {
            insert_upstream_auth_header(&mut default_headers, key, upstream_auth);
        }
        default_headers.insert(http::header::HOST, host_header(&base_url));
        default_headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_str(mime::APPLICATION_JSON.essence_str()).unwrap(),
        );
        let inner = client_builder
            .default_headers(default_headers)
            .build()
            .map_err(InitError::CreateReqwestClient)?;
        Ok(Self {
            inner,
            upstream_auth,
        })
    }

    pub fn set_auth_header(
        request_builder: reqwest::RequestBuilder,
        key: &Secret<String>,
        style: UpstreamAuthStyle,
    ) -> reqwest::RequestBuilder {
        match style {
            UpstreamAuthStyle::Bearer => request_builder.header(
                http::header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", key.expose())).unwrap(),
            ),
            UpstreamAuthStyle::ApiKey => request_builder.header(
                HeaderName::from_static("api-key"),
                HeaderValue::from_str(key.expose()).unwrap(),
            ),
        }
    }
}

fn insert_upstream_auth_header(
    headers: &mut HeaderMap,
    key: &Secret<String>,
    style: UpstreamAuthStyle,
) {
    match style {
        UpstreamAuthStyle::Bearer => {
            headers.insert(
                http::header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", key.expose())).unwrap(),
            );
        }
        UpstreamAuthStyle::ApiKey => {
            headers.insert(
                HeaderName::from_static("api-key"),
                HeaderValue::from_str(key.expose()).unwrap(),
            );
        }
    }
}
