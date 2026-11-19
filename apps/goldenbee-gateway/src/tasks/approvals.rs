//! Caller-authenticated approval routes.
//!
//! The Gateway authenticates the caller, checks visibility, and forwards to the
//! Worker that owns the approval. Handler state stays in that Worker; these
//! routes only project the Worker record with the Gateway execution identity.

use crate::http::{failure, JsonResult};
use crate::state::SharedState;
use axum::extract::{Path, RawQuery, State as AxumState};
use axum::{Extension, Json};
use common::{ApprovalDecision, GatewayApproval};
use gateway_core::Caller;

pub(crate) async fn list(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> JsonResult<Vec<GatewayApproval>> {
    let pending_only = raw
        .as_deref()
        .unwrap_or_default()
        .split('&')
        .any(|part| matches!(part, "pending=1" | "pending=true"));
    state
        .core
        .approvals(&caller, &id, pending_only)
        .await
        .map(Json)
        .map_err(failure)
}

pub(crate) async fn get(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((id, approval)): Path<(String, String)>,
) -> JsonResult<GatewayApproval> {
    state
        .core
        .approval(&caller, &id, &approval)
        .await
        .map(Json)
        .map_err(failure)
}

pub(crate) async fn decide(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((id, approval)): Path<(String, String)>,
    Json(decision): Json<ApprovalDecision>,
) -> JsonResult<GatewayApproval> {
    state
        .core
        .decide_approval(&caller, &id, &approval, decision)
        .await
        .map(Json)
        .map_err(failure)
}

pub(crate) async fn revoke(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((id, approval)): Path<(String, String)>,
) -> JsonResult<GatewayApproval> {
    state
        .core
        .revoke_approval(&caller, &id, &approval)
        .await
        .map(Json)
        .map_err(failure)
}
