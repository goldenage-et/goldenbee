//! Stable wire errors and their HTTP status mapping.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Result returned by the wire contract helpers.
pub type ContractResult<T> = Result<T, ApiError>;

/// Stable, machine-readable error shared by Worker, Gateway, SDK, and CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    /// Stable error code, for example `task.idempotency_conflict`.
    pub code: String,
    /// Human-readable diagnostic. Never contains credentials.
    pub message: String,
    /// Whether a client may safely retry the same request.
    pub retryable: bool,
    /// Actionable recovery guidance.
    pub recovery: String,
    /// Additional redacted, structured detail.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
}

impl ApiError {
    /// Build a non-retryable error.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        Self {
            recovery: recovery_for(&code),
            code,
            message: message.into(),
            retryable: false,
            details: BTreeMap::new(),
        }
    }

    /// Build a retryable error.
    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        let mut error = Self::new(code, message);
        error.retryable = true;
        error
    }

    /// Attach a redacted structured field.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    /// HTTP status selected from the stable error category.
    pub fn status(&self) -> u16 {
        if self.code.starts_with("delegation.") {
            if self.code == "delegation.depth_exceeded" {
                403
            } else {
                401
            }
        } else if self.code.starts_with("auth.") || self.code.starts_with("scope.") {
            if matches!(
                self.code.as_str(),
                "auth.unauthenticated" | "auth.socket_ticket"
            ) {
                401
            } else {
                403
            }
        } else if self.code.starts_with("peer.") || self.code.starts_with("delegation.") {
            if matches!(
                self.code.as_str(),
                "peer.unreachable" | "peer.unavailable" | "peer.transport_failed"
            ) {
                503
            } else if self.code == "peer.saturated" {
                429
            } else if matches!(
                self.code.as_str(),
                "peer.not_found" | "peer.delegation_not_found" | "peer.task_not_found"
            ) {
                404
            } else if self.code == "peer.idempotency_conflict" {
                409
            } else if self.code.ends_with("_denied")
                || self.code.ends_with("_forbidden")
                || self.code == "peer.capability_unavailable"
                || self.code == "delegation.capability_escalation"
                || self.code == "delegation.replayed"
                || self.code == "delegation.expired"
                || self.code == "delegation.depth_exceeded"
                || self.code == "peer.identity_mismatch"
                || self.code == "peer.endpoint_mismatch"
                || self.code == "peer.tls_required"
                || self.code == "peer.remote_not_approved"
            {
                403
            } else {
                400
            }
        } else if (self.code.contains("unavailable") && !self.code.starts_with("host."))
            || self.code == "lifecycle.draining"
            || self.code == "host.unreachable"
            || self.code == "host.operation_unknown"
            || self.code == "host.transport_failed"
        {
            503
        } else if self.code == "host.unauthorized" {
            401
        } else if matches!(
            self.code.as_str(),
            "host.scope_denied"
                | "host.config_forbidden"
                | "host.workspace_forbidden"
                | "host.grant_forbidden"
                | "host.secret_forbidden"
                | "host.operation_forbidden"
        ) {
            403
        } else if self.code == "host.operation_conflict" {
            409
        } else if self.code.ends_with(".not_found") || self.code == "route.not_found" {
            404
        } else if matches!(
            self.code.as_str(),
            "approval.digest_mismatch" | "approval.expired" | "approval.revoked"
        ) {
            // The request conflicts with the recorded authorization outcome,
            // not with request syntax. A fresh exact operation is needed.
            409
        } else if self.code.ends_with(".conflict") || self.code.starts_with("graph.") {
            // Graph checkpoint incompatibility, corruption, and missing state
            // are recoverable conflicts with the exact persisted definition,
            // not fresh bad requests.
            409
        } else if matches!(
            self.code.as_str(),
            "task.idempotency_conflict"
                | "worker.idempotency_conflict"
                | "worker.control_conflict"
                | "worker.revision_conflict"
                | "team.version_conflict"
        ) {
            409
        } else if matches!(
            self.code.as_str(),
            "worker.endpoint_forbidden"
                | "worker.instance_mismatch"
                | "worker.package_mismatch"
                | "worker.capability_mismatch"
                | "worker.descriptor_mismatch"
                | "worker.incarnation_mismatch"
                | "team.member_hidden"
                | "team.capability_unavailable"
        ) {
            403
        } else if matches!(
            self.code.as_str(),
            "worker.duplicate_identity"
                | "worker.decommissioned"
                | "worker.disabled"
                | "worker.endpoint_in_use"
        ) {
            409
        } else if self.code.starts_with("capacity.")
            || self.code.contains("rate_limited")
            || self.code == "delegation.fanout_exceeded"
            || self.code == "stream.subscriber_limit"
        {
            429
        } else if self.code.ends_with(".invalid") || self.code.starts_with("validation.") {
            400
        } else if self.code == "task.unknown_submission" {
            202
        } else {
            400
        }
    }
}

fn recovery_for(code: &str) -> String {
    if code.starts_with("peer.") || code.starts_with("delegation.") {
        if code == "peer.unreachable" {
            "Check the authorized peer endpoint and process health; the existing child binding keeps the remote task id for reconnect."
        } else if code == "peer.saturated" {
            "Wait for the peer's advertised retry window or reduce concurrent child fanout; do not submit a new objective."
        } else if code == "peer.capability_unavailable" || code == "peer.capability_denied" {
            "Request only a capability the operator authorized and the peer advertises, or update the static peer policy."
        } else if code == "delegation.replayed" {
            "Use the original message id to reattach the existing child, or obtain a fresh delegation for a new objective."
        } else if code == "delegation.depth_exceeded" || code == "delegation.fanout_exceeded" {
            "Reduce child depth or fanout to the parent allowance and reuse an existing child binding."
        } else if code == "peer.task_not_cancelable" {
            "Refresh the exact remote task; a terminal task reports its authoritative completed or failed state."
        } else if code == "peer.task_unavailable" || code == "peer.task_not_found" {
            "Recover the delegation by its stored context and task binding before taking another action."
        } else if code.ends_with("_mismatch") || code == "peer.tls_required" {
            "Correct the static peer identity or endpoint policy, then retry before any connection."
        } else {
            "Correct the peer policy or request and retry without creating a second child objective."
        }
        .into()
    } else if code.starts_with("auth.") {
        "Provide a valid bearer token for the target namespace.".into()
    } else if code == "route.worker_required" {
        "Name one authorized Worker Instance explicitly; this Gateway does not choose a target."
            .into()
    } else if code == "route.worker_unavailable" {
        "Check the Worker endpoint and process health, then retry the same explicit target.".into()
    } else if code.starts_with("scope.") {
        "Request a token or grant that includes the required scope.".into()
    } else if code.contains("not_found") {
        "Check the identifier and namespace, then retry with a visible resource.".into()
    } else if code.contains("conflict") {
        "Use the original payload for this idempotency key, or submit with a new key.".into()
    } else if code.starts_with("capacity.") {
        "Wait for the advertised retry interval or drain completed work before retrying.".into()
    } else if code == "lifecycle.draining" {
        "This Worker is draining. Retry against a ready instance after the bounded drain completes."
            .into()
    } else if code == "stream.cursor_expired" {
        "Reload the canonical task snapshot, then subscribe from its last sequence.".into()
    } else if code == "stream.cursor_invalid" {
        "Read the retained event page for this task and subscribe from a sequence it reports, not from another stream."
            .into()
    } else if code == "stream.subscriber_limit" {
        "Close another task stream or retry after it ends; streaming capacity is bounded independently of execution."
            .into()
    } else if code == "task.unknown_submission" {
        "Inspect the task or repeat the original idempotent submission; do not create a new key."
            .into()
    } else if code == "host.unreachable" {
        "Check the host composer process and network reachability, then repeat the same lifecycle operation id."
            .into()
    } else if code == "host.unsupported" {
        "Use a supported operation advertised by this host composer, or perform the action through local operator maintenance."
            .into()
    } else if code == "host.operation_conflict" {
        "Use the original payload for this lifecycle operation id, or choose a new operation id."
            .into()
    } else if code.starts_with("host.") {
        "Correct the host-local selection or scope and retry. Do not assume the lifecycle operation succeeded."
            .into()
    } else if code == "approval.digest_mismatch" {
        "Request a fresh approval for the exact operation digest that will execute.".into()
    } else if code == "auth.socket_ticket" {
        "Request a new short-lived socket ticket through the authenticated HTTP surface; a ticket is one-use and expires quickly.".into()
    } else if code == "socket.scope_mismatch" {
        "Carry the command's actual task or Worker identity in the frame scope and retry.".into()
    } else if code == "socket.invalid_frame" || code == "socket.unsupported_protocol" {
        "Send a schema-version 1 command frame with a non-empty command_id and a known command kind.".into()
    } else if code == "socket.frame_too_large" {
        "Reduce the frame to the published socket size bound; use the HTTP submission route for large inputs.".into()
    } else if code == "capacity.socket_subscriptions" {
        "Close another subscription on this connection or open a second connection; subscription fan-out is bounded per connection.".into()
    } else if code == "capacity.socket_slow_consumer" {
        "Reconnect and resume from the last durable cursor; the previous connection could not drain its bounded queue.".into()
    } else if code == "socket.idle_timeout" {
        "Reconnect with a fresh ticket and resume from the last durable cursor; send periodic ping or command frames to stay live.".into()
    } else if code.starts_with("budget.") {
        "Inspect the durable budget event and partial result, then submit a new task with an authorized budget that fits the work; a stopped run is not automatically resumed as success."
            .into()
    } else {
        "Correct the request and retry. Do not assume the operation was accepted.".into()
    }
}
