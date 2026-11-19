//! Durable Gateway storage interfaces.
//!
//! The control plane owns approvals, Worker leases, dispatch intent, request
//! identity, audit decisions, and stream mappings. Routing and authorization
//! policy stays in `crate::gateway`; these interfaces enforce storage
//! invariants only: uniqueness, optimistic revisions, atomic claims, lease
//! timestamps, and append-only audit.
//!
//! Two adapters implement the interfaces:
//!
//! - [`FileDispatchStore`] is the single-writer JSON store for standalone
//!   deployments.
//! - the feature-gated `PostgresBackend` is the transaction-shaped shared
//!   store. It keeps no authoritative process-local copy.

mod control;
#[cfg(feature = "postgres")]
mod postgres;
#[cfg(test)]
mod tests;

mod file;

use async_trait::async_trait;
use common::{ApiError, Task, TaskState};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use control::Control;
pub use control::{ApprovalStore, AuditStore, LeaseStore, TeamStore};
pub use file::DispatchData;
pub use file::FileDispatchStore;
#[cfg(feature = "postgres")]
pub use postgres::PostgresBackend;

/// Persisted Gateway dispatch schema.
pub const DISPATCH_SCHEMA: u32 = 1;

/// Stable identity for one Gateway replica process. Lease renewals record which
/// replica observed the Worker; claims are scoped to the same identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaId(pub String);

impl ReplicaId {
    /// Resolve a replica identity from configuration, then the environment,
    /// then a process-stable fallback for local use.
    pub fn resolve(configured: Option<&str>) -> Self {
        let value = configured
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                std::env::var("GOLDENBEE_GATEWAY_REPLICA_ID")
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_else(|| format!("gateway-{}", std::process::id()));
        Self(value)
    }
}

/// A claim held by one replica for one dispatch attempt.
#[derive(Debug, Clone)]
pub struct ClaimToken {
    /// External execution identity.
    pub execution_id: String,
    /// Unique claim within the execution.
    pub claim_id: String,
    /// Replica that owns the claim.
    pub replica: String,
}

/// New caller-scoped dispatch intent.
#[derive(Debug, Clone)]
pub struct NewIntent {
    /// External execution identity allocated by the Gateway.
    pub execution_id: String,
    /// Owning namespace.
    pub namespace: String,
    /// Submitting principal.
    pub caller: String,
    /// Caller idempotency key.
    pub idempotency_key: String,
    /// Canonical request digest.
    pub request_digest: String,
    /// Authorized Worker identity.
    pub worker: String,
    /// Agent capability.
    pub agent: String,
}

/// Result of recording a request identity.
#[derive(Debug, Clone)]
pub enum Begin {
    /// The row was inserted.
    Created(Submission),
    /// The same key and digest already exist.
    Existing(Submission),
    /// The key is bound to a different request digest.
    Conflict(Submission),
}

/// Result of trying to claim a dispatch attempt.
#[derive(Debug, Clone)]
pub enum Claim {
    /// This replica now owns the attempt.
    Claimed(ClaimToken, Submission),
    /// The mapping is already committed; the caller must replay it.
    Accepted(Submission),
    /// Another replica is in flight. The claim becomes available after the
    /// supplied database time.
    Busy {
        /// Claim expiry in milliseconds since the epoch.
        until_ms: i64,
    },
    /// The execution row does not exist.
    Missing,
}

/// First-writer-wins acceptance mapping.
#[derive(Debug, Clone)]
pub struct AcceptedMapping {
    /// Worker task handle.
    pub worker_task_id: String,
    /// Observed Worker process identity.
    pub worker_instance: String,
    /// Worker process incarnation at acceptance.
    pub worker_incarnation: String,
    /// Frozen acceptance fields.
    pub accepted: Accepted,
}

/// Outcome recorded when a claimed attempt ends without acceptance.
#[derive(Debug, Clone)]
pub struct AttemptOutcome {
    /// Durable dispatch state after the attempt.
    pub state: DispatchState,
    /// Connectivity projection.
    pub connectivity: Connectivity,
    /// Last redacted error label.
    pub last_error: Option<String>,
    /// Short audit outcome label.
    pub outcome: String,
    /// Redacted audit detail.
    pub detail: String,
}

impl AttemptOutcome {
    /// A typed Worker rejection. The request may be retried later.
    pub fn rejected(code: &str) -> Self {
        Self {
            state: DispatchState::Intent,
            connectivity: Connectivity::Online,
            last_error: Some(code.to_string()),
            outcome: "rejected".into(),
            detail: code.into(),
        }
    }

    /// A connect failure. A prior ambiguous attempt may still have been
    /// accepted, so the caller chooses the durable state.
    pub fn unreachable(previous: DispatchState, code: &str) -> Self {
        Self {
            state: if matches!(
                previous,
                DispatchState::Unknown | DispatchState::Dispatching
            ) {
                DispatchState::Unknown
            } else {
                DispatchState::Intent
            },
            connectivity: Connectivity::Unreachable,
            last_error: Some(code.to_string()),
            outcome: "unreachable".into(),
            detail: "no confirmed remote acceptance".into(),
        }
    }

    /// The remote request may have been accepted without an acknowledgment.
    pub fn ambiguous(code: &str) -> Self {
        Self {
            state: DispatchState::Unknown,
            connectivity: Connectivity::Unreachable,
            last_error: Some(code.to_string()),
            outcome: "unknown".into(),
            detail: "remote acceptance was not acknowledged".into(),
        }
    }
}

/// Shared read shape for stream reattachment. It deliberately carries no prompt
/// or request body: another replica can resume a stream from the mapping alone.
#[derive(Debug, Clone)]
pub struct StreamMapping {
    /// External execution identity.
    pub execution_id: String,
    /// Authorized Worker identity.
    pub worker: String,
    /// Observed Worker process identity.
    pub worker_instance: String,
    /// Worker process incarnation at acceptance.
    pub worker_incarnation: String,
    /// Worker task handle.
    pub worker_task_id: String,
    /// Frozen agent capability.
    pub agent: String,
    /// Owning namespace.
    pub namespace: String,
    /// Submitting principal.
    pub caller: String,
}

/// Durable dispatch status for one external execution identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchState {
    /// Identity and intent are durable; no remote request has been recorded as
    /// possible yet.
    Intent,
    /// A remote request may be in flight or the Gateway stopped while it was.
    Dispatching,
    /// The Worker returned a task handle. The mapping is durable.
    Accepted,
    /// The remote request may have been accepted but no handle is known.
    Unknown,
}

impl DispatchState {
    /// Stable audit label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Dispatching => "dispatching",
            Self::Accepted => "accepted",
            Self::Unknown => "unknown",
        }
    }
}

/// Connectivity is a Gateway projection. It never rewrites the Worker outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Connectivity {
    /// Not yet observed or lost during an ambiguous exchange.
    Unknown,
    /// The last request to the Worker succeeded.
    Online,
    /// The last request could not reach the Worker.
    Unreachable,
}

/// Frozen acceptance fields returned by the Worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Accepted {
    /// Frozen agent.
    pub agent: String,
    /// Frozen profile name and revision.
    pub profile: String,
    /// Last observed management state.
    pub state: TaskState,
    /// Frozen team version.
    pub team_version: Option<u64>,
}

/// One durable dispatch audit transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    /// Time.
    pub at: i64,
    /// Dispatch state after the transition.
    pub status: String,
    /// Short outcome label.
    pub outcome: String,
    /// Redacted detail.
    pub detail: String,
}

/// One caller-scoped external execution and its Worker mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submission {
    /// Stable external execution identity returned to clients.
    pub execution_id: String,
    /// Owning namespace.
    pub namespace: String,
    /// Submitting principal.
    pub caller: String,
    /// Original caller idempotency key.
    pub idempotency_key: String,
    /// Gateway request digest including the explicit Worker target.
    pub request_digest: String,
    /// Authorized Worker identity.
    pub worker: String,
    /// Observed Worker process identity.
    pub worker_instance: String,
    /// Worker process incarnation frozen at acceptance.
    #[serde(default)]
    pub worker_incarnation: String,
    /// Frozen agent capability.
    pub agent: String,
    /// Durable dispatch status.
    pub dispatch: DispatchState,
    /// Worker task handle, when the Worker confirmed acceptance.
    pub worker_task_id: Option<String>,
    /// Frozen Worker acceptance fields.
    pub accepted: Option<Accepted>,
    /// Last Worker projection observed by this Gateway.
    pub projection: Option<Task>,
    /// Connectivity projection only.
    pub connectivity: Connectivity,
    /// Last redacted error label.
    pub last_error: Option<String>,
    /// Monotonic row revision.
    #[serde(default)]
    pub revision: u64,
    /// Current claim, when a replica owns an in-flight attempt.
    #[serde(default)]
    pub claim_id: Option<String>,
    /// Replica that owns the claim.
    #[serde(default)]
    pub claimed_by: Option<String>,
    /// Claim expiry in milliseconds since the epoch.
    #[serde(default)]
    pub claim_expires_at: Option<i64>,
    /// Creation time.
    pub created_at: i64,
    /// Update time.
    pub updated_at: i64,
    /// Durable audit of dispatch transitions.
    pub transitions: Vec<Transition>,
}

impl Submission {
    /// Record one transition without changing other fields.
    pub fn transition(&mut self, outcome: &str, detail: &str) {
        self.updated_at = common::now_ms();
        self.transitions.push(Transition {
            at: self.updated_at,
            status: self.dispatch.label().to_string(),
            outcome: outcome.to_string(),
            detail: detail.to_string(),
        });
    }

    /// Shared stream mapping, when the Worker acceptance is resolved.
    pub fn stream_mapping(&self) -> Option<StreamMapping> {
        Some(StreamMapping {
            execution_id: self.execution_id.clone(),
            worker: self.worker.clone(),
            worker_instance: self.worker_instance.clone(),
            worker_incarnation: self.worker_incarnation.clone(),
            worker_task_id: self.worker_task_id.clone()?,
            agent: self.agent.clone(),
            namespace: self.namespace.clone(),
            caller: self.caller.clone(),
        })
    }
}

/// Durability shape behind the Gateway dispatch policy.
#[async_trait]
pub trait DispatchStore: Send + Sync {
    /// Record the caller-scoped request identity exactly once.
    async fn begin(&self, intent: NewIntent) -> Result<Begin, ApiError>;

    /// Atomically claim an intent, an unknown attempt, or an expired dispatch.
    async fn claim(
        &self,
        execution_id: &str,
        replica: &ReplicaId,
        claim_ttl_ms: i64,
    ) -> Result<Claim, ApiError>;

    /// Commit the first accepted mapping for a claim.
    async fn commit(
        &self,
        claim: &ClaimToken,
        mapping: AcceptedMapping,
    ) -> Result<Submission, ApiError>;

    /// Release a claim with a non-accepted outcome.
    async fn release(&self, claim: &ClaimToken, outcome: AttemptOutcome) -> Result<(), ApiError>;

    /// Update only the projection and connectivity of an accepted execution.
    async fn record_projection(
        &self,
        execution_id: &str,
        projection: Option<Task>,
        state: Option<TaskState>,
        connectivity: Connectivity,
    ) -> Result<(), ApiError>;

    /// Remove a Worker projection after the owning store reports the task or
    /// its retained history is absent. The dispatch mapping remains for
    /// diagnostics, but no stale content is served to a caller.
    async fn clear_projection(
        &self,
        execution_id: &str,
        outcome: &str,
        detail: &str,
    ) -> Result<(), ApiError>;

    /// Record a connectivity change without changing dispatch identity.
    async fn mark_connectivity(
        &self,
        execution_id: &str,
        connectivity: Connectivity,
        outcome: &str,
        detail: &str,
    ) -> Result<(), ApiError>;

    /// Append one dispatch transition.
    async fn transition(
        &self,
        execution_id: &str,
        outcome: &str,
        detail: &str,
    ) -> Result<(), ApiError>;

    /// Read one external execution.
    async fn submission(&self, execution_id: &str) -> Result<Option<Submission>, ApiError>;

    /// Read one caller-scoped request identity.
    async fn submission_by_key(
        &self,
        namespace: &str,
        caller: &str,
        key: &str,
    ) -> Result<Option<Submission>, ApiError>;

    /// Redacted operator description. Never contains a credential.
    fn description(&self) -> String;

    /// Backing file path for the standalone adapter.
    fn path(&self) -> Option<PathBuf>;

    /// Readiness check for a shared adapter.
    async fn ready(&self) -> Result<(), ApiError> {
        Ok(())
    }
}

/// Read-only stream mapping interface. The mapping lives on the dispatch row so
/// it cannot drift from `worker_task_id`.
#[async_trait]
pub trait StreamMapStore: Send + Sync {
    /// Resolve a stream mapping without reading or resubmitting the prompt.
    async fn stream_mapping(&self, execution_id: &str) -> Result<Option<StreamMapping>, ApiError>;
}

/// Stable caller-scoped index key. Namespace and caller are framed separately so
/// two namespaces cannot share a mapping.
pub fn index_key(namespace: &str, caller: &str, idempotency_key: &str) -> String {
    format!("{namespace}\u{1f}{caller}\u{1f}{idempotency_key}")
}

/// Replace a JSON file through a synced temporary file.
pub(crate) fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), ApiError> {
    use std::io::Write;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(|error| {
        ApiError::retryable(
            "storage.unavailable",
            format!("cannot persist Gateway dispatch state: {error}"),
        )
    })
}

#[cfg(unix)]
pub(crate) fn sync_parent(path: &std::path::Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
pub(crate) fn sync_parent(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// Exclusive file lock that makes the standalone store single-writer.
#[derive(Debug)]
pub(crate) struct SingleWriter {
    _file: std::fs::File,
}

impl SingleWriter {
    pub(crate) fn acquire(path: &std::path::Path) -> Result<Self, ApiError> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|error| {
                ApiError::new(
                    "storage.unavailable",
                    format!("cannot open Gateway state lock: {error}"),
                )
            })?;
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Self { _file: file }),
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => Err(ApiError::new(
                "storage.single_writer",
                "this Gateway state directory is already owned by another process",
            )
            .with(
                "hint",
                "shared deployments must use the Postgres store; the file store is single-writer",
            )),
            Err(error) => Err(ApiError::retryable(
                "storage.unavailable",
                format!("cannot lock Gateway state: {error}"),
            )),
        }
    }
}
