//! Fenced interactive-control commands through Gateway.
//!
//! The SDK carries the Worker-issued epoch and generation unchanged. It never
//! allocates, extends, or validates a lease locally; a stale value is rejected
//! by the Worker that owns the session.

use super::transport::{get, post};
use super::Client;
use common::{
    ApiError, ControlAuditEntry, ControlCommandRequest, ControlCommandResponse,
    ControlHandBackRequest, ControlLease, ControlLeaseRequest, ControlStatus,
};

fn path(worker: &str, session: &str, suffix: &str) -> String {
    format!(
        "/v1/workers/{}/sessions/{}/control{}",
        encode(worker),
        encode(session),
        suffix
    )
}

fn encode(value: &str) -> String {
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

impl Client {
    /// Take exclusive human control of one exact authorized session.
    pub async fn acquire_control(
        &self,
        worker: &str,
        request: &ControlLeaseRequest,
    ) -> Result<ControlLease, ApiError> {
        post(self, &path(worker, &request.session_id, ""), request).await
    }

    /// Read the Worker-owned lease without changing ownership.
    pub async fn control_status(
        &self,
        worker: &str,
        session_id: &str,
    ) -> Result<ControlStatus, ApiError> {
        get(self, &path(worker, session_id, "")).await
    }

    /// Execute one fenced command on the exact session.
    pub async fn control_command(
        &self,
        worker: &str,
        request: &ControlCommandRequest,
    ) -> Result<ControlCommandResponse, ApiError> {
        post(
            self,
            &path(worker, &request.session_id, "/commands"),
            request,
        )
        .await
    }

    /// Explicitly return control to the Worker agent.
    pub async fn hand_back_control(
        &self,
        worker: &str,
        request: &ControlHandBackRequest,
    ) -> Result<ControlLease, ApiError> {
        post(
            self,
            &path(worker, &request.session_id, "/handback"),
            request,
        )
        .await
    }

    /// Retained redacted control decisions.
    pub async fn control_audit(
        &self,
        worker: &str,
        session_id: &str,
    ) -> Result<Vec<ControlAuditEntry>, ApiError> {
        get(self, &path(worker, session_id, "/audit")).await
    }
}
