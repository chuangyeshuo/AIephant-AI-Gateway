//! Router discovery backed by the `providers` / `provider_models` DB tables.
//!
//! On startup it fetches all enabled providers, derives one [`RouterConfig`]
//! per provider, and writes a fresh [`ProvidersConfig`] into [`AppState`].
//! Hot-updates arrive on `rx` from `db_listener`.

use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::Stream;
use pin_project_lite::pin_project;
use rustc_hash::FxHashMap;
use tokio::sync::mpsc::Receiver;
use tokio_stream::wrappers::ReceiverStream;
use tower::discover::Change;
use tracing::{error, info};

use super::provider_db_config::build_from_db;
use crate::{
    app_state::AppState, discover::ServiceMap, error::init::InitError,
    router::service::Router, types::router::RouterId,
};

pin_project! {
    /// Reads available routers derived from the `providers` / `provider_models`
    /// DB tables.
    ///
    /// The `initial` stream emits one `Change::Insert` per router discovered
    /// at startup; subsequent hot-updates arrive via `events`.
    #[derive(Debug)]
    pub struct ProviderDbDiscovery {
        #[pin]
        initial: ServiceMap<RouterId, Router>,
        #[pin]
        events: ReceiverStream<Change<RouterId, Router>>,
    }
}

impl ProviderDbDiscovery {
    pub async fn new(
        app_state: &AppState,
        rx: Receiver<Change<RouterId, Router>>,
    ) -> Result<Self, InitError> {
        let router_store = app_state
            .0
            .router_store
            .as_ref()
            .ok_or(InitError::StoreNotConfigured("router_store"))?;

        let db_providers = router_store
            .get_all_providers_for_gateway()
            .await
            .map_err(|e| {
            InitError::InitRouters(format!(
                "failed to load providers from DB: {e}"
            ))
        })?;

        let db_models = router_store
            .get_all_provider_models_for_gateway()
            .await
            .map_err(|e| {
                InitError::InitRouters(format!(
                    "failed to load provider_models from DB: {e}"
                ))
            })?;

        let (providers_config, router_configs, bare_model_expand) =
            build_from_db(&db_providers, &db_models);

        info!(
            providers = providers_config.len(),
            routers = router_configs.len(),
            "ProviderDbDiscovery: loaded from DB"
        );

        let flags: FxHashMap<String, bool> = db_providers
            .iter()
            .map(|p| (p.code.clone(), p.is_router))
            .collect();
        app_state.set_provider_is_router_flags(flags);

        // Publish the fresh ProvidersConfig into AppState so all request
        // handlers see DB-derived data instead of the YAML defaults.
        app_state.set_providers_config(providers_config);
        app_state.set_bare_model_expand_index(bare_model_expand);

        // Pre-load workspace provider allowlist (F-10).
        match router_store.get_workspace_provider_allowlist().await {
            Ok(allowlist) => {
                info!(
                    workspaces = allowlist.len(),
                    "ProviderDbDiscovery: loaded workspace provider allowlist"
                );
                app_state.set_workspace_provider_allowlist(allowlist);
            }
            Err(e) => {
                error!(
                    error = %e,
                    "ProviderDbDiscovery: failed to load workspace provider allowlist, \
                     proceeding with empty allowlist (all providers allowed)"
                );
            }
        }

        // Cloud mode: router_organization_map left empty (no organization_id
        // in the providers table).  Access control uses virtual_keys directly.

        // Build the initial ServiceMap from derived router configs.
        let mut service_map = std::collections::HashMap::new();
        for (router_id, router_config) in router_configs {
            match Router::new(
                router_id.clone(),
                Arc::new(router_config),
                app_state.clone(),
            )
            .await
            {
                Ok(router) => {
                    service_map.insert(router_id, router);
                }
                Err(e) => {
                    error!(
                        %router_id,
                        error = %e,
                        "ProviderDbDiscovery: failed to build router, skipping"
                    );
                }
            }
        }

        Ok(Self {
            initial: ServiceMap::new(service_map),
            events: ReceiverStream::new(rx),
        })
    }

    /// Compat mode: seed routers from `config.routers` instead of PostgreSQL.
    pub async fn without_database(
        app_state: &AppState,
        rx: Receiver<Change<RouterId, Router>>,
    ) -> Result<Self, InitError> {
        let mut service_map = HashMap::new();

        for (router_id, router_config) in app_state.config().routers.as_ref() {
            match Router::new(
                router_id.clone(),
                Arc::new(router_config.clone()),
                app_state.clone(),
            )
            .await
            {
                Ok(router) => {
                    service_map.insert(router_id.clone(), router);
                }
                Err(e) => {
                    error!(
                        %router_id,
                        error = %e,
                        "ProviderDbDiscovery: failed to build compat router, skipping"
                    );
                }
            }
        }

        Ok(Self {
            initial: ServiceMap::new(service_map),
            events: ReceiverStream::new(rx),
        })
    }
}

impl Stream for ProviderDbDiscovery {
    type Item = Change<RouterId, Router>;

    fn poll_next(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        if let Poll::Ready(Some(change)) = this.initial.as_mut().poll_next(ctx)
        {
            return handle_change(change);
        }
        match this.events.as_mut().poll_next(ctx) {
            Poll::Ready(Some(change)) => handle_change(change),
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
        }
    }
}

fn handle_change(
    change: Change<RouterId, Router>,
) -> Poll<Option<Change<RouterId, Router>>> {
    match change {
        Change::Insert(key, service) => {
            tracing::debug!(%key, "ProviderDbDiscovery: router inserted");
            Poll::Ready(Some(Change::Insert(key, service)))
        }
        Change::Remove(key) => {
            tracing::debug!(%key, "ProviderDbDiscovery: router removed");
            Poll::Ready(Some(Change::Remove(key)))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use compact_str::CompactString;
    use futures::StreamExt;
    use tokio::sync::mpsc;

    use super::*;

    #[tokio::test]
    async fn stream_forwards_remove_events_from_rx() {
        let (tx, rx) = mpsc::channel(1);
        let router_id = RouterId::Named(CompactString::new("openai"));
        tx.send(Change::Remove(router_id.clone()))
            .await
            .expect("send remove event");
        drop(tx);

        let mut discovery = ProviderDbDiscovery {
            initial: ServiceMap::new(HashMap::new()),
            events: ReceiverStream::new(rx),
        };

        let first = discovery
            .next()
            .await
            .expect("stream should yield first event");
        match first {
            Change::Remove(id) => assert_eq!(id, router_id),
            Change::Insert(_, _) => panic!("expected remove event"),
        }

        assert!(
            discovery.next().await.is_none(),
            "stream should end once rx closes"
        );
    }
}
