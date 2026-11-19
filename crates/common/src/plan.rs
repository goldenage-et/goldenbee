//! Master plan wire inputs and objective-evidence criteria.
//!
//! These values describe what a Master Worker should coordinate and what it
//! must observe before reporting success. They carry no execution authority;
//! the Worker runtime validates every child against the parent task's frozen
//! team roster or its operator-configured static peers.

use crate::limits::Limits;
use serde::{Deserialize, Serialize};

/// Create one durable Master execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePlanRequest {
    /// Master harness task that owns this plan and its frozen authority.
    pub parent_task_id: String,
    /// Objective statement retained across restart.
    pub objective: String,
    /// Required result criteria evaluated against observed child evidence.
    #[serde(default)]
    pub criteria: Vec<PlanCriterion>,
    /// Planned remote subtasks.
    #[serde(default)]
    pub children: Vec<PlanChildRequest>,
    /// Requested bounds for every child. Narrowed before issuance.
    #[serde(default)]
    pub limits: Limits,
    /// Optional caller idempotency key. The canonical request always selects
    /// the same plan identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// One required result criterion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCriterion {
    /// Stable criterion id within the plan.
    pub id: String,
    /// Human-readable requirement.
    #[serde(default)]
    pub description: String,
    /// Planned child whose observed evidence satisfies this criterion.
    pub child: String,
    /// Evidence that must be observed. A child success claim alone is not
    /// evidence.
    pub evidence: CriterionEvidence,
}

/// Evidence requirement checked by the Master runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CriterionEvidence {
    /// The observed child output contains this text. An observed result is
    /// required; a child success assertion alone cannot verify the objective.
    OutputContains {
        /// Exact substring required in the observed output.
        text: String,
    },
    /// The child produced an immutable artifact. When `digest` is present it
    /// must match exactly; a mismatch is conflicting evidence.
    ArtifactDigest {
        /// Optional exact artifact identity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_id: Option<String>,
        /// Optional exact `sha256:<hex>` content digest.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
    },
}

/// One planned remote subtask.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanChildRequest {
    /// Stable child id within the plan.
    pub id: String,
    /// Frozen team-roster member (Gateway Worker identity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// Operator-configured static peer id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    /// Authorized capability to invoke.
    pub capability: String,
    /// Objective text sent once to the child.
    pub objective: String,
    /// Requested child limits. Narrowed against the parent and roster.
    #[serde(default)]
    pub limits: Limits,
    /// Whether this child must complete successfully before verification.
    #[serde(default = "default_required")]
    pub required: bool,
}

fn default_required() -> bool {
    true
}

/// Bounded management request for dispatch, recovery, or evaluation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanControlRequest {
    /// Maximum time to wait for known child handles to become terminal. The
    /// Worker clamps this to a bounded window.
    #[serde(default)]
    pub wait_ms: Option<u64>,
}

/// Explicit escalation of a partial or unresolved plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalatePlanRequest {
    /// Redacted operator/Master reason.
    pub reason: String,
}
