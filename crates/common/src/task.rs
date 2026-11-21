//! Direct task lifecycle, submission, and durable projection.

fn default_traffic() -> String {
    "local".into()
}

use crate::error::ApiError;
use crate::limits::{Limits, Usage};
use serde::{Deserialize, Serialize};

/// Lifecycle of a harness task. This is a management projection, not the A2A
/// wire enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Metadata is durable; native execution has not been confirmed.
    Accepted,
    /// Waiting for a finite admission slot.
    Queued,
    /// Native execution is active.
    Running,
    /// Native execution paused for an exact approval.
    WaitingApproval,
    /// Cancellation was requested and is not yet confirmed.
    Cancelling,
    /// The process cannot currently prove the dispatch or effect outcome.
    Recovering,
    /// The outcome is unknown and requires reconciliation or review.
    Unknown,
    /// Completed successfully.
    Succeeded,
    /// Completed with a definite failure.
    Failed,
    /// Cancellation completed or was confirmed.
    Cancelled,
}

impl TaskState {
    /// Terminal states never accept a new native attempt under the same task.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    /// Whether the state carries an actionable uncertainty.
    pub fn is_uncertain(self) -> bool {
        matches!(self, Self::Recovering | Self::Unknown)
    }
}

/// One item in a task-owned plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    /// What the step does.
    pub content: String,
    /// One of `pending`, `in_progress`, or `completed`.
    pub status: String,
}

/// Per-task plan state recorded by the planning tool.
///
/// The plan belongs to exactly one task. It is replaced in full on each update
/// and is removed when the task record is retained or erased.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPlan {
    /// Owning task.
    pub task_id: String,
    /// Current ordered plan.
    #[serde(default)]
    pub items: Vec<PlanItem>,
    /// Last update time in milliseconds.
    pub updated_at: i64,
}

/// Request to submit a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitTaskRequest {
    /// Application namespace. Omitted means the authenticated principal's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Agent selected by the caller.
    pub agent: String,
    /// Session identity, scoped by namespace and principal.
    pub session_id: String,
    /// User input.
    pub input: String,
    /// Caller-scoped idempotency key.
    pub idempotency_key: String,
    /// Optional profile name. The active revision is pinned at acceptance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Optional team name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Explicit target Worker through a Gateway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// Requested execution limits.
    #[serde(default)]
    pub limits: Limits,
}

/// Response returned when a submission is durably accepted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitTaskResponse {
    /// Stable external task handle.
    pub task_id: String,
    /// Current projection.
    pub state: TaskState,
    /// True when the original handle was returned for a duplicate key.
    pub replayed: bool,
    /// True when acceptance could not be confirmed.
    #[serde(default)]
    pub unknown_submission: bool,
    /// Frozen agent name.
    pub agent: String,
    /// Frozen profile name and revision.
    pub profile: String,
    /// Frozen team version, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_version: Option<u64>,
}

/// Concrete provider selected for a task's effective profile revision.
///
/// This is the invocation-time projection of a logical model reference. It
/// carries references and identifiers only; credential values are never part
/// of a task or an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedModel {
    /// Logical provider profile selected by the profile revision.
    pub provider: String,
    /// Native adapter kind used to construct the model.
    pub adapter: String,
    /// Concrete provider model identifier sent to the endpoint.
    pub model: String,
    /// Whether the provider is a local deployment.
    pub local: bool,
    /// Declared deployment residency.
    pub residency: String,
    /// Sanitized endpoint identity, without userinfo, query, or fragment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Credential reference, never a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    /// Operator-declared input price in millionths of a currency unit per
    /// million tokens, when configured. Used only for local estimates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cost_micros_per_million: Option<u64>,
    /// Operator-declared candidate price in millionths of a currency unit per
    /// million tokens, when configured. Used only for local estimates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_cost_micros_per_million: Option<u64>,
    /// Effective immutable profile revision that selected the provider.
    pub profile_revision: u64,
}

/// Durable task projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Stable external handle.
    pub id: String,
    /// Owning namespace.
    pub namespace: String,
    /// Submitting principal.
    pub caller: String,
    /// Agent name.
    pub agent: String,
    /// Session ID.
    pub session_id: String,
    /// Original input.
    pub input: String,
    /// Bounded inline images admitted with the text input.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_images: Vec<crate::run::RunImage>,
    /// Idempotency key.
    pub idempotency_key: String,
    /// Canonical request digest.
    pub request_digest: String,
    /// Frozen profile revision.
    pub profile: String,
    /// Frozen profile revision number.
    pub profile_revision: u64,
    /// Frozen resolved configuration and asset revision.
    #[serde(default)]
    pub asset_revision: u64,
    /// Cross-process trace identity.
    #[serde(default)]
    pub trace_id: String,
    /// `product` when admitted through a Gateway delegation, `local` for a
    /// direct operator/Launcher submission.
    #[serde(default = "default_traffic")]
    pub traffic: String,
    /// Concrete provider resolved from the effective profile revision.
    ///
    /// Absent only for a task recorded before this field existed or when no
    /// provider decision could be proven at admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<ResolvedModel>,
    /// Frozen team name and version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Frozen team version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_version: Option<u64>,
    /// Immutable process-level roster snapshot, when this is a team execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_roster: Option<crate::team::TeamRoster>,
    /// Effective scopes inherited from the submitting authority.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Worker instance identities in this delegation path, including the current hop.
    #[serde(default)]
    pub delegation_path: Vec<String>,
    /// Expiry of the submitting delegation authority, when bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_expires_at: Option<i64>,
    /// Management state.
    pub state: TaskState,
    /// Attempts under this task identity.
    pub attempt: u32,
    /// Native invocation identity, when confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_invocation_id: Option<String>,
    /// Native graph thread identity, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_thread_id: Option<String>,
    /// Native A2A task identity, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_a2a_task_id: Option<String>,
    /// Final output text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Package-owned objective evidence committed after the native outcome.
    ///
    /// A package hook may attach digest references, check outcomes, and
    /// explicit unverified limits. Absence means no evidence was produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
    /// Stable error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
    /// Why an outcome is uncertain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<String>,
    /// Applied limits.
    #[serde(default)]
    pub limits: Limits,
    /// Provider-confirmed usage.
    #[serde(default)]
    pub usage: Usage,
    /// Creation time.
    pub created_at: i64,
    /// Last update time.
    pub updated_at: i64,
}
