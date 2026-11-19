//! Fenced human control routes over the Worker-owned browser capability.
//!
//! The Worker runtime is the only lease authority. These handlers resolve the
//! exact authenticated `(namespace, principal, session)` identity, require the
//! control scope, and delegate to the browser controller. They never allocate a
//! generation or accept a lease from a caller.

use crate::browser::{BrowserController, SessionKey};
use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::{Path, State};
use axum::response::Json;
use axum::Extension;
use common::{
    scopes, ApiError, ControlAuditEntry, ControlCommandRequest, ControlCommandResponse,
    ControlHandBackRequest, ControlLease, ControlLeaseRequest, ControlStatus, Principal,
};
use std::sync::Arc;

fn controller(state: &WorkerState) -> Result<Arc<BrowserController>, ApiError> {
    state.browser.clone().ok_or_else(|| {
        ApiError::new(
            "browser.not_installed",
            "this Worker Instance has no package-declared browser capability",
        )
    })
}

fn key(principal: &Principal, session: &str) -> Result<SessionKey, ApiError> {
    if session.trim().is_empty() || session.len() > 512 {
        return Err(ApiError::new(
            "validation.session_identity",
            "session_id must be 1-512 characters",
        ));
    }
    let user = crate::native::native_user_id(&principal.namespace, &principal.id)?;
    Ok(SessionKey {
        user,
        session: session.to_string(),
    })
}

fn exact_session(request: &str, path: &str) -> Result<(), ApiError> {
    if request == path {
        Ok(())
    } else {
        Err(ApiError::new(
            "control.session_mismatch",
            "the request body session_id must match the authorized path session",
        ))
    }
}

pub(super) async fn acquire(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(session): Path<String>,
    Json(request): Json<ControlLeaseRequest>,
) -> JsonResult<ControlLease> {
    principal
        .require(scopes::SESSION_CONTROL)
        .map_err(failure)?;
    exact_session(&request.session_id, &session).map_err(failure)?;
    let controller = controller(&state).map_err(failure)?;
    controller
        .acquire_control(
            key(&principal, &session).map_err(failure)?,
            principal.id.clone(),
            request.ttl_ms,
        )
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn status(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(session): Path<String>,
) -> JsonResult<ControlStatus> {
    principal
        .require(scopes::SESSION_CONTROL)
        .map_err(failure)?;
    let controller = controller(&state).map_err(failure)?;
    controller
        .control_status(&key(&principal, &session).map_err(failure)?)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn command(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(session): Path<String>,
    Json(request): Json<ControlCommandRequest>,
) -> JsonResult<ControlCommandResponse> {
    principal
        .require(scopes::SESSION_CONTROL)
        .map_err(failure)?;
    exact_session(&request.session_id, &session).map_err(failure)?;
    let controller = controller(&state).map_err(failure)?;
    controller
        .control_command(
            key(&principal, &session).map_err(failure)?,
            &principal.id,
            request,
        )
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn hand_back(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(session): Path<String>,
    Json(request): Json<ControlHandBackRequest>,
) -> JsonResult<ControlLease> {
    principal
        .require(scopes::SESSION_CONTROL)
        .map_err(failure)?;
    exact_session(&request.session_id, &session).map_err(failure)?;
    let controller = controller(&state).map_err(failure)?;
    controller
        .hand_back_control(
            &key(&principal, &session).map_err(failure)?,
            &principal.id,
            &request.epoch,
            request.generation,
        )
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn audit(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(session): Path<String>,
) -> JsonResult<Vec<ControlAuditEntry>> {
    principal
        .require(scopes::SESSION_CONTROL)
        .map_err(failure)?;
    let controller = controller(&state).map_err(failure)?;
    let entries = controller
        .control_audit(&key(&principal, &session).map_err(failure)?)
        .await;
    Ok(Json(entries))
}
