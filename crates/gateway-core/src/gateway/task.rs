//! Task projections, retained events, and submission lookup.

use crate::caller::Caller;
use crate::store::{Connectivity, Submission};
use common::{
    scopes, ApiError, GatewayConnectivity, GatewayTask, Limits, Principal, StreamPage, Task,
    WorkerSummary,
};

use super::submit::unknown_submission;
use super::worker_client::issue_worker_delegation;
use super::{Gateway, State};

impl Gateway {
    /// Read the Worker projection for one Gateway external execution id.
    pub async fn task(&self, caller: &Caller, id: &str) -> Result<GatewayTask, ApiError> {
        caller.principal.require(scopes::TASK_READ)?;
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
            scopes::TASK_READ,
            &Limits::default(),
        )?;
        match self
            .state
            .worker_get::<Task>(&worker, &format!("/v1/tasks/{worker_task_id}"), &token)
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
                if worker_store_absent(&error) {
                    let _ = self
                        .state
                        .dispatch
                        .clear_projection(
                            id,
                            "projection_cleared",
                            "the owning Worker store reports the projection is absent",
                        )
                        .await;
                    return Err(error);
                }
                let _ = self
                    .state
                    .dispatch
                    .mark_connectivity(
                        id,
                        Connectivity::Unreachable,
                        "connectivity_unreachable",
                        "connectivity changed; Worker outcome was not rewritten",
                    )
                    .await;
                match submission.projection {
                    Some(projection) => Ok(GatewayTask {
                        task_id: id.to_string(),
                        worker: worker.id,
                        worker_instance: submission.worker_instance,
                        worker_incarnation: submission.worker_incarnation,
                        connectivity: GatewayConnectivity::Unreachable,
                        task: projection,
                    }),
                    None => Err(error),
                }
            }
        }
    }

    /// Read one retained Worker event page.
    pub async fn events(
        &self,
        caller: &Caller,
        id: &str,
        after: u64,
        limit: usize,
    ) -> Result<StreamPage, ApiError> {
        caller.principal.require(scopes::TASK_READ)?;
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
            scopes::TASK_READ,
            &Limits::default(),
        )?;
        let path = format!(
            "/v1/tasks/{worker_task_id}/events?after={after}&limit={}",
            limit.clamp(1, 1_000)
        );
        match self
            .state
            .worker_get::<StreamPage>(&worker, &path, &token)
            .await
        {
            Ok(page) => Ok(page),
            Err(error) => {
                if worker_store_absent(&error) {
                    let _ = self
                        .state
                        .dispatch
                        .clear_projection(
                            id,
                            "projection_cleared",
                            "the owning Worker store reports retained history is absent",
                        )
                        .await;
                    return Err(error);
                }
                let _ = self
                    .state
                    .dispatch
                    .mark_connectivity(
                        id,
                        Connectivity::Unreachable,
                        "connectivity_unreachable",
                        "connectivity changed; retained Worker events were not rewritten",
                    )
                    .await;
                Err(error)
            }
        }
    }
    pub(super) async fn submission_for(
        &self,
        principal: &Principal,
        execution_id: &str,
    ) -> Result<Submission, ApiError> {
        let submission = submission_by_id(&self.state, execution_id)
            .await
            .ok_or_else(|| ApiError::new("route.task.not_found", "routed task is not known"))?;
        if submission.caller != principal.id || submission.namespace != principal.namespace {
            return Err(ApiError::new(
                "route.task.not_found",
                "routed task is not visible",
            ));
        }
        Ok(submission)
    }
    pub(super) async fn refresh_projection(&self, worker: &WorkerSummary, execution_id: &str) {
        let Some(submission) = submission_by_id(&self.state, execution_id).await else {
            return;
        };
        let Some(worker_task_id) = submission.worker_task_id.clone() else {
            return;
        };
        let Ok(token) = issue_worker_delegation(
            &self.state.secret,
            worker,
            &Principal::new(
                submission.caller.clone(),
                submission.namespace.clone(),
                &[scopes::TASK_READ],
            ),
            &submission.agent,
            scopes::TASK_READ,
            &Limits::default(),
        ) else {
            return;
        };
        match self
            .state
            .worker_get::<Task>(worker, &format!("/v1/tasks/{worker_task_id}"), &token)
            .await
        {
            Ok(task) => {
                let observed_state = task.state;
                let _ = self
                    .state
                    .dispatch
                    .record_projection(
                        execution_id,
                        Some(task),
                        Some(observed_state),
                        Connectivity::Online,
                    )
                    .await;
            }
            Err(error) => {
                if worker_store_absent(&error) {
                    let _ = self
                        .state
                        .dispatch
                        .clear_projection(
                            execution_id,
                            "projection_cleared",
                            "the owning Worker store reports the projection is absent",
                        )
                        .await;
                } else {
                    let _ = self
                        .state
                        .dispatch
                        .mark_connectivity(
                            execution_id,
                            Connectivity::Unreachable,
                            "connectivity_unreachable",
                            "connectivity changed; Worker outcome was not rewritten",
                        )
                        .await;
                }
            }
        }
    }
}

fn worker_store_absent(error: &ApiError) -> bool {
    matches!(
        error.code.as_str(),
        "task.not_found" | "stream.not_found" | "artifact.not_found"
    )
}

pub(super) async fn submission_by_id(state: &State, execution_id: &str) -> Option<Submission> {
    state.dispatch.submission(execution_id).await.ok().flatten()
}
