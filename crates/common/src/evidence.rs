//! Read-only observability evidence carried by the Worker app-server contract.
//!
//! These types project records the owning Worker already retains. They are not
//! a second store and they never carry credential values or host paths: the
//! composing process redacts before projection.

use crate::task::TaskState;
use serde::{Deserialize, Serialize};

/// One audit record projected for operators and Clients.
///
/// The durable record keeps its original field names; this projection names the
/// decision and the affected resource explicitly so a Client can render why an
/// action was allowed, denied, or asked without parsing prose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Monotonic audit sequence.
    pub seq: u64,
    /// Decision time in Unix milliseconds.
    pub at: i64,
    /// Principal that performed the action.
    pub principal: String,
    /// Stable action name, for example `permission.decision`.
    pub action: String,
    /// Decision or outcome, for example `allow`, `deny`, `apply`, or `ok`.
    pub decision: String,
    /// Redacted reason the decision was made.
    pub reason: String,
    /// Affected resource identity, for example a task or approval id.
    pub resource: String,
    /// Owning process instance.
    pub source: String,
    /// Effective policy/configuration revision.
    pub policy_revision: u64,
    /// Trace correlation, when the record belongs to a traced task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

/// Safe, read-only projection of one protected effect operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationView {
    /// Operation identity.
    pub id: String,
    /// Owning task identity.
    pub task_id: String,
    /// Exact operation digest.
    pub digest: String,
    /// Operation status: `reserved`, `committed`, `absent`, `failed`, or
    /// `unknown`.
    pub status: String,
    /// Dispatch attempts inside this operation identity.
    pub attempt: u32,
    /// Native invocation that dispatched it, when confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_id: Option<String>,
    /// Owning tool.
    pub tool: String,
    /// Stable external idempotency key.
    pub idempotency_key: String,
    /// Policy recorded with the attempt.
    pub policy: String,
    /// Redacted external reference, when the status interface exposes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Redacted receipt or reconciliation detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
    /// Whether the operation reached a terminal effect status.
    pub terminal: bool,
    /// Last update time.
    pub updated_at: i64,
}

/// Public effect evidence linking a task, its attempt, the confirmed native
/// invocation, the task's terminal result, and every protected operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectEvidence {
    /// Public task handle.
    pub task_id: String,
    /// Cross-process trace identity, when the task carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Current or terminal task state.
    pub state: TaskState,
    /// Whether the task reached a terminal state.
    pub terminal: bool,
    /// Current task attempt.
    pub attempt: u32,
    /// Confirmed native invocation, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_invocation_id: Option<String>,
    /// Operations under this task.
    pub operations: Vec<OperationView>,
}
