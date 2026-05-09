use tower::Layer;

use super::service::WorkspaceConcurrencyService;
use crate::app_state::AppState;

#[derive(Clone)]
pub struct WorkspaceConcurrencyLayer {
    app_state: AppState,
}

impl WorkspaceConcurrencyLayer {
    #[must_use]
    pub fn new(app_state: &AppState) -> Self {
        Self {
            app_state: app_state.clone(),
        }
    }
}

impl<S> Layer<S> for WorkspaceConcurrencyLayer {
    type Service = WorkspaceConcurrencyService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        WorkspaceConcurrencyService::new(inner, &self.app_state)
    }
}
