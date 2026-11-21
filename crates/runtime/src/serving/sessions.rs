//! Session continue, resume, fork, rename, and durable transcript export.
//!
//! Every handler resolves the authenticated `(namespace, principal)` identity
//! through the runtime, which requires `session:read` or `session:write` and
//! the canonical native session store. A session id belonging to another
//! caller or namespace is reported as not found, never disclosed.

use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::{Path, State};
use axum::response::Json;
use axum::Extension;
use common::{Principal, SessionForkRequest, SessionRenameRequest, SessionTranscript, SessionView};

pub(super) async fn list(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<Vec<SessionView>> {
    state
        .runtime
        .session_views(&principal)
        .await
        .map(Json)
        .map_err(failure)
}

/// The session a caller would continue: the newest canonical session.
pub(super) async fn latest(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<SessionView> {
    state
        .runtime
        .latest_session(&principal)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn session(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(session): Path<String>,
) -> JsonResult<SessionView> {
    state
        .runtime
        .session_view(&principal, &session)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn fork(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(session): Path<String>,
    Json(request): Json<SessionForkRequest>,
) -> JsonResult<SessionView> {
    state
        .runtime
        .fork_session(&principal, &session, &request)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn rename(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(session): Path<String>,
    Json(request): Json<SessionRenameRequest>,
) -> JsonResult<SessionView> {
    state
        .runtime
        .rename_session(&principal, &session, &request)
        .await
        .map(Json)
        .map_err(failure)
}

/// Create or refresh the durable transcript export.
pub(super) async fn export(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(session): Path<String>,
) -> JsonResult<SessionTranscript> {
    state
        .runtime
        .export_session(&principal, &session)
        .await
        .map(Json)
        .map_err(failure)
}

/// Read the newest durable transcript export.
pub(super) async fn transcript(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(session): Path<String>,
) -> JsonResult<SessionTranscript> {
    state
        .runtime
        .session_transcript(&principal, &session)
        .await
        .map(Json)
        .map_err(failure)
}
