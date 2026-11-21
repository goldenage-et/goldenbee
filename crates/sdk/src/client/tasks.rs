//! Task submission, projection, retained events, and cancellation.

use super::transport::{get, post_empty, post_reconciling};
use super::Client;
use common::{
    ApiError, GatewaySubmitRequest, GatewaySubmitResponse, GatewayTask, RunResult, StreamPage,
    MAX_EVENT_PAGE,
};
use std::time::Duration;

impl Client {
    /// Submit to an explicit Worker through Gateway.
    ///
    /// Retries are bounded and always reuse the caller's request body and
    /// idempotency key. A lost acknowledgment therefore reconciles the same
    /// Gateway task instead of creating a fresh request.
    pub async fn submit(
        &self,
        request: &GatewaySubmitRequest,
    ) -> Result<GatewaySubmitResponse, ApiError> {
        post_reconciling(self, "/v1/tasks", request).await
    }

    /// Read a routed task projection.
    pub async fn task(&self, task_id: &str) -> Result<GatewayTask, ApiError> {
        get(self, &format!("/v1/tasks/{task_id}")).await
    }

    /// Read one retained event page. The limit is bounded to the published
    /// 1-1000 range so a caller cannot request an unbounded page.
    pub async fn events(
        &self,
        task_id: &str,
        after: u64,
        limit: usize,
    ) -> Result<StreamPage, ApiError> {
        let limit = limit.clamp(1, MAX_EVENT_PAGE);
        get(
            self,
            &format!("/v1/tasks/{task_id}/events?after={after}&limit={limit}"),
        )
        .await
    }

    /// Cancel a routed task.
    pub async fn cancel(&self, task_id: &str) -> Result<GatewayTask, ApiError> {
        post_empty(self, &format!("/v1/tasks/{task_id}/cancel")).await
    }

    /// Wait for a routed task and project the shared one-shot result.
    ///
    /// This is a bounded projection of the existing task route, not a second
    /// executor. A wait bound that expires returns the truthful non-terminal
    /// projection with the uncertain exit class.
    pub async fn await_run(&self, task_id: &str, timeout: Duration) -> Result<RunResult, ApiError> {
        let started = tokio::time::Instant::now();
        loop {
            let routed = self.task(task_id).await?;
            if routed.task.state.is_terminal() || started.elapsed() >= timeout {
                return Ok(RunResult::from_task(&routed.task));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}
