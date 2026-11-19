//! Authorized fleet lifecycle routes over Gateway host composers.
//!
//! These routes require an authenticated caller and the `instance:admin`
//! scope enforced by the control plane. No route accepts a host endpoint, host
//! credential, filesystem path, or arbitrary executable.

use crate::http::{failure, JsonResult};
use crate::state::SharedState;
use axum::extract::{Path, State as AxumState};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use common::{
    HostDescriptor, HostInspection, HostOperationOutcome, HostProvisionRequest,
    HostRollbackRequest, HostUpgradeRequest,
};
use gateway_core::Caller;

pub(crate) fn routes() -> Router<SharedState> {
    Router::new()
        .route("/v1/hosts", get(hosts))
        .route("/v1/hosts/{host}", get(host))
        .route("/v1/hosts/{host}/instances", post(provision))
        .route("/v1/hosts/{host}/instances/{instance}", get(inspect))
        .route(
            "/v1/hosts/{host}/instances/{instance}/upgrade",
            post(upgrade),
        )
        .route(
            "/v1/hosts/{host}/instances/{instance}/rollback",
            post(rollback),
        )
}

async fn hosts(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
) -> JsonResult<Vec<HostDescriptor>> {
    state.core.hosts(&caller).await.map(Json).map_err(failure)
}

async fn host(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(host): Path<String>,
) -> JsonResult<HostDescriptor> {
    state
        .core
        .host(&caller, &host)
        .await
        .map(Json)
        .map_err(failure)
}

async fn provision(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(host): Path<String>,
    Json(request): Json<HostProvisionRequest>,
) -> JsonResult<HostOperationOutcome> {
    state
        .core
        .provision_instance(&caller, &host, request)
        .await
        .map(Json)
        .map_err(failure)
}

async fn inspect(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((host, instance)): Path<(String, String)>,
) -> JsonResult<HostInspection> {
    state
        .core
        .inspect_instance(&caller, &host, &instance)
        .await
        .map(Json)
        .map_err(failure)
}

async fn upgrade(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((host, instance)): Path<(String, String)>,
    Json(request): Json<HostUpgradeRequest>,
) -> JsonResult<HostOperationOutcome> {
    state
        .core
        .upgrade_instance(&caller, &host, &instance, request)
        .await
        .map(Json)
        .map_err(failure)
}

async fn rollback(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((host, instance)): Path<(String, String)>,
    Json(request): Json<HostRollbackRequest>,
) -> JsonResult<HostOperationOutcome> {
    state
        .core
        .rollback_instance(&caller, &host, &instance, request)
        .await
        .map(Json)
        .map_err(failure)
}
