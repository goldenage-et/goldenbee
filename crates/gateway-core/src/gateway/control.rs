//! Authorized interactive-control routing.
//!
//! The Gateway checks the caller's control scope and Worker allow-list, then
//! forwards the request with a narrow delegation. It never allocates a fencing
//! generation, stores a lease, or decides which controller is current. Those
//! decisions belong to the Worker runtime.

use super::submit::route_worker;
use super::worker_client::issue_control_delegation;
use super::{Gateway, State};
use crate::caller::Caller;
use common::{
    scopes, ApiError, ControlAuditEntry, ControlCommandRequest, ControlCommandResponse,
    ControlHandBackRequest, ControlLease, ControlLeaseRequest, ControlStatus,
};

const CONTROL: &str = scopes::SESSION_CONTROL;

fn worker_path(session: &str, suffix: &str) -> String {
    format!("/v1/sessions/{}{suffix}", encode_segment(session))
}

fn encode_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn validate_session(session: &str) -> Result<(), ApiError> {
    if session.trim().is_empty() || session.len() > 512 {
        return Err(ApiError::new(
            "validation.session_identity",
            "session_id must be 1-512 characters",
        ));
    }
    Ok(())
}

fn token(
    state: &State,
    worker: &common::WorkerSummary,
    caller: &Caller,
) -> Result<String, ApiError> {
    issue_control_delegation(&state.secret, worker, &caller.principal, CONTROL)
}

impl Gateway {
    /// Take exclusive human control of one exact authorized session.
    pub async fn acquire_control(
        &self,
        caller: &Caller,
        worker_id: &str,
        request: ControlLeaseRequest,
    ) -> Result<ControlLease, ApiError> {
        caller.principal.require(CONTROL)?;
        validate_session(&request.session_id)?;
        let worker = route_worker(&self.state, caller, worker_id)?;
        let token = token(&self.state, &worker, caller)?;
        self.state
            .worker_post(
                &worker,
                &worker_path(&request.session_id, "/control"),
                &token,
                &request,
            )
            .await
    }

    /// Current Worker-owned lease without changing ownership.
    pub async fn control_status(
        &self,
        caller: &Caller,
        worker_id: &str,
        session_id: &str,
    ) -> Result<ControlStatus, ApiError> {
        caller.principal.require(CONTROL)?;
        validate_session(session_id)?;
        let worker = route_worker(&self.state, caller, worker_id)?;
        let token = token(&self.state, &worker, caller)?;
        self.state
            .worker_get(&worker, &worker_path(session_id, "/control"), &token)
            .await
    }

    /// Execute one fenced human command on the exact session.
    pub async fn control_command(
        &self,
        caller: &Caller,
        worker_id: &str,
        request: ControlCommandRequest,
    ) -> Result<ControlCommandResponse, ApiError> {
        caller.principal.require(CONTROL)?;
        validate_session(&request.session_id)?;
        let worker = route_worker(&self.state, caller, worker_id)?;
        let token = token(&self.state, &worker, caller)?;
        self.state
            .worker_post(
                &worker,
                &worker_path(&request.session_id, "/control/commands"),
                &token,
                &request,
            )
            .await
    }

    /// Explicitly return ownership to the Worker agent.
    pub async fn hand_back_control(
        &self,
        caller: &Caller,
        worker_id: &str,
        request: ControlHandBackRequest,
    ) -> Result<ControlLease, ApiError> {
        caller.principal.require(CONTROL)?;
        validate_session(&request.session_id)?;
        let worker = route_worker(&self.state, caller, worker_id)?;
        let token = token(&self.state, &worker, caller)?;
        self.state
            .worker_post(
                &worker,
                &worker_path(&request.session_id, "/control/handback"),
                &token,
                &request,
            )
            .await
    }

    /// Redacted control decisions retained by the Worker.
    pub async fn control_audit(
        &self,
        caller: &Caller,
        worker_id: &str,
        session_id: &str,
    ) -> Result<Vec<ControlAuditEntry>, ApiError> {
        caller.principal.require(CONTROL)?;
        validate_session(session_id)?;
        let worker = route_worker(&self.state, caller, worker_id)?;
        let token = token(&self.state, &worker, caller)?;
        self.state
            .worker_get(&worker, &worker_path(session_id, "/control/audit"), &token)
            .await
    }
}
