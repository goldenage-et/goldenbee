//! Worker discovery, audit, registration, leases, and the legacy delete alias.

use super::types::{AuditQuery, DiscoveryQuery};
use crate::http::{bearer, failure, JsonResult};
use crate::state::SharedState;
use axum::extract::{Path, Query, State as AxumState};
use axum::http::HeaderMap;
use axum::Extension;
use axum::Json;
use common::{
    AdminWorkerSummary, RegistryAuditEntry, WorkerDiscoveryFilter, WorkerLease, WorkerLeaseRequest,
    WorkerRegistrationRequest, WorkerSummary,
};
use gateway_core::Caller;

pub(super) async fn register_worker(
    AxumState(state): AxumState<SharedState>,
    headers: HeaderMap,
    Json(request): Json<WorkerRegistrationRequest>,
) -> JsonResult<WorkerLease> {
    state
        .core
        .register_worker(bearer(&headers).as_deref(), &request)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn renew_worker(
    AxumState(state): AxumState<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<WorkerLeaseRequest>,
) -> JsonResult<WorkerLease> {
    state
        .core
        .renew_worker(bearer(&headers).as_deref(), &id, &request)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn workers(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Query(query): Query<DiscoveryQuery>,
) -> JsonResult<Vec<WorkerSummary>> {
    let filter = WorkerDiscoveryFilter {
        capability: query.capability,
        protocol: query.protocol,
        locality: query.locality,
        health: query.health.filter(|health| health != "any"),
    };
    state
        .core
        .discover(&caller, &filter)
        .map(Json)
        .map_err(failure)
}

pub(super) async fn audit(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Query(query): Query<AuditQuery>,
) -> JsonResult<Vec<RegistryAuditEntry>> {
    state
        .core
        .audit(&caller, query.worker.as_deref())
        .map(Json)
        .map_err(failure)
}

/// Legacy `DELETE /v1/workers/{id}` alias for admin decommission.
pub(super) async fn decommission(
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
