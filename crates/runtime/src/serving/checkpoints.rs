//! Checkpoint inspection, typed diff preview, and restore routes.
//!
//! Access is task-scoped: the runtime resolves the checkpoint's owning task
//! through the normal task view, so another caller or namespace cannot inspect
//! or rewind it.

use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::{Path, State};
use axum::response::Json;
use axum::Extension;
use common::{
    CheckpointDiff, CheckpointRestoreOutcome, CheckpointRestoreRequest, CheckpointView, Principal,
};

pub(super) async fn task_checkpoints(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<Vec<CheckpointView>> {
    state
        .runtime
        .checkpoints_for_task(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn checkpoint(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(checkpoint): Path<String>,
) -> JsonResult<CheckpointView> {
    state
        .runtime
        .checkpoint(&principal, &checkpoint)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn diff(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(checkpoint): Path<String>,
) -> JsonResult<CheckpointDiff> {
    state
        .runtime
        .checkpoint_diff(&principal, &checkpoint)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn restore(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(checkpoint): Path<String>,
    Json(request): Json<CheckpointRestoreRequest>,
) -> JsonResult<CheckpointRestoreOutcome> {
    state
        .runtime
        .restore_checkpoint(&principal, &checkpoint, &request)
        .await
        .map(Json)
        .map_err(failure)
}
