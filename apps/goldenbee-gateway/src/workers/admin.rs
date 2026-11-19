//! Caller-authenticated Worker administration.

use super::handlers::audit;
use crate::http::{failure, JsonResult};
use crate::state::SharedState;
use axum::extract::{Path, State as AxumState};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use common::{AddWorkerRequest, AddWorkerResponse, AdminWorkerSummary, UpdateWorkerRequest};
use gateway_core::Caller;

/// Admin inventory and lifecycle routes. All require the caller `admin` scope,
/// which the control plane enforces.
pub(super) fn routes() -> Router<SharedState> {
    Router::new()
        .route("/v1/admin/workers", get(list).post(add))
        .route("/v1/admin/workers/audit", get(audit))
        .route(
            "/v1/admin/workers/{id}",
            get(worker).patch(update).delete(decommission),
        )
        .route("/v1/admin/workers/{id}/probe", post(probe))
        .route("/v1/admin/workers/{id}/disable", post(disable))
        .route("/v1/admin/workers/{id}/enable", post(enable))
}

async fn list(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
) -> JsonResult<Vec<AdminWorkerSummary>> {
    state.core.admin_workers(&caller).map(Json).map_err(failure)
}

async fn worker(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> JsonResult<AdminWorkerSummary> {
    state
        .core
        .admin_worker(&caller, &id)
        .map(Json)
        .map_err(failure)
}

async fn add(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Json(request): Json<AddWorkerRequest>,
) -> JsonResult<AddWorkerResponse> {
    state
        .core
        .add_worker(&caller, request)
        .await
        .map(Json)
        .map_err(failure)
}

async fn update(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(request): Json<UpdateWorkerRequest>,
) -> JsonResult<AdminWorkerSummary> {
    state
        .core
        .update_worker(&caller, &id, request)
        .await
        .map(Json)
        .map_err(failure)
}

async fn probe(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> JsonResult<AdminWorkerSummary> {
    state
        .core
        .probe_worker(&caller, &id)
        .await
        .map(Json)
        .map_err(failure)
}

async fn disable(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> JsonResult<AdminWorkerSummary> {
    state
        .core
        .disable_worker(&caller, &id)
        .map(Json)
        .map_err(failure)
}

async fn enable(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> JsonResult<AdminWorkerSummary> {
    state
        .core
        .enable_worker(&caller, &id)
        .map(Json)
        .map_err(failure)
}

async fn decommission(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> JsonResult<AdminWorkerSummary> {
    state
        .core
        .decommission(&caller, &id)
        .map(Json)
        .map_err(failure)
}
