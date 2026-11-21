//! Read-only Worker workspace relay.
//!
//! The Gateway verifies caller visibility, issues a narrow Worker delegation,
//! and forwards the bounded Worker projection unchanged. It never reads the
//! filesystem and never stores file content.

use super::worker_client::{decode, issue_control_delegation, unavailable};
use super::Gateway;
use crate::caller::Caller;
use common::{scopes, ApiError, WorkspaceFile, WorkspaceTreePage};
use futures::stream::{BoxStream, Stream, StreamExt};
use std::pin::Pin;
use std::task::{Context, Poll};

/// One bounded relay of Worker workspace change events.
pub struct WorkspaceEventStream {
    inner: BoxStream<'static, Result<Vec<u8>, ApiError>>,
    _slot: tokio::sync::OwnedSemaphorePermit,
}

impl Stream for WorkspaceEventStream {
    type Item = Result<Vec<u8>, ApiError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

/// One bounded raw file relay with response metadata.
pub struct WorkspaceRawContent {
    /// Bounded body stream.
    pub stream: WorkspaceEventStream,
    /// Upstream status: 200 or 206.
    pub status: u16,
    /// Safe Worker-classified content type.
    pub content_type: String,
    /// Byte length of this response body.
    pub content_length: Option<u64>,
    /// Upstream `Content-Range`, when partial.
    pub content_range: Option<String>,
    /// Whole-file digest.
    pub digest: Option<String>,
}

impl Gateway {
    /// List one directory level in a visible Worker workspace.
    pub async fn workspace_tree(
        &self,
        caller: &Caller,
        worker_id: &str,
        path: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<WorkspaceTreePage, ApiError> {
        caller.principal.require(scopes::WORKSPACE_READ)?;
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
            scopes::WORKSPACE_READ,
        )?;
        let mut path = format!(
            "/v1/workspace/tree?path={}&limit={}",
            encode(path),
            limit.clamp(1, 500)
        );
        if let Some(cursor) = cursor.filter(|value| !value.is_empty()) {
            path.push_str("&cursor=");
            path.push_str(&encode(cursor));
        }
        self.state
            .worker_get::<WorkspaceTreePage>(&worker, &path, &token)
            .await
    }

    /// Read one bounded text preview or file metadata from a visible Worker.
    pub async fn workspace_file(
        &self,
        caller: &Caller,
        worker_id: &str,
        path: &str,
    ) -> Result<WorkspaceFile, ApiError> {
        caller.principal.require(scopes::WORKSPACE_READ)?;
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
            scopes::WORKSPACE_READ,
        )?;
        self.state
            .worker_get::<WorkspaceFile>(
                &worker,
                &format!("/v1/workspace/file?path={}", encode(path)),
                &token,
            )
            .await
    }

    /// Open one bounded raw workspace file preview from a visible Worker.
    pub async fn workspace_content(
        &self,
        caller: &Caller,
        worker_id: &str,
        path: &str,
        range: Option<&str>,
    ) -> Result<WorkspaceRawContent, ApiError> {
        caller.principal.require(scopes::WORKSPACE_READ)?;
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
            scopes::WORKSPACE_READ,
        )?;
        let slot = self
            .state
            .stream_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                ApiError::retryable(
                    "capacity.workspace_content",
                    "this Gateway has reached its bounded number of workspace transfers",
                )
            })?;
        let mut request = self
            .state
            .stream_client
            .get(format!(
                "{}/v1/workspace/content?path={}",
                worker.endpoint,
                encode(path)
            ))
            .bearer_auth(&token);
        if let Some(range) = range.filter(|value| !value.is_empty()) {
            request = request.header(reqwest::header::RANGE, range);
        }
        let response = request.send().await.map_err(|_| unavailable(&worker))?;
        let status = response.status();
        if !status.is_success() {
            let error = match decode::<serde_json::Value>(response, &worker).await {
                Ok(_) => ApiError::new(
                    "route.invalid_worker_response",
                    "Worker workspace content failed without a typed error",
                ),
                Err(error) => error,
            };
            return Err(error);
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let content_length = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let content_range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let digest = response
            .headers()
            .get("x-goldenbee-workspace-digest")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let worker_id = worker.id.clone();
        let inner = response
            .bytes_stream()
            .map(move |item| {
                item.map(|bytes| bytes.to_vec()).map_err(|_| {
                    ApiError::retryable(
                        "workspace.transport_unavailable",
                        format!("Worker '{worker_id}' ended the workspace transfer unexpectedly"),
                    )
                })
            })
            .boxed();
        Ok(WorkspaceRawContent {
            stream: WorkspaceEventStream { inner, _slot: slot },
            status: status.as_u16(),
            content_type,
            content_length,
            content_range,
            digest,
        })
    }

    /// Open the Worker workspace change stream for one visible Worker.
    pub async fn workspace_events(
        &self,
        caller: &Caller,
        worker_id: &str,
    ) -> Result<WorkspaceEventStream, ApiError> {
        caller.principal.require(scopes::WORKSPACE_READ)?;
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
            scopes::WORKSPACE_READ,
        )?;
        let slot = self
            .state
            .stream_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                ApiError::retryable(
                    "capacity.workspace_stream",
                    "this Gateway has reached its bounded number of workspace streams",
                )
            })?;
        let response = self
            .state
            .stream_client
            .get(format!("{}/v1/workspace/events/stream", worker.endpoint))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| unavailable(&worker))?;
        if !response.status().is_success() {
            let error = match decode::<serde_json::Value>(response, &worker).await {
                Ok(_) => ApiError::new(
                    "route.invalid_worker_response",
                    "Worker workspace stream failed without a typed error",
                ),
                Err(error) => error,
            };
            return Err(error);
        }
        let worker_id = worker.id.clone();
        let inner = response
            .bytes_stream()
            .map(move |item| {
                item.map(|bytes| bytes.to_vec()).map_err(|_| {
                    ApiError::retryable(
                        "workspace.transport_unavailable",
                        format!("Worker '{worker_id}' ended the workspace stream unexpectedly"),
                    )
                })
            })
            .boxed();
        Ok(WorkspaceEventStream { inner, _slot: slot })
    }
}

fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}
