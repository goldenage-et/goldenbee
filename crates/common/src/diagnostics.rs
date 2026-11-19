//! Operator-facing diagnostic composition.
//!
//! A diagnostic bundle is composed from evidence the owning process already
//! retains: status, bounded metrics, redacted audit/trace samples, task
//! projection, delegation bindings, and pending approvals. It is not a second
//! execution authority and it never includes raw prompt content, credentials,
//! or another instance's filesystem paths.

use crate::health::Health;
use crate::task::TaskState;
use crate::telemetry::MetricSample;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One actionable condition found while composing a diagnostic bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticFinding {
    /// Stable finding kind, for example `unreachable_child`, `blocked_approval`,
    /// `hook_failure`, `native_error`, `degraded_storage`, or `recovery`.
    pub kind: String,
    /// Owning subsystem.
    pub source: String,
    /// Stable code or label.
    pub code: String,
    /// Redacted explanation.
    pub message: String,
    /// Actionable next step.
    pub action: String,
    /// Correlation for the affected task or delegation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

/// Correlation for one task execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiagnosticCorrelation {
    /// Trace identity propagated across process hops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Harness task identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Attempt number.
    #[serde(default)]
    pub attempt: u32,
    /// Native invocation identity, when confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_invocation_id: Option<String>,
}

/// One observed child delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticDelegation {
    /// Local binding identity.
    pub id: String,
    /// Remote peer identity.
    pub peer: String,
    /// Local lifecycle.
    pub state: String,
    /// Remote A2A task identity, when confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_task_id: Option<String>,
    /// Propagated trace identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Last redacted error code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Last redacted error message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Blocked approval summary. The exact target text and any path stay on the
/// owning approval record; the bundle exposes only identity and decision data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticApproval {
    /// Approval identity.
    pub id: String,
    /// Operation name.
    pub operation: String,
    /// Exact operation digest.
    pub digest: String,
    /// Current status.
    pub status: String,
    /// Risk note.
    pub risk: String,
    /// Expiry time in milliseconds.
    pub expires_at: i64,
    /// Creation time.
    pub created_at: i64,
}

/// Task projection included without its prompt text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticTask {
    /// Harness task identity.
    pub task_id: String,
    /// Management state.
    pub state: TaskState,
    /// Attempt number.
    pub attempt: u32,
    /// Trace identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Frozen profile name and revision.
    pub profile: String,
    /// Frozen profile revision.
    pub profile_revision: u64,
    /// Frozen configuration and asset revision.
    pub asset_revision: u64,
    /// `product` or `local` admission path.
    pub traffic: String,
    /// Native invocation identity, when confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_invocation_id: Option<String>,
    /// Terminal or current error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::error::ApiError>,
    /// Child delegations observed for this task.
    #[serde(default)]
    pub delegations: Vec<DiagnosticDelegation>,
    /// Pending approvals that currently block this task.
    #[serde(default)]
    pub approvals: Vec<DiagnosticApproval>,
}

/// Degraded storage observations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiagnosticStorage {
    /// Durable metadata state.
    pub metadata: String,
    /// Native artifact store state.
    pub artifacts: String,
    /// Explicit degradation reasons.
    #[serde(default)]
    pub degraded: Vec<String>,
}

/// Exported diagnostic bundle for one Worker Instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticBundle {
    /// Generation time.
    pub generated_at: i64,
    /// Composed Worker status.
    pub health: Health,
    /// Bounded metric samples.
    #[serde(default)]
    pub metrics: Vec<MetricSample>,
    /// Storage observations.
    pub storage: DiagnosticStorage,
    /// Actionable findings.
    #[serde(default)]
    pub findings: Vec<DiagnosticFinding>,
    /// Task diagnosis, when requested and visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<DiagnosticTask>,
    /// Correlation summary.
    pub correlation: DiagnosticCorrelation,
    /// Redacted audit tail.
    #[serde(default)]
    pub audit: Vec<serde_json::Value>,
    /// Redacted trace samples.
    #[serde(default)]
    pub traces: Vec<crate::health::TraceSample>,
    /// Subsystem summaries with no instance paths.
    #[serde(default)]
    pub subsystems: BTreeMap<String, String>,
}
