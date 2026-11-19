//! Authorized retention and principal-erasure lifecycle routes.
//!
//! The owning Worker stores perform the work. These handlers only authenticate
//! the caller and expose the durable deletion report; they never implement a
//! second deletion engine.

use crate::serving::error::JsonResult;
use crate::serving::WorkerState;
use axum::extract::{Path, State};
use axum::response::Json;
use axum::Extension;
use common::Principal;

/// Apply every configured per-owner retention rule.
pub(super) async fn apply(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<crate::LifecycleReport> {
    state
        .runtime
        .apply_retention(&principal)
        .await
        .map(Json)
        .map_err(crate::serving::failure)
}

/// Erase one principal's content and report preserved recovery obligations.
pub(super) async fn erase(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(target): Path<String>,
) -> JsonResult<crate::LifecycleReport> {
    state
        .runtime
        .erase_principal(&principal, &target)
        .await
        .map(Json)
        .map_err(crate::serving::failure)
}
