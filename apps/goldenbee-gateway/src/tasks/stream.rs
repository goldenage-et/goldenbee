//! Server-Sent Event response adapter for Worker streams.

use crate::http::failure;
use crate::state::SharedState;
use axum::body::Body;
use axum::extract::{Path, RawQuery, State as AxumState};
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::Extension;
use common::ApiError;
use futures::StreamExt;
use gateway_core::Caller;

/// Proxy the Worker's replay-then-live SSE stream without resubmitting work.
pub(super) async fn stream_events(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Response, Response> {
    let after = common::parse_cursor(raw.as_deref()).map_err(failure)?;
    let stream = state
        .core
        .stream(&caller, &id, after)
        .await
        .map_err(failure)?;
    let body = Body::from_stream(
        stream.map(|item| item.map_err(|error| std::io::Error::other(error.message))),
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .map_err(|_| {
            failure(ApiError::new(
                "route.stream",
                "the Worker stream could not be forwarded",
            ))
        })
}
