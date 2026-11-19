//! Authorized artifact metadata and bounded, digest-preserving relay.
//!
//! The Gateway never stores artifact bytes and never re-implements the Worker
//! artifact service. It maps one caller-visible task to the owning Worker,
//! issues a narrow delegation, and forwards the Worker's immutable metadata or
//! 64 KiB content chunks unchanged.

use crate::caller::Caller;
use crate::store::Connectivity;
use common::{scopes, ApiError, Artifact, Limits};
use futures::stream::{BoxStream, Stream, StreamExt};
use std::pin::Pin;
use std::task::{Context, Poll};

use super::submit::unknown_submission;
use super::worker_client::{decode, issue_worker_delegation_scopes, unavailable};
use super::{Gateway, State};

/// One authorized artifact plus its open, bounded content transfer.
pub struct ArtifactContent {
    /// Immutable metadata used to verify the transfer.
    pub artifact: Artifact,
    /// Worker content chunks, bounded by the Worker artifact ceiling.
    pub content: ArtifactStream,
}

/// A continuation of one Worker artifact transfer.
///
/// Dropping the stream closes the Worker connection and releases the Gateway
/// fan-out permit. It never cancels the task that produced the artifact.
pub struct ArtifactStream {
    inner: BoxStream<'static, Result<Vec<u8>, ApiError>>,
    _slot: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl ArtifactStream {
    fn from_worker(
        response: reqwest::Response,
        worker: String,
        slot: tokio::sync::OwnedSemaphorePermit,
    ) -> Self {
        let inner = response
            .bytes_stream()
            .map(move |item| {
                item.map(|bytes| bytes.to_vec()).map_err(|_| {
                    ApiError::retryable(
                        "artifact.transfer_interrupted",
                        format!("Worker '{worker}' ended the artifact transfer unexpectedly"),
                    )
                    .with("worker", worker.clone())
                })
            })
            .boxed();
        Self {
            inner,
            _slot: Some(slot),
        }
    }
}

impl Stream for ArtifactStream {
    type Item = Result<Vec<u8>, ApiError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

impl Gateway {
    /// List immutable artifacts belonging to one routed task.
    pub async fn artifacts(&self, caller: &Caller, id: &str) -> Result<Vec<Artifact>, ApiError> {
        caller.principal.require(scopes::ARTIFACT_READ)?;
        let (worker, worker_task_id, agent) = self.artifact_target(caller, id).await?;
        let token = artifact_delegation(&self.state, &worker, caller, &agent)?;
        let artifacts = match self
            .state
            .worker_get::<Vec<Artifact>>(
                &worker,
                &format!("/v1/tasks/{worker_task_id}/artifacts"),
                &token,
            )
            .await
        {
            Ok(artifacts) => artifacts,
            Err(error) => return Err(artifact_error(&self.state, id, error).await),
        };
        // The Worker is authoritative for ownership. The Gateway still rejects
        // a foreign task reference rather than presenting another task's
        // artifact under this route.
        if artifacts
            .iter()
            .any(|artifact| artifact.task_id != worker_task_id)
        {
            return Err(ApiError::new(
                "artifact.not_found",
                "artifact is not visible for this routed task",
            ));
        }
        Ok(artifacts)
    }

    /// Read immutable metadata for one routed task artifact.
    pub async fn artifact(
        &self,
        caller: &Caller,
        id: &str,
        artifact_id: &str,
    ) -> Result<Artifact, ApiError> {
        caller.principal.require(scopes::ARTIFACT_READ)?;
        let (worker, worker_task_id, agent) = self.artifact_target(caller, id).await?;
        let token = artifact_delegation(&self.state, &worker, caller, &agent)?;
        let artifact = match self
            .state
            .worker_get::<Artifact>(&worker, &format!("/v1/artifacts/{artifact_id}"), &token)
            .await
        {
            Ok(artifact) => artifact,
            Err(error) => return Err(artifact_error(&self.state, id, error).await),
        };
        if artifact.task_id != worker_task_id {
            return Err(ApiError::new(
                "artifact.not_found",
                "artifact is not visible for this routed task",
            ));
        }
        Ok(artifact)
    }

    /// Open one digest-verified Worker artifact transfer.
    ///
    /// Metadata is read first so the adapter can publish the expected digest
    /// and content type. Content is then forwarded as a bounded stream; no
    /// bytes are buffered by the Gateway.
    pub async fn artifact_content(
        &self,
        caller: &Caller,
        id: &str,
        artifact_id: &str,
    ) -> Result<ArtifactContent, ApiError> {
        let artifact = self.artifact(caller, id, artifact_id).await?;
        let (worker, _, agent) = self.artifact_target(caller, id).await?;
        let token = artifact_delegation(&self.state, &worker, caller, &agent)?;
        // Bound concurrent transfers independently of Worker execution. The
        // permit is held by the returned stream and released on disconnect.
        let slot = self
            .state
            .stream_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                ApiError::retryable(
                    "capacity.artifact_transfer",
                    "this Gateway has reached its bounded number of artifact transfers",
                )
            })?;
        let response = self
            .state
            .stream_client
            .get(format!(
                "{}/v1/artifacts/{artifact_id}/content",
                worker.endpoint
            ))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| unavailable(&worker))?;
        let status = response.status();
        if !status.is_success() {
            let error = match decode::<serde_json::Value>(response, &worker).await {
                Ok(_) => ApiError::new(
                    "route.invalid_worker_response",
                    "Worker artifact transfer failed without a typed error",
                ),
                Err(error) => error,
            };
            return Err(artifact_error(&self.state, id, error).await);
        }
        Ok(ArtifactContent {
            artifact,
            content: ArtifactStream::from_worker(response, worker.id, slot),
        })
    }

    async fn artifact_target(
        &self,
        caller: &Caller,
        id: &str,
    ) -> Result<(common::WorkerSummary, String, String), ApiError> {
        let submission = self.submission_for(&caller.principal, id).await?;
        let Some(worker_task_id) = submission.worker_task_id.clone() else {
            return Err(unknown_submission(
                &submission.execution_id,
                &submission.worker,
                "the Worker acceptance is not resolved",
            ));
        };
        Ok((
            self.worker(&submission.worker)?,
            worker_task_id,
            submission.agent,
        ))
    }
}

fn artifact_delegation(
    state: &State,
    worker: &common::WorkerSummary,
    caller: &Caller,
    agent: &str,
) -> Result<String, ApiError> {
    issue_worker_delegation_scopes(
        &state.secret,
        worker,
        &caller.principal,
        agent,
        &[scopes::TASK_READ, scopes::ARTIFACT_READ],
        &Limits::default(),
    )
}

async fn artifact_error(state: &State, id: &str, error: ApiError) -> ApiError {
    // A typed Worker rejection is authoritative. A connectivity failure is
    // recorded separately and never rewrites the task outcome.
    if matches!(
        error.code.as_str(),
        "route.worker_unavailable" | "sdk.transport"
    ) {
        let _ = state
            .dispatch
            .mark_connectivity(
                id,
                Connectivity::Unreachable,
                "connectivity_unreachable",
                "artifact metadata or content could not be read from the Worker",
            )
            .await;
    }
    error
}
