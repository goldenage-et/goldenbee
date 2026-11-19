//! Small helpers shared by more than one Gateway use case.

use common::{ApiError, WorkerSummary};

use super::Gateway;

impl Gateway {
    pub(super) fn worker(&self, id: &str) -> Result<WorkerSummary, ApiError> {
        self.state.registry.worker(id).ok_or_else(|| {
            ApiError::new(
                "route.worker.not_found",
                format!("Worker '{id}' is not approved by this Gateway"),
            )
        })
    }
}

pub(super) fn normalize(endpoint: &str) -> String {
    endpoint.trim().trim_end_matches('/').to_string()
}
