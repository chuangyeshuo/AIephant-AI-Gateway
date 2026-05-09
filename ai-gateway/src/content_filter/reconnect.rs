use std::{sync::Arc, time::Duration};

use tokio::{sync::RwLock, time::MissedTickBehavior};

use super::ContentFilterGrpcClient;

pub(crate) fn spawn_content_filter_reconnect_task(
    lock: Arc<RwLock<Option<Arc<ContentFilterGrpcClient>>>>,
    endpoint: String,
    interval: Duration,
) {
    let tick = if interval.is_zero() {
        tracing::warn!(
            "policy.grpc-reconnect-interval is zero; using 5s for \
             content-filter reconnect"
        );
        Duration::from_secs(5)
    } else {
        interval
    };

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(tick);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let need_retry = {
                let g = lock.read().await;
                g.is_none()
            };
            if !need_retry {
                continue;
            }
            match ContentFilterGrpcClient::connect(endpoint.clone()).await {
                Ok(client) => {
                    let mut w = lock.write().await;
                    if w.is_none() {
                        *w = Some(Arc::new(client));
                        tracing::info!(
                            %endpoint,
                            "content_filter: policy gRPC client connected after reconnect"
                        );
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        %endpoint,
                        error = %e,
                        "content_filter: policy gRPC reconnect attempt failed"
                    );
                }
            }
        }
    });
}
