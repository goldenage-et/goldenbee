//! Persisted side-effect operation identities and reconciliation.
//!
//! The operation store belongs to one Worker Instance. A registered
//! side-effecting tool reserves its identity here before dispatch and records
//! the receipt only after a confirmed response. A crash therefore leaves
//! either a durable reservation or a durable receipt, never an untracked
//! effect.
//!
//! Receipts are evidence, not authority. No shared `common` type can write
//! this store, and a Client cannot authorize a retry by presenting an
//! operation id.

use crate::{new_id, now, OperationRecord, Result, Runtime};
use common::{ApiError, Task};
use serde::Serialize;

/// Operation is reserved and may be in flight.
pub const STATUS_RESERVED: &str = "reserved";
/// External system confirmed the effect.
pub const STATUS_COMMITTED: &str = "committed";
/// External system confirmed no effect exists.
pub const STATUS_ABSENT: &str = "absent";
/// External system confirmed a definite failure with no applied effect.
pub const STATUS_FAILED: &str = "failed";
/// The external outcome cannot currently be established.
pub const STATUS_UNKNOWN: &str = "unknown";

/// Result of asking an external system about one operation.
///
/// `Unknown` is a first-class answer. An unavailable or inconclusive status
/// interface must never be reported as `Absent`, `Failed`, or success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectStatus {
    /// The effect exists.
    Committed {
        /// External reference, when the status interface exposes one.
        external_ref: Option<String>,
        /// Receipt text, already safe for redacted persistence.
        receipt: Option<String>,
    },
    /// The external system confirms no effect exists.
    Absent,
    /// The external system confirms a definite failure with no applied effect.
    Failed {
        /// Redacted detail.
        detail: String,
    },
    /// The outcome is unknown, including an unavailable status interface.
    Unknown {
        /// Redacted detail.
        detail: String,
    },
}

/// Worker-owned status or idempotency interface for one effect tool.
#[async_trait::async_trait]
pub trait EffectReconciler: Send + Sync {
    /// Establish the external state for a reserved or unknown operation.
    async fn status(&self, operation: &OperationRecord) -> EffectStatus;
}

/// Approval requirement for one protected effect operation.
///
/// The policy is Worker-owned configuration, not a Client decision. It supplies
/// the risk note and expiry shown to an approver and the argument that names the
/// exact destination. The digest still binds every argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectApprovalPolicy {
    /// Risk note shown with the request.
    pub risk: String,
    /// Approval lifetime in milliseconds.
    pub ttl_ms: i64,
    /// Argument whose value names the exact target. Falls back to the digest
    /// when the argument is absent or not a string.
    pub target_argument: String,
}

impl EffectApprovalPolicy {
    /// Approval policy for an external write with an explicit risk note.
    pub fn new(risk: impl Into<String>, ttl_ms: i64) -> Self {
        Self {
            risk: risk.into(),
            ttl_ms: ttl_ms.max(1),
            target_argument: "target".into(),
        }
    }

    /// Name the argument that carries the exact target.
    pub fn target_argument(mut self, argument: impl Into<String>) -> Self {
        self.target_argument = argument.into();
        self
    }

    /// Extract the exact target from canonical tool arguments.
    pub fn target(&self, args: &serde_json::Value) -> String {
        args.get(&self.target_argument)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{} arguments", self.target_argument))
    }
}

/// Documented retry policy for one protected effect.
///
/// Retries happen inside the same admitted task execution, with the same task
/// identity and the same immutable operation identity. A Client cannot request
/// one. Known-absent and definite-failed retries are opt-in; the default policy
/// never repeats an external effect.
#[derive(Debug, Clone)]
pub struct EffectPolicy {
    /// Stable policy name recorded with every attempt.
    pub name: String,
    /// Maximum dispatch attempts under one operation identity.
    pub max_attempts: u32,
    /// Permit a retry after the status interface reports `absent`.
    pub retry_absent: bool,
    /// Permit a retry after the status interface reports a definite failure.
    pub retry_failed: bool,
    /// Require exact human approval before dispatch.
    pub approval: Option<EffectApprovalPolicy>,
}

impl Default for EffectPolicy {
    fn default() -> Self {
        Self {
            name: "effect.no-retry.v1".into(),
            max_attempts: 1,
            retry_absent: false,
            retry_failed: false,
            approval: None,
        }
    }
}

impl EffectPolicy {
    /// A policy that retries only outcomes the status interface classifies as
    /// known-absent or definite-failed, up to an explicit attempt bound.
    pub fn reconcile(max_attempts: u32) -> Self {
        Self {
            name: "effect.reconcile.v1".into(),
            max_attempts: max_attempts.max(1),
            retry_absent: true,
            retry_failed: true,
            approval: None,
        }
    }

    /// Require exact approval for the unchanged operation before dispatch.
    pub fn requiring_approval(mut self, approval: EffectApprovalPolicy) -> Self {
        self.approval = Some(approval);
        self
    }

    /// Whether this policy requires an exact approval.
    pub fn requires_approval(&self) -> bool {
        self.approval.is_some()
    }

    /// Recorded description. It contains no secret and no Client identity.
    pub fn description(&self) -> String {
        format!(
            "{};max_attempts={};retry_absent={};retry_failed={};approval={}",
            self.name,
            self.max_attempts,
            self.retry_absent,
            self.retry_failed,
            self.approval.is_some()
        )
    }

    pub(crate) fn permits(&self, status: &str) -> bool {
        match status {
            STATUS_ABSENT => self.retry_absent,
            STATUS_FAILED => self.retry_failed,
            _ => false,
        }
    }
}

/// Atomic result of reserving an operation before dispatch.
#[derive(Debug, Clone)]
pub enum BeginOutcome {
    /// The caller owns the reservation and must dispatch.
    Dispatch(OperationRecord),
    /// The effect is already confirmed; return the stored receipt without
    /// dispatching again.
    Committed(OperationRecord),
    /// The outcome is unknown or another attempt is still reserved. No
    /// dispatch is safe.
    Blocked {
        /// Persisted operation.
        operation: OperationRecord,
        /// Actionable reason.
        reason: String,
    },
}

/// Safe, read-only projection of one operation.
#[derive(Debug, Clone, Serialize)]
pub struct OperationView {
    /// Operation identity.
    pub id: String,
    /// Owning task identity.
    pub task_id: String,
    /// Exact operation digest.
    pub digest: String,
    /// Operation status.
    pub status: String,
    /// Dispatch attempts.
    pub attempt: u32,
    /// Native invocation that dispatched it.
    pub invocation_id: Option<String>,
    /// Owning tool.
    pub tool: String,
    /// Stable external idempotency key.
    pub idempotency_key: String,
    /// Policy recorded with the attempt.
    pub policy: String,
    /// Redacted external reference.
    pub external_ref: Option<String>,
    /// Redacted receipt or reconciliation detail.
    pub receipt: Option<String>,
    /// Update time.
    pub updated_at: i64,
}

impl From<&OperationRecord> for OperationView {
    fn from(operation: &OperationRecord) -> Self {
        Self {
            id: operation.id.clone(),
            task_id: operation.task_id.clone(),
            digest: operation.digest.clone(),
            status: operation.status.clone(),
            attempt: operation.attempt,
            invocation_id: operation.invocation_id.clone(),
            tool: operation.tool.clone(),
            idempotency_key: operation.idempotency_key.clone(),
            policy: operation.policy.clone(),
            external_ref: operation.external_ref.clone(),
            receipt: operation.receipt.clone(),
            updated_at: operation.updated_at,
        }
    }
}

/// Public evidence linking task, attempt, native invocation, operations, and
/// external references.
#[derive(Debug, Clone, Serialize)]
pub struct EffectEvidence {
    /// Public task handle.
    pub task_id: String,
    /// Current task attempt.
    pub attempt: u32,
    /// Confirmed native invocation, when known.
    pub native_invocation_id: Option<String>,
    /// Operations under this task.
    pub operations: Vec<OperationView>,
}

/// Counts from startup effect reconciliation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectRecovery {
    /// Operations confirmed committed.
    pub committed: Vec<String>,
    /// Operations confirmed absent.
    pub absent: Vec<String>,
    /// Operations confirmed failed.
    pub failed: Vec<String>,
    /// Operations that remain unknown.
    pub unknown: Vec<String>,
}

/// Why a task cannot be reported as a definite success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectTerminal {
    /// At least one operation is reserved or unknown.
    Unknown,
    /// At least one operation definitely failed.
    Failed,
    /// At least one operation is known absent.
    Absent,
}

fn safe_text(value: &str, limit: usize) -> String {
    common::redact(value).chars().take(limit).collect()
}

fn safe_ref(value: Option<&str>) -> Option<String> {
    value
        .map(|value| safe_text(value, 512))
        .filter(|value| !value.is_empty())
}

impl Runtime {
    /// Reserve one effect operation before dispatch.
    ///
    /// A repeated task/digest pair reuses the original operation identity. A
    /// committed operation is returned as [`BeginOutcome::Committed`] and must
    /// not dispatch again. An unknown or still-reserved operation is returned
    /// as [`BeginOutcome::Blocked`]. A known-absent or definite-failed
    /// operation becomes dispatchable again only when the policy explicitly
    /// permits it and the attempt bound is not exhausted.
    pub async fn begin_operation(
        &self,
        task_id: &str,
        tool: &str,
        digest: &str,
        invocation_id: Option<&str>,
        policy: &EffectPolicy,
    ) -> Result<BeginOutcome> {
        let policy_text = policy.description();
        let max_attempts = policy.max_attempts.max(1);
        self.store()
            .write(|state| {
                let existing = state
                    .operations
                    .values()
                    .find(|operation| operation.task_id == task_id && operation.digest == digest)
                    .cloned();
                if let Some(mut operation) = existing {
                    match operation.status.as_str() {
                        STATUS_COMMITTED => return Ok(BeginOutcome::Committed(operation)),
                        STATUS_UNKNOWN => {
                            return Ok(BeginOutcome::Blocked {
                                reason: "the external effect outcome is unknown and requires reconciliation or review".into(),
                                operation,
                            })
                        }
                        STATUS_RESERVED => {
                            return Ok(BeginOutcome::Blocked {
                                reason: "another dispatch attempt for this effect is still reserved".into(),
                                operation,
                            })
                        }
                        status if policy.permits(status) && operation.attempt < max_attempts => {
                            operation.status = STATUS_RESERVED.into();
                            operation.attempt += 1;
                            operation.invocation_id = invocation_id.map(str::to_string);
                            operation.policy = policy_text.clone();
                            operation.receipt = None;
                            operation.updated_at = now();
                            let reserved = operation.clone();
                            state
                                .operations
                                .insert(reserved.id.clone(), reserved.clone());
                            return Ok(BeginOutcome::Dispatch(reserved));
                        }
                        status => {
                            return Ok(BeginOutcome::Blocked {
                                reason: format!(
                                    "the effect is '{status}' and the retry policy does not authorize another dispatch"
                                ),
                                operation,
                            })
                        }
                    }
                }
                let id = new_id("op");
                let operation = OperationRecord {
                    id: id.clone(),
                    task_id: task_id.to_string(),
                    digest: digest.to_string(),
                    status: STATUS_RESERVED.into(),
                    external_ref: None,
                    receipt: None,
                    updated_at: now(),
                    attempt: 1,
                    invocation_id: invocation_id.map(str::to_string),
                    tool: tool.to_string(),
                    idempotency_key: id,
                    policy: policy_text,
                };
                state
                    .operations
                    .insert(operation.id.clone(), operation.clone());
                Ok(BeginOutcome::Dispatch(operation))
            })
            .await
    }

    /// Advance a known-absent or definite-failed operation to another dispatch
    /// attempt inside the same task execution.
    ///
    /// The operation identity and external idempotency key do not change. The
    /// policy attempt bound is re-checked atomically so concurrent callers
    /// cannot exceed it.
    pub async fn retry_reserved_operation(
        &self,
        operation_id: &str,
        policy: &EffectPolicy,
    ) -> Result<OperationRecord> {
        let policy_text = policy.description();
        let max_attempts = policy.max_attempts.max(1);
        self.store()
            .write(|state| {
                let operation = state.operations.get_mut(operation_id).ok_or_else(|| {
                    ApiError::new("operation.not_found", "operation is not visible")
                })?;
                if operation.status == STATUS_UNKNOWN {
                    return Err(ApiError::new(
                        "operation.outcome_unknown",
                        "an unknown effect outcome cannot be retried automatically",
                    ));
                }
                if !matches!(operation.status.as_str(), STATUS_ABSENT | STATUS_FAILED) {
                    return Err(ApiError::new(
                        "operation.retry_invalid",
                        "only a known-absent or definite-failed effect may be retried",
                    ));
                }
                if !policy.permits(&operation.status) || operation.attempt >= max_attempts {
                    return Err(ApiError::new(
                        "operation.retry_exhausted",
                        "the documented retry policy does not authorize another attempt",
                    ));
                }
                operation.status = STATUS_RESERVED.into();
                operation.attempt += 1;
                operation.policy = policy_text;
                operation.receipt = None;
                operation.updated_at = now();
                Ok(operation.clone())
            })
            .await
    }

    /// Record a confirmed external result.
    ///
    /// Repeating the call for an already-committed operation is idempotent and
    /// keeps the original receipt.
    pub async fn complete_operation(
        &self,
        operation_id: &str,
        external_ref: Option<&str>,
        receipt: &str,
    ) -> Result<OperationRecord> {
        self.store()
            .write(|state| {
                let operation = state.operations.get_mut(operation_id).ok_or_else(|| {
                    ApiError::new("operation.not_found", "operation is not visible")
                })?;
                if operation.status == STATUS_COMMITTED {
                    return Ok(operation.clone());
                }
                operation.status = STATUS_COMMITTED.into();
                operation.external_ref = safe_ref(external_ref);
                operation.receipt = Some(safe_text(receipt, 4096));
                operation.updated_at = now();
                Ok(operation.clone())
            })
            .await
    }

    /// Record a status established by a status or idempotency interface.
    pub async fn resolve_operation(
        &self,
        operation_id: &str,
        status: &str,
        external_ref: Option<&str>,
        detail: &str,
    ) -> Result<OperationRecord> {
        if !matches!(
            status,
            STATUS_COMMITTED | STATUS_ABSENT | STATUS_FAILED | STATUS_UNKNOWN
        ) {
            return Err(ApiError::new(
                "operation.status_invalid",
                "unsupported operation status",
            ));
        }
        self.store()
            .write(|state| {
                let operation = state.operations.get_mut(operation_id).ok_or_else(|| {
                    ApiError::new("operation.not_found", "operation is not visible")
                })?;
                operation.status = status.to_string();
                operation.external_ref = safe_ref(external_ref);
                operation.receipt = Some(safe_text(detail, 4096));
                operation.updated_at = now();
                Ok(operation.clone())
            })
            .await
    }

    /// Mark an operation whose external outcome could not be established.
    pub async fn mark_operation_unknown(&self, operation_id: &str, reason: &str) -> Result<()> {
        self.resolve_operation(operation_id, STATUS_UNKNOWN, None, reason)
            .await
            .map(|_| ())
    }

    /// Fetch an operation.
    pub async fn operation(&self, operation_id: &str) -> Option<OperationRecord> {
        self.store()
            .read(|state| state.operations.get(operation_id).cloned())
            .await
    }

    /// List operations for a task.
    pub async fn operations_for(&self, task_id: &str) -> Vec<OperationRecord> {
        self.store()
            .read(|state| {
                state
                    .operations
                    .values()
                    .filter(|operation| operation.task_id == task_id)
                    .cloned()
                    .collect()
            })
            .await
    }

    /// Resolve the active task that owns a native tool invocation.
    ///
    /// The session write lock guarantees one running task per
    /// namespace/caller/session, so an effect tool cannot attribute its
    /// operation to an unrelated task.
    pub async fn effect_task(
        &self,
        session_id: &str,
        namespace: &str,
        caller: &str,
    ) -> Option<Task> {
        self.store()
            .read(|state| {
                state
                    .tasks
                    .values()
                    .filter(|task| {
                        task.session_id == session_id
                            && task.namespace == namespace
                            && task.caller == caller
                            && task.state == common::TaskState::Running
                    })
                    .max_by_key(|task| task.updated_at)
                    .cloned()
            })
            .await
    }

    /// Public, read-only evidence projection for one task.
    pub async fn effect_evidence(&self, task_id: &str) -> Option<EffectEvidence> {
        let (task, operations) = self
            .store()
            .read(|state| {
                let task = state.tasks.get(task_id).cloned()?;
                let operations = state
                    .operations
                    .values()
                    .filter(|operation| operation.task_id == task_id)
                    .map(OperationView::from)
                    .collect();
                Some((task, operations))
            })
            .await?;
        Some(EffectEvidence {
            task_id: task.id,
            attempt: task.attempt,
            native_invocation_id: task.native_invocation_id,
            operations,
        })
    }

    /// Whether any operation prevents treating this task as a definite result.
    pub async fn effect_terminal(&self, task_id: &str) -> Option<EffectTerminal> {
        self.store()
            .read(|state| {
                let mut terminal = None;
                for operation in state
                    .operations
                    .values()
                    .filter(|operation| operation.task_id == task_id)
                {
                    match operation.status.as_str() {
                        STATUS_RESERVED | STATUS_UNKNOWN => return Some(EffectTerminal::Unknown),
                        STATUS_FAILED => terminal = Some(EffectTerminal::Failed),
                        STATUS_ABSENT if terminal.is_none() => {
                            terminal = Some(EffectTerminal::Absent)
                        }
                        _ => {}
                    }
                }
                terminal
            })
            .await
    }

    /// Reconcile operations left reserved or unknown by a previous process.
    ///
    /// Startup calls this before readiness. Each operation asks its registered
    /// Worker-owned status interface. An unavailable or inconclusive interface
    /// leaves the operation unknown; it is never guessed absent or retried.
    pub async fn reconcile_effects(&self) -> Result<EffectRecovery> {
        let operations = self
            .store()
            .read(|state| {
                state
                    .operations
                    .values()
                    .filter(|operation| {
                        matches!(operation.status.as_str(), STATUS_RESERVED | STATUS_UNKNOWN)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .await;
        let mut recovery = EffectRecovery::default();
        let mut touched_tasks = Vec::new();
        for operation in operations {
            let status = match self.effect_reconciler(&operation.tool).await {
                Some(reconciler) => reconciler.status(&operation).await,
                None => EffectStatus::Unknown {
                    detail: format!(
                        "no status interface is registered for effect tool '{}'",
                        operation.tool
                    ),
                },
            };
            let (resolved, detail) = match status {
                EffectStatus::Committed {
                    external_ref,
                    receipt,
                } => {
                    let detail =
                        receipt.unwrap_or_else(|| "external status confirmed commitment".into());
                    self.resolve_operation(
                        &operation.id,
                        STATUS_COMMITTED,
                        external_ref.as_deref(),
                        &detail,
                    )
                    .await?;
                    recovery.committed.push(operation.id.clone());
                    (STATUS_COMMITTED, detail)
                }
                EffectStatus::Absent => {
                    let detail = "external status confirms no effect exists".to_string();
                    self.resolve_operation(&operation.id, STATUS_ABSENT, None, &detail)
                        .await?;
                    recovery.absent.push(operation.id.clone());
                    (STATUS_ABSENT, detail)
                }
                EffectStatus::Failed { detail } => {
                    self.resolve_operation(&operation.id, STATUS_FAILED, None, &detail)
                        .await?;
                    recovery.failed.push(operation.id.clone());
                    (STATUS_FAILED, detail)
                }
                EffectStatus::Unknown { detail } => {
                    self.resolve_operation(&operation.id, STATUS_UNKNOWN, None, &detail)
                        .await?;
                    recovery.unknown.push(operation.id.clone());
                    (STATUS_UNKNOWN, detail)
                }
            };
            touched_tasks.push(operation.task_id.clone());
            let _ = self
                .append_event(
                    &operation.task_id,
                    "operation.reconciled",
                    serde_json::json!({
                        "operation": operation.id,
                        "tool": operation.tool,
                        "attempt": operation.attempt,
                        "status": resolved,
                        "detail": detail,
                    }),
                    true,
                )
                .await;
        }
        touched_tasks.sort();
        touched_tasks.dedup();
        for task_id in touched_tasks {
            if self.effect_terminal(&task_id).await == Some(EffectTerminal::Unknown) {
                self.block_task_for_effect(&task_id).await;
            }
        }
        Ok(recovery)
    }

    /// Mark a non-terminal task explicitly unknown because an effect is
    /// unresolved. Terminal outcomes are never rewritten.
    pub(crate) async fn block_task_for_effect(&self, task_id: &str) {
        let updated = self
            .store()
            .write(|state| {
                let Some(task) = state.tasks.get_mut(task_id) else {
                    return Ok(false);
                };
                if task.state.is_terminal() {
                    return Ok(false);
                }
                task.state = common::TaskState::Unknown;
                task.uncertainty = Some(
                    "an external effect outcome could not be established; reconcile or review before retrying"
                        .into(),
                );
                task.updated_at = now();
                Ok(true)
            })
            .await
            .unwrap_or(false);
        if updated {
            let _ = self
                .append_event(
                    task_id,
                    "task.unknown",
                    serde_json::json!({
                        "reason": "effect_outcome_unknown",
                        "next": "query the external status or idempotency interface, then resume or review",
                    }),
                    true,
                )
                .await;
        }
    }
}
