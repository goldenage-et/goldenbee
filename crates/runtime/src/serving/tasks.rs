//! Task, event, and cancellation routes.

use crate::core::crypto::DelegationClaims;
use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::{Path, RawQuery, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Json, Response};
use axum::Extension;
use common::{
    ApiError, Principal, StreamEvent, StreamPage, SubmitTaskRequest, SubmitTaskResponse, Task,
    EVENT_SCHEMA_VERSION, MAX_EVENT_PAGE,
};
use futures::stream::{self, Stream};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

pub(super) async fn list(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<Vec<Task>> {
    state
        .runtime
        .tasks(&principal, None)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn submit(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    claims: Option<Extension<DelegationClaims>>,
    headers: HeaderMap,
    Json(mut request): Json<SubmitTaskRequest>,
) -> JsonResult<SubmitTaskResponse> {
    let trace = headers
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .and_then(common::traceparent_id)
        .or_else(|| {
            headers
                .get("x-goldenbee-trace-id")
                .and_then(|value| value.to_str().ok())
                .map(|value| common::trace_id(Some(value)))
        });
    if let Some(Extension(claims)) = &claims {
        if !claims.capabilities.contains(&request.agent) {
            return Err(failure(ApiError::new(
                "scope.capability_denied",
                "delegation does not grant this capability",
            )));
        }
        request.limits = request.limits.narrower(&claims.limits);
        if request
            .namespace
            .as_deref()
            .is_some_and(|namespace| namespace != claims.namespace)
        {
            return Err(failure(ApiError::new(
                "scope.namespace_denied",
                "delegation is scoped to one namespace",
            )));
        }
    }
    state
        .runtime
        .submit_task_with_trace(
            &principal,
            request,
            claims.as_ref().map(|claims| &claims.0),
            trace.as_deref(),
        )
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn task(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<Task> {
    state
        .runtime
        .task(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}

/// Read-only evidence linking task, attempt, native invocation, and every
/// protected effect operation. The Worker Instance owns the receipts; this
/// route only projects them.
pub(super) async fn operations(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<crate::EffectEvidence> {
    let task = state.runtime.task(&principal, &id).await.map_err(failure)?;
    Ok(Json(
        state
            .runtime
            .effect_evidence(&task.id)
            .await
            .unwrap_or(crate::EffectEvidence {
                task_id: task.id.clone(),
                attempt: task.attempt,
                native_invocation_id: task.native_invocation_id.clone(),
                operations: Vec::new(),
            }),
    ))
}

fn default_limit() -> usize {
    500
}

pub(super) async fn events(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> JsonResult<StreamPage> {
    let after = common::parse_cursor(raw.as_deref()).map_err(failure)?;
    let limit = common::parse_limit(raw.as_deref())
        .map_err(failure)?
        .unwrap_or_else(default_limit)
        .clamp(1, MAX_EVENT_PAGE);
    // Reading the task first enforces caller and namespace scoping before the
    // retained stream is exposed.
    state.runtime.task(&principal, &id).await.map_err(failure)?;
    let newest = state.runtime.latest_seq(&id).await;
    if after > newest {
        return Err(failure(cursor_too_new(after, newest)));
    }
    state
        .runtime
        .events_since(&id, after, limit)
        .await
        .map(Json)
        .map_err(failure)
}

fn cursor_too_new(after: u64, newest: u64) -> ApiError {
    ApiError::new(
        "stream.cursor_invalid",
        "the requested cursor is newer than this task stream",
    )
    .with("after", after.to_string())
    .with("newest_seq", newest.to_string())
}

fn cursor_gap(oldest: u64) -> ApiError {
    ApiError::new(
        "stream.cursor_expired",
        "the requested cursor is older than retained events",
    )
    .with("oldest_seq", oldest.to_string())
}

/// Replay retained events, then follow the live Worker broadcast.
///
/// The receiver is registered before the retained page is read, so a durable
/// event committed during the handoff is either replayed or received live.
/// Ephemeral model deltas are never replayed; that loss is explicit in the
/// envelope's `durable` flag.
pub(super) async fn stream(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Response> {
    let after = common::parse_cursor(raw.as_deref()).map_err(failure)?;
    let task = state.runtime.task(&principal, &id).await.map_err(failure)?;
    // Reserve bounded fan-out capacity before opening the live receiver. The
    // permit is held for the connection and released on drop.
    let slot = state.runtime.stream_slot().map_err(failure)?;
    let newest = state.runtime.latest_seq(&id).await;
    if after > newest {
        return Err(failure(cursor_too_new(after, newest)));
    }
    let receiver = state.runtime.subscribe();
    let mut replay = VecDeque::new();
    let mut cursor = after;
    let connected_terminal = task.state.is_terminal();
    loop {
        let page = state
            .runtime
            .events_since(&id, cursor, MAX_EVENT_PAGE)
            .await
            .map_err(failure)?;
        // A fresh subscription on trimmed history cannot silently pretend to
        // be the whole stream. The retained page reports the oldest sequence
        // so the caller can reload the canonical snapshot and resume.
        if page.cursor_expired || (after == 0 && page.oldest_seq > 1) {
            return Err(failure(cursor_gap(page.oldest_seq)));
        }
        let empty = page.events.is_empty();
        cursor = page.next_seq;
        replay.extend(page.events);
        if empty {
            break;
        }
    }
    if let Some(event) = replay.back() {
        cursor = cursor.max(event.seq);
    }
    let stream_id = format!("task:{id}");
    let drain = state.runtime.drain_token();
    let lag_notice = lag_event(&state, &stream_id, cursor);
    let keepalive = state.runtime.config().stream_keepalive_ms.max(1);
    let live = stream::unfold(
        LiveStream {
            receiver,
            replay,
            cursor,
            stream_id,
            connected_terminal,
            terminal: false,
            drain,
            draining: false,
            lag_notice: Some(lag_notice),
            _slot: slot,
        },
        |mut state| async move {
            loop {
                if let Some(event) = state.replay.pop_front() {
                    state.terminal |= is_terminal_event(&event.kind);
                    return Some((Ok(sse_event(&event)), state));
                }
                if state.terminal || state.connected_terminal {
                    return None;
                }
                if state.drain.is_cancelled() {
                    if state.draining {
                        return None;
                    }
                    state.draining = true;
                    return Some((
                        Ok(Event::default()
                            .event("stream.draining")
                            .data("{\"phase\":\"draining\"}")),
                        state,
                    ));
                }
                let received = tokio::select! {
                    event = state.receiver.recv() => Some(event),
                    _ = state.drain.cancelled() => None,
                };
                match received {
                    Some(Ok(event)) => {
                        if event.stream_id != state.stream_id || event.seq <= state.cursor {
                            continue;
                        }
                        state.cursor = event.seq;
                        state.terminal |= is_terminal_event(&event.kind);
                        return Some((Ok(sse_event(&event)), state));
                    }
                    Some(Err(RecvError::Lagged(_))) => {
                        // A slow viewer must not stall execution. Emit one
                        // explicit notice so the client reconnects from the
                        // last durable sequence it observed, then close.
                        if let Some(notice) = state.lag_notice.take() {
                            return Some((Ok(sse_event(&notice)), state));
                        }
                        return None;
                    }
                    Some(Err(RecvError::Closed)) => return None,
                    None => continue,
                }
            }
        },
    );
    Ok(Sse::new(live).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_millis(keepalive))
            .text("goldenbee"),
    ))
}

struct LiveStream {
    receiver: tokio::sync::broadcast::Receiver<StreamEvent>,
    replay: VecDeque<StreamEvent>,
    cursor: u64,
    stream_id: String,
    connected_terminal: bool,
    terminal: bool,
    drain: tokio_util::sync::CancellationToken,
    draining: bool,
    lag_notice: Option<StreamEvent>,
    _slot: tokio::sync::OwnedSemaphorePermit,
}

fn lag_event(state: &WorkerState, stream_id: &str, cursor: u64) -> StreamEvent {
    StreamEvent {
        event_id: crate::new_id("evt"),
        source: state.runtime.config().instance_id.clone(),
        incarnation: state.runtime.incarnation().to_string(),
        stream_id: stream_id.to_string(),
        seq: cursor,
        schema_version: EVENT_SCHEMA_VERSION,
        kind: "stream.lagged".into(),
        durable: false,
        payload: serde_json::json!({
            "reason": "subscriber_lagged",
            "from_seq": cursor,
            "recovery": "Reconnect from this sequence and replay retained events.",
        }),
        trace_id: None,
        at: common::now_ms(),
    }
}

fn is_terminal_event(kind: &str) -> bool {
    matches!(
        kind,
        "task.succeeded" | "task.failed" | "task.cancelled" | "task.unknown"
    )
}

fn sse_event(event: &StreamEvent) -> Event {
    Event::default()
        .id(event.seq.to_string())
        .event(event.kind.clone())
        .data(serde_json::to_string(event).unwrap_or_else(|_| "{}".into()))
}

pub(super) async fn cancel(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<Task> {
    state
        .runtime
        .cancel_task(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn resume(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<Task> {
    state
        .runtime
        .resume_task(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}
