//! Authorized standalone approval routes.
//!
//! The Worker Instance owns the protected operation request and its recorded
//! outcome. These routes expose and decide that state; they never dispatch an
//! effect, and a decision is revalidated against the exact operation digest.

use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::{Path, RawQuery, State};
use axum::response::Json;
use axum::Extension;
use common::{Approval, ApprovalDecision, Principal};

pub(super) async fn list(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    RawQuery(raw): RawQuery,
) -> JsonResult<Vec<Approval>> {
    let pending_only = raw
        .as_deref()
        .unwrap_or_default()
        .split('&')
        .any(|part| matches!(part, "pending=1" | "pending=true"));
    state
        .runtime
        .approvals(&principal, pending_only)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn get(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<Approval> {
    state
        .runtime
        .approval(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn decide(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(decision): Json<ApprovalDecision>,
) -> JsonResult<Approval> {
    state
        .runtime
        .decide_approval(&principal, &id, decision)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn revoke(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<Approval> {
    state
        .runtime
        .revoke_approval(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}
