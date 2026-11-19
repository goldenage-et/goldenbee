//! Operator diagnostic export through Gateway.

use super::transport::get_query;
use super::Client;
use common::{ApiError, DiagnosticBundle};

impl Client {
    /// Read one Worker's redacted diagnostic bundle.
    ///
    /// Passing a task id narrows the bundle to that execution. The same SDK
    /// transport, retry policy, and error decoding are used for every hop.
    pub async fn diagnostics(
        &self,
        worker: &str,
        task: Option<&str>,
    ) -> Result<DiagnosticBundle, ApiError> {
        let mut query = Vec::new();
        if let Some(task) = task.filter(|task| !task.is_empty()) {
            query.push(("task", task));
        }
        get_query(self, &format!("/v1/workers/{worker}/diagnostics"), &query).await
    }

    /// Submit with an explicit W3C trace identity propagated through Gateway
    /// and the target Worker.
    pub async fn submit_traced(
        &self,
        request: &common::GatewaySubmitRequest,
        trace_id: &str,
    ) -> Result<common::GatewaySubmitResponse, ApiError> {
        super::transport::post_reconciling_with_trace(self, "/v1/tasks", request, Some(trace_id))
            .await
    }
}
