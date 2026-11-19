//! Precise Worker cancellation.

use crate::caller::Caller;
use crate::store::Connectivity;
use common::{scopes, ApiError, GatewayConnectivity, GatewayTask, Limits, Task};

use super::submit::unknown_submission;
use super::worker_client::issue_worker_delegation;
use super::Gateway;

impl Gateway {
    /// Cancel one routed task through the Worker's precise cancellation route.
    pub async fn cancel(&self, caller: &Caller, id: &str) -> Result<GatewayTask, ApiError> {
        caller.principal.require(scopes::TASK_CANCEL)?;
        let submission = self.submission_for(&caller.principal, id).await?;
        let Some(worker_task_id) = submission.worker_task_id.clone() else {
            return Err(unknown_submission(
                &submission.execution_id,
                &submission.worker,
                "the Worker acceptance is not resolved",
            ));
        };
        let worker = self.worker(&submission.worker)?;
        let token = issue_worker_delegation(
            &self.state.secret,
            &worker,
            &caller.principal,
            &submission.agent,
            scopes::TASK_CANCEL,
            &Limits::default(),
        )?;
        match self
            .state
            .worker_post::<Task, _>(
                &worker,
                &format!("/v1/tasks/{worker_task_id}/cancel"),
                &token,
                &serde_json::json!({}),
            )
            .await
        {
            Ok(projection) => {
                let observed_state = projection.state;
                let _ = self
                    .state
                    .dispatch
                    .record_projection(
                        id,
                        Some(projection.clone()),
                        Some(observed_state),
                        Connectivity::Online,
                    )
                    .await;
                Ok(GatewayTask {
                    task_id: id.to_string(),
                    worker: worker.id,
                    worker_instance: submission.worker_instance,
                    worker_incarnation: submission.worker_incarnation,
                    connectivity: GatewayConnectivity::Online,
                    task: projection,
                })
            }
            Err(error) => {
                let _ = self
                    .state
                    .dispatch
                    .mark_connectivity(
                        id,
                        Connectivity::Unreachable,
                        "connectivity_unreachable",
                        "cancellation was not confirmed; the Worker outcome is unchanged",
                    )
                    .await;
                Err(error)
            }
        }
    }
}
