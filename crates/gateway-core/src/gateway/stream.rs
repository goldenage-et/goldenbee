//! Live Worker event stream orchestration.

use crate::caller::Caller;
use crate::store::Connectivity;
use crate::stream::EventStream;
use common::{scopes, ApiError, Limits};

use super::submit::unknown_submission;
use super::worker_client::{decode, issue_worker_delegation, unavailable};
use super::Gateway;

impl Gateway {
    /// Open the Worker's replay-then-live event stream without resubmitting
    /// work. The adapter maps [`EventStream`] onto its own transport.
    pub async fn stream(
        &self,
        caller: &Caller,
        id: &str,
        after: u64,
    ) -> Result<EventStream, ApiError> {
        caller.principal.require(scopes::TASK_READ)?;
        // Reattachment uses only the shared mapping. It never reads the request
        // body and never calls submit.
        let Some(mapping) = self.state.streams.stream_mapping(id).await? else {
            let Some(submission) = self.state.dispatch.submission(id).await? else {
                return Err(ApiError::new(
                    "route.task.not_found",
                    "routed task is not known",
                ));
            };
            if submission.caller != caller.principal.id
                || submission.namespace != caller.principal.namespace
            {
                return Err(ApiError::new(
                    "route.task.not_found",
                    "routed task is not visible",
                ));
            }
            return Err(unknown_submission(
                &submission.execution_id,
                &submission.worker,
                "the Worker acceptance is not resolved",
            ));
        };
        if mapping.caller != caller.principal.id || mapping.namespace != caller.principal.namespace
        {
            return Err(ApiError::new(
                "route.task.not_found",
                "routed task is not visible",
            ));
        }
        let worker = self.worker(&mapping.worker)?;
        let token = issue_worker_delegation(
            &self.state.secret,
            &worker,
            &caller.principal,
            &mapping.agent,
            scopes::TASK_READ,
            &Limits::default(),
        )?;
        // Bound Gateway fan-out independently of Worker execution. The permit
        // moves into the returned stream and is released when the viewer ends.
        let slot = self
            .state
            .stream_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                ApiError::retryable(
                    "stream.subscriber_limit",
                    "this Gateway has reached its bounded number of task streams",
                )
            })?;
        let path = format!(
            "/v1/tasks/{}/events/stream?after={after}",
            mapping.worker_task_id
        );
        let response = self
            .state
            .stream_client
            .get(format!("{}{path}", worker.endpoint))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| unavailable(&worker))?;
        let status = response.status();
        if !status.is_success() {
            let error = match decode::<serde_json::Value>(response, &worker).await {
                Ok(_) => ApiError::new(
                    "route.invalid_worker_response",
                    "Worker stream failed without a typed error",
                ),
                Err(error) => error,
            };
            // A typed cursor or capacity rejection is not a connectivity
            // change and must not rewrite the projection.
            if matches!(error.code.as_str(), "task.not_found" | "stream.not_found") {
                let _ = self
                    .state
                    .dispatch
                    .clear_projection(
                        id,
                        "projection_cleared",
                        "the owning Worker store reports retained history is absent",
                    )
                    .await;
            } else if status.is_server_error() {
                let _ = self
                    .state
                    .dispatch
                    .mark_connectivity(
                        id,
                        Connectivity::Unreachable,
                        "connectivity_unreachable",
                        "connectivity changed; the Worker stream was not opened",
                    )
                    .await;
            }
            return Err(error);
        }
        Ok(EventStream::from_worker(response, worker.id, slot))
    }
}
