//! Gateway-projected Worker diagnostics.

use super::worker_client::issue_control_delegation;
use super::Gateway;
use crate::caller::Caller;
use common::{scopes, ApiError, DiagnosticBundle};

impl Gateway {
    /// Read one Worker's redacted diagnostic bundle.
    ///
    /// The caller must be allowed to see the target Worker and hold `task:read`.
    /// The Worker remains the owner of the evidence; the Gateway only forwards
    /// a short-lived delegation and returns the bundle unchanged.
    pub async fn diagnostics(
        &self,
        caller: &Caller,
        worker_id: &str,
        task: Option<&str>,
    ) -> Result<DiagnosticBundle, ApiError> {
        caller.principal.require(scopes::TASK_READ)?;
        if !caller.allows(worker_id) {
            return Err(ApiError::new(
                "scope.worker_denied",
                format!("caller is not authorized for Worker '{worker_id}'"),
            ));
        }
        let worker = self.worker(worker_id)?;
        let token = issue_control_delegation(
            &self.state.secret,
            &worker,
            &caller.principal,
            scopes::TASK_READ,
        )?;
        let task = match task.filter(|task| !task.is_empty()) {
            Some(task) => {
                let worker_task = match self.submission_for(&caller.principal, task).await {
                    Ok(submission) => {
                        if submission.worker != worker_id {
                            return Err(ApiError::new(
                                "route.task.not_found",
                                "routed task is not visible on this Worker",
                            ));
                        }
                        submission.worker_task_id.ok_or_else(|| {
                            ApiError::new(
                                "task.unknown_submission",
                                "the Worker acceptance is not resolved",
                            )
                        })?
                    }
                    // The caller may already hold a Worker task identity from
                    // another explicit management surface. The Worker still
                    // enforces ownership and namespace visibility.
                    Err(_) => task.to_string(),
                };
                Some(worker_task)
            }
            None => None,
        };
        let path = match task {
            Some(task) => format!("/v1/diagnostics?task={task}"),
            None => "/v1/diagnostics".to_string(),
        };
        self.state
            .worker_get::<DiagnosticBundle>(&worker, &path, &token)
            .await
    }
}
