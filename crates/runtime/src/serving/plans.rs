//! Worker-native Master plan management routes.
//!
//! These routes are the ordinary Worker execution surface for a designated
//! Master. They do not add a Gateway orchestration loop and never bypass the
//! frozen team roster or static peer authorization.

use crate::core::crypto::DelegationClaims;
use crate::master::MasterPlan;
use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::{Path, State};
use axum::response::Json;
use axum::Extension;
use common::{CreatePlanRequest, EscalatePlanRequest, PlanControlRequest, Principal};

pub(super) async fn create(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<CreatePlanRequest>,
) -> JsonResult<MasterPlan> {
    state
        .runtime
        .create_master_plan(&principal, request)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn list(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<Vec<MasterPlan>> {
    state
        .runtime
        .master_plans(&principal)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn plan(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<MasterPlan> {
    state
        .runtime
        .master_plan(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn dispatch(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    claims: Option<Extension<DelegationClaims>>,
    Path(id): Path<String>,
    body: Option<Json<PlanControlRequest>>,
) -> JsonResult<MasterPlan> {
    let control = body.map(|Json(body)| body).unwrap_or_default();
    state
        .runtime
        .dispatch_master_plan(&principal, claims.map(|claims| claims.0), &id, control)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn recover(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    claims: Option<Extension<DelegationClaims>>,
    Path(id): Path<String>,
    body: Option<Json<PlanControlRequest>>,
) -> JsonResult<MasterPlan> {
    let control = body.map(|Json(body)| body).unwrap_or_default();
    state
        .runtime
        .recover_master_plan(&principal, claims.map(|claims| claims.0), &id, control)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn evaluate(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<MasterPlan> {
    state
        .runtime
        .evaluate_master_plan(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn cancel(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<MasterPlan> {
    state
        .runtime
        .cancel_master_plan(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn escalate(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(request): Json<EscalatePlanRequest>,
) -> JsonResult<MasterPlan> {
    state
        .runtime
        .escalate_master_plan(&principal, &id, request.reason)
        .await
        .map(Json)
        .map_err(failure)
}
