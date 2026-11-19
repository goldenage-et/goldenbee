//! Transport-neutral Worker event stream.
//!
//! The core holds the live replay-then-live Worker stream. Deployment adapters
//! decide how chunks become Server-Sent Events, gRPC frames, or an in-process
//! iterator. No HTTP response type crosses this boundary.

use common::ApiError;
use futures::stream::{BoxStream, Stream, StreamExt};
use std::pin::Pin;
use std::task::{Context, Poll};

/// A live Worker event stream produced by the replay-then-live Worker route.
///
/// Chunks are forwarded unchanged. An error closes the stream; it never
/// rewrites a Worker outcome or resubmits the task.
pub struct EventStream {
    inner: BoxStream<'static, Result<Vec<u8>, ApiError>>,
    _slot: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl EventStream {
    /// Adapt a successful Worker response into a byte stream.
    pub(crate) fn from_worker(
        response: reqwest::Response,
        worker: String,
        slot: tokio::sync::OwnedSemaphorePermit,
    ) -> Self {
        let inner = response
            .bytes_stream()
            .map(move |item| {
                item.map(|bytes| bytes.to_vec()).map_err(|_| {
                    ApiError::retryable(
                        "route.worker_unavailable",
                        format!("Worker '{worker}' ended the event stream unexpectedly"),
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

    /// Consume the stream as a boxed transport-neutral stream.
    pub fn into_inner(self) -> BoxStream<'static, Result<Vec<u8>, ApiError>> {
        self.inner
    }
}

impl Stream for EventStream {
    type Item = Result<Vec<u8>, ApiError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}
