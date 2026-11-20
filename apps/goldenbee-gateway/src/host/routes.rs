//! Caller-authenticated local package and instance routes.
//!
//! These routes expose the Gateway's own host capability directly. The older
//! `/v1/hosts/{host}` routes remain as a compatibility alias and also let one
//! Gateway proxy lifecycle operations to another Gateway.

use super::HostManager;
use crate::http::{failure, JsonResult};
use crate::state::SharedState;
use axum::extract::{Path, Query, State as AxumState};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use common::{
    scopes, ApiError, HostDescriptor, HostInspection, HostInstanceView, HostOperationOutcome,
    HostProvisionRequest, HostRollbackRequest, HostUpgradeRequest, InstanceLogs,
    PackageInstallOutcome, PackageInstallRequest, PackageList,
};
use gateway_core::Caller;
use serde::Deserialize;
use std::sync::Arc;

pub(crate) fn routes() -> Router<SharedState> {
    Router::new()
        .route("/v1/host", get(local_descriptor))
        .route("/v1/packages", get(list_packages))
        .route("/v1/packages/install", post(install_package))
        .route("/v1/instances", get(list_instances).post(provision))
        .route("/v1/instances/{instance}", get(inspect))
        .route("/v1/instances/{instance}/start", post(start))
        .route("/v1/instances/{instance}/stop", post(stop))
        .route("/v1/instances/{instance}/restart", post(restart))
        .route("/v1/instances/{instance}/upgrade", post(upgrade))
        .route("/v1/instances/{instance}/rollback", post(rollback))
        .route("/v1/instances/{instance}/logs", get(logs))
        .route(
            "/v1/hosts/{host}/packages",
            get(list_packages_for_host).post(install_package_for_host),
        )
        .route(
            "/v1/hosts/{host}/instances/{instance}/start",
            post(start_for_host),
        )
        .route(
            "/v1/hosts/{host}/instances/{instance}/stop",
            post(stop_for_host),
        )
        .route(
            "/v1/hosts/{host}/instances/{instance}/restart",
            post(restart_for_host),
        )
        .route(
            "/v1/hosts/{host}/instances/{instance}/logs",
            get(logs_for_host),
        )
}

fn require(caller: &Caller, admin: bool) -> Result<(), axum::response::Response> {
    let result = if admin {
        caller.principal.require(scopes::INSTANCE_ADMIN)
    } else {
        caller
            .principal
            .require_any(&[scopes::INSTANCE_READ, scopes::INSTANCE_ADMIN])
    };
    result.map_err(failure)
}

fn local_host(state: &SharedState) -> Result<Arc<HostManager>, axum::response::Response> {
    state.local_host.clone().ok_or_else(|| {
        failure(ApiError::new(
            "host.not_configured",
            "this Gateway does not manage a local host",
        ))
    })
}

fn local_host_id(state: &SharedState) -> Result<String, axum::response::Response> {
    state.local_host_id.clone().ok_or_else(|| {
        failure(ApiError::new(
            "host.not_configured",
            "this Gateway does not manage a local host",
        ))
    })
}

fn match_host(state: &SharedState, host: &str) -> Result<(), axum::response::Response> {
    let local = local_host_id(state)?;
    if local == host {
        Ok(())
    } else {
        Err(failure(ApiError::new(
            "host.not_found",
            format!("host '{host}' is not managed by this Gateway"),
        )))
    }
}

fn inspection(
    host: String,
    view: HostInstanceView,
    worker: Option<common::AdminWorkerSummary>,
) -> HostInspection {
    HostInspection { host, view, worker }
}

async fn local_descriptor(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
) -> JsonResult<HostDescriptor> {
    require(&caller, false)?;
    let host = local_host(&state)?;
    Ok(Json(host.descriptor()))
}

async fn list_packages(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
) -> JsonResult<PackageList> {
    require(&caller, false)?;
    let host = local_host(&state)?;
    spawn_blocking(move || host.list_packages()).await.map(Json)
}

async fn list_packages_for_host(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(host): Path<String>,
) -> JsonResult<PackageList> {
    require(&caller, false)?;
    match_host(&state, &host)?;
    let local = local_host(&state)?;
    spawn_blocking(move || local.list_packages())
        .await
        .map(Json)
}

async fn install_package(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Json(request): Json<PackageInstallRequest>,
) -> JsonResult<PackageInstallOutcome> {
    require(&caller, true)?;
    let host = local_host(&state)?;
    host.install_package(request)
        .await
        .map(Json)
        .map_err(failure)
}

async fn install_package_for_host(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(host): Path<String>,
    Json(request): Json<PackageInstallRequest>,
) -> JsonResult<PackageInstallOutcome> {
    require(&caller, true)?;
    match_host(&state, &host)?;
    let local = local_host(&state)?;
    local
        .install_package(request)
        .await
        .map(Json)
        .map_err(failure)
}

async fn list_instances(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
) -> JsonResult<Vec<HostInstanceView>> {
    require(&caller, false)?;
    let host = local_host(&state)?;
    spawn_blocking(move || host.list_instances())
        .await
        .map(Json)
}

async fn provision(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Json(request): Json<HostProvisionRequest>,
) -> JsonResult<HostOperationOutcome> {
    let host = local_host_id(&state)?;
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
    Path(instance): Path<String>,
) -> JsonResult<HostInspection> {
    let host = local_host_id(&state)?;
    state
        .core
        .inspect_instance(&caller, &host, &instance)
        .await
        .map(Json)
        .map_err(failure)
}

async fn start(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(instance): Path<String>,
) -> JsonResult<HostInspection> {
    require(&caller, true)?;
    start_instance(&state, &instance).await
}

async fn start_for_host(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((host, instance)): Path<(String, String)>,
) -> JsonResult<HostInspection> {
    require(&caller, true)?;
    match_host(&state, &host)?;
    start_instance(&state, &instance).await
}

async fn start_instance(state: &SharedState, instance: &str) -> JsonResult<HostInspection> {
    let host = local_host(state)?;
    let host_id = local_host_id(state)?;
    let instance = instance.to_string();
    let view = {
        let instance = instance.clone();
        tokio::task::spawn_blocking(move || host.start_instance(&instance))
            .await
            .map_err(join_failure)?
            .map_err(failure)?
    };
    let worker = state
        .core
        .reconcile_instance_worker(&instance, view.endpoint.as_deref())
        .await
        .map_err(failure)?;
    Ok(Json(inspection(host_id, view, worker)))
}

async fn stop(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(instance): Path<String>,
) -> JsonResult<HostInspection> {
    require(&caller, true)?;
    stop_instance(&state, &instance).await
}

async fn stop_for_host(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((host, instance)): Path<(String, String)>,
) -> JsonResult<HostInspection> {
    require(&caller, true)?;
    match_host(&state, &host)?;
    stop_instance(&state, &instance).await
}

async fn stop_instance(state: &SharedState, instance: &str) -> JsonResult<HostInspection> {
    let host = local_host(state)?;
    let host_id = local_host_id(state)?;
    let instance = instance.to_string();
    let view = {
        let instance = instance.clone();
        tokio::task::spawn_blocking(move || host.stop_instance(&instance))
            .await
            .map_err(join_failure)?
            .map_err(failure)?
    };
    state
        .core
        .mark_instance_unreachable(&instance, "local instance was stopped");
    let worker = state.core.instance_worker(&instance);
    Ok(Json(inspection(host_id, view, worker)))
}

async fn restart(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(instance): Path<String>,
) -> JsonResult<HostInspection> {
    require(&caller, true)?;
    restart_instance(&state, &instance).await
}

async fn restart_for_host(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((host, instance)): Path<(String, String)>,
) -> JsonResult<HostInspection> {
    require(&caller, true)?;
    match_host(&state, &host)?;
    restart_instance(&state, &instance).await
}

async fn restart_instance(state: &SharedState, instance: &str) -> JsonResult<HostInspection> {
    let host = local_host(state)?;
    let host_id = local_host_id(state)?;
    let instance = instance.to_string();
    let view = {
        let instance = instance.clone();
        tokio::task::spawn_blocking(move || host.restart_instance(&instance))
            .await
            .map_err(join_failure)?
            .map_err(failure)?
    };
    let worker = state
        .core
        .reconcile_instance_worker(&instance, view.endpoint.as_deref())
        .await
        .map_err(failure)?;
    Ok(Json(inspection(host_id, view, worker)))
}

async fn upgrade(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(instance): Path<String>,
    Json(request): Json<HostUpgradeRequest>,
) -> JsonResult<HostOperationOutcome> {
    let host = local_host_id(&state)?;
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
    Path(instance): Path<String>,
    Json(request): Json<HostRollbackRequest>,
) -> JsonResult<HostOperationOutcome> {
    let host = local_host_id(&state)?;
    state
        .core
        .rollback_instance(&caller, &host, &instance, request)
        .await
        .map(Json)
        .map_err(failure)
}

#[derive(Debug, Default, Deserialize)]
struct LogQuery {
    #[serde(default)]
    tail: Option<usize>,
}

async fn logs(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(instance): Path<String>,
    Query(query): Query<LogQuery>,
) -> JsonResult<InstanceLogs> {
    require(&caller, false)?;
    logs_instance(&state, &instance, query.tail.unwrap_or(200)).await
}

async fn logs_for_host(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((host, instance)): Path<(String, String)>,
    Query(query): Query<LogQuery>,
) -> JsonResult<InstanceLogs> {
    require(&caller, false)?;
    match_host(&state, &host)?;
    logs_instance(&state, &instance, query.tail.unwrap_or(200)).await
}

async fn logs_instance(
    state: &SharedState,
    instance: &str,
    tail: usize,
) -> JsonResult<InstanceLogs> {
    let host = local_host(state)?;
    let instance = instance.to_string();
    tokio::task::spawn_blocking(move || host.instance_logs(&instance, tail))
        .await
        .map_err(join_failure)?
        .map(Json)
        .map_err(failure)
}

async fn spawn_blocking<T, F>(run: F) -> Result<T, axum::response::Response>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
{
    tokio::task::spawn_blocking(run)
        .await
        .map_err(join_failure)?
        .map_err(failure)
}

fn join_failure(_: tokio::task::JoinError) -> axum::response::Response {
    failure(ApiError::new(
        "host.operation_failed",
        "the local host operation task did not complete",
    ))
}
