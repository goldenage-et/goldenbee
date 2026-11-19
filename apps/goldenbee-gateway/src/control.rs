//! Caller-authenticated interactive-control routes.
//!
//! These handlers forward to `gateway-core`. The Gateway adapter does not keep
//! control leases; generation, expiry, fencing, and audit stay in the Worker.

use crate::http::{failure, JsonResult};
use crate::state::SharedState;
use axum::extract::{Path, State as AxumState};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use common::{
    ControlAuditEntry, ControlCommandRequest, ControlCommandResponse, ControlHandBackRequest,
    ControlLease, ControlLeaseRequest, ControlStatus,
};
use gateway_core::Caller;

pub(crate) fn routes() -> Router<SharedState> {
    Router::new()
        .route(
            "/v1/workers/{worker}/sessions/{session}/control",
            get(status).post(acquire),
        )
        .route(
            "/v1/workers/{worker}/sessions/{session}/control/commands",
            post(command),
        )
        .route(
            "/v1/workers/{worker}/sessions/{session}/control/handback",
            post(hand_back),
        )
        .route(
            "/v1/workers/{worker}/sessions/{session}/control/audit",
            get(audit),
        )
}

async fn acquire(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((worker, session)): Path<(String, String)>,
    Json(request): Json<ControlLeaseRequest>,
) -> JsonResult<ControlLease> {
    if request.session_id != session {
        return Err(failure(common::ApiError::new(
            "control.session_mismatch",
            "the request body session_id must match the authorized path session",
        )));
    }
    state
        .core
        .acquire_control(&caller, &worker, request)
        .await
        .map(Json)
        .map_err(failure)
}

async fn status(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((worker, session)): Path<(String, String)>,
) -> JsonResult<ControlStatus> {
    state
        .core
        .control_status(&caller, &worker, &session)
        .await
        .map(Json)
        .map_err(failure)
}

async fn command(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((worker, session)): Path<(String, String)>,
    Json(request): Json<ControlCommandRequest>,
) -> JsonResult<ControlCommandResponse> {
    if request.session_id != session {
        return Err(failure(common::ApiError::new(
            "control.session_mismatch",
            "the request body session_id must match the authorized path session",
        )));
    }
    state
        .core
        .control_command(&caller, &worker, request)
        .await
        .map(Json)
        .map_err(failure)
}

async fn hand_back(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((worker, session)): Path<(String, String)>,
    Json(request): Json<ControlHandBackRequest>,
) -> JsonResult<ControlLease> {
    if request.session_id != session {
        return Err(failure(common::ApiError::new(
            "control.session_mismatch",
            "the request body session_id must match the authorized path session",
        )));
    }
    state
        .core
        .hand_back_control(&caller, &worker, request)
        .await
        .map(Json)
        .map_err(failure)
}

async fn audit(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((worker, session)): Path<(String, String)>,
) -> JsonResult<Vec<ControlAuditEntry>> {
    state
        .core
        .control_audit(&caller, &worker, &session)
        .await
        .map(Json)
        .map_err(failure)
}
