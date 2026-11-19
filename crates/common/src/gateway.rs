//! Gateway routing requests, results, connectivity, and liveness.

use crate::limits::Limits;
use crate::profile::Approval;
use crate::task::{Task, TaskState};
use serde::{Deserialize, Serialize};

/// Submission through a Gateway to an explicit Worker Instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewaySubmitRequest {
    /// Explicit authorized Worker Instance. Omitted is accepted only when the
    /// Gateway can choose without ambiguity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// Agent capability within the selected package.
    pub agent: String,
    /// Session identity, scoped by namespace and principal.
    pub session_id: String,
    /// User input.
    pub input: String,
    /// Caller-scoped idempotency key.
    pub idempotency_key: String,
    /// Optional profile name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Optional team name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Required frozen team version. Omitted selects the current active version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_version: Option<u64>,
    /// Requested execution limits.
    #[serde(default)]
    pub limits: Limits,
}

/// Gateway acceptance result carrying routing metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewaySubmitResponse {
    /// Stable Gateway external execution identity. The Worker handle stays
    /// private to the Gateway dispatch mapping.
    pub task_id: String,
    /// Authorized Worker Instance that accepted the work.
    pub worker: String,
    /// Worker process identity observed by the Gateway.
    pub worker_instance: String,
    /// Worker incarnation frozen when the task was routed.
    #[serde(default)]
    pub worker_incarnation: String,
    /// Frozen agent capability.
    pub agent: String,
    /// Frozen profile name and revision.
    pub profile: String,
    /// Current management projection.
    pub state: TaskState,
    /// True when the Worker returned an existing handle.
    pub replayed: bool,
    /// Frozen team version, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_version: Option<u64>,
}

/// Gateway routing metadata for one task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayTask {
    /// Gateway external execution identity.
    pub task_id: String,
    /// Authorized Worker Instance.
    pub worker: String,
    /// Worker process identity.
    pub worker_instance: String,
    /// Worker incarnation that accepted the routed task.
    #[serde(default)]
    pub worker_incarnation: String,
    /// Connectivity projection observed by the Gateway.
    #[serde(default)]
    pub connectivity: GatewayConnectivity,
    /// Durable Worker projection.
    pub task: Task,
}

/// Gateway connectivity projection. It is not a task outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayConnectivity {
    /// The Gateway has not observed the Worker for this execution.
    #[default]
    Unknown,
    /// The last Worker exchange succeeded.
    Online,
    /// The last Worker exchange could not reach the Worker. The stored Worker
    /// projection is returned unchanged.
    Unreachable,
}

/// One Worker-owned approval projected through the Gateway.
///
/// The Gateway carries only the route and the Worker-recorded approval. It does
/// not store the decision, so an unreachable Worker can never be reported as
/// approved from a Client command alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayApproval {
    /// Gateway external execution identity for the owning task.
    pub task_id: String,
    /// Authorized Worker Instance that owns the approval.
    pub worker: String,
    /// Durable Worker approval record.
    pub approval: Approval,
}

/// Gateway liveness and routing summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayHealth {
    /// Liveness phase.
    pub status: String,
    /// Approved Worker registrations known at startup.
    pub workers: u32,
    /// Approved Workers currently reachable.
    pub reachable: u32,
}
