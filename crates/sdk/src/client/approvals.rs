//! Worker-owned approval decisions through Gateway.
//!
//! The SDK forwards the exact digest the approver observed. It never infers,
//! refreshes, or mutates a decision locally; the owning Worker is the authority
//! for expiry, authorization, and resume.

use super::transport::{get, post};
use super::Client;
use common::{ApiError, ApprovalDecision, GatewayApproval};

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

fn path(id: &str, approval: &str, suffix: &str) -> String {
    format!(
        "/v1/tasks/{}/approvals/{}{suffix}",
        encode(id),
        encode(approval)
    )
}

impl Client {
    /// List approvals for one routed task. Pending-only is explicit so a Client
    /// cannot mistake historical decisions for outstanding requests.
    pub async fn approvals(
        &self,
        task_id: &str,
        pending_only: bool,
    ) -> Result<Vec<GatewayApproval>, ApiError> {
        get(
            self,
            &format!(
                "/v1/tasks/{}/approvals?pending={pending_only}",
                encode(task_id)
            ),
        )
        .await
    }

    /// Read one exact Worker-owned approval through its Gateway execution id.
    pub async fn approval(
        &self,
        task_id: &str,
        approval_id: &str,
    ) -> Result<GatewayApproval, ApiError> {
        get(self, &path(task_id, approval_id, "")).await
    }

    /// Approve or reject the exact operation digest the approver saw.
    pub async fn decide_approval(
        &self,
        task_id: &str,
        approval_id: &str,
        decision: &ApprovalDecision,
    ) -> Result<GatewayApproval, ApiError> {
        post(self, &path(task_id, approval_id, "/decision"), decision).await
    }

    /// Revoke a pending approval before a decision is recorded.
    pub async fn revoke_approval(
        &self,
        task_id: &str,
        approval_id: &str,
    ) -> Result<GatewayApproval, ApiError> {
        post(
            self,
            &path(task_id, approval_id, "/revoke"),
            &serde_json::json!({}),
        )
        .await
    }
}
