//! Exact, digest-bound approvals for protected operations.
//!
//! Approval state belongs to the Worker Instance that owns the protected
//! effect. A request names one task, native operation, exact target, canonical
//! argument digest, risk note, and expiry. A decision can only be made by the
//! task caller or an administrator in the task namespace, and it never grants
//! authority beyond the Worker's configured effect policy.

use crate::{new_id, now, Result, Runtime};
use common::{scopes, ApiError, Approval, ApprovalDecision, Principal, TaskState};
use std::time::Duration;

impl Runtime {
    /// Request approval for one exact operation. The task pauses before its
    /// effect and the native checkpoint is expected to retain the rest.
    ///
    /// The request is idempotent for the same task, native operation, and
    /// digest. A repeated dispatch cannot create a second approval, and a
    /// previous rejection, revocation, or expiry is returned unchanged so a
    /// caller cannot silently replace the recorded outcome with a new request.
    pub async fn request_approval(
        &self,
        task_id: &str,
        operation: &str,
        target: &str,
        digest: &str,
        risk: &str,
        ttl_ms: i64,
    ) -> Result<Approval> {
        let at = now();
        let (approval, created) = self
            .store()
            .write(|state| {
                let task = state
                    .tasks
                    .get_mut(task_id)
                    .ok_or_else(|| ApiError::new("task.not_found", "task does not exist"))?;
                if task.state.is_terminal() {
                    return Err(ApiError::new(
                        "approval.task_finished",
                        "task is already terminal",
                    ));
                }
                if let Some(existing) = state
                    .approvals
                    .values()
                    .filter(|approval| {
                        approval.task_id == task_id
                            && approval.operation == operation
                            && approval.digest == digest
                    })
                    .max_by_key(|approval| approval.created_at)
                    .cloned()
                {
                    // Preserve the recorded outcome. Only a pending request may
                    // be re-shown; it keeps its original expiry.
                    if existing.status == "pending" {
                        task.state = TaskState::WaitingApproval;
                        task.updated_at = at;
                    }
                    return Ok((existing, false));
                }
                let approval = Approval {
                    id: new_id("approval"),
                    task_id: task_id.to_string(),
                    namespace: task.namespace.clone(),
                    operation: operation.to_string(),
                    target: target.to_string(),
                    digest: digest.to_string(),
                    risk: risk.to_string(),
                    requested_by: task.caller.clone(),
                    expires_at: at + ttl_ms.max(1),
                    status: "pending".into(),
                    decided_by: None,
                    decided_at: None,
                    receipt: None,
                    answer: None,
                    created_at: at,
                };
                task.state = TaskState::WaitingApproval;
                task.updated_at = at;
                state
                    .approvals
                    .insert(approval.id.clone(), approval.clone());
                Ok((approval, true))
            })
            .await?;
        if !created {
            return Ok(approval);
        }
        self.append_event(
            task_id,
            "approval.requested",
            serde_json::json!({
                "approval_id": approval.id,
                "operation": approval.operation,
                "target": approval.target,
                "digest": approval.digest,
                "expires_at": approval.expires_at,
                "risk": approval.risk,
                "requested_by": approval.requested_by,
            }),
            true,
        )
        .await?;
        self.set_pause(task_id, crate::PAUSE_WAITING).await;
        self.approval_notify().notify_waiters();
        Ok(approval)
    }

    /// Find the recorded approval for one exact native operation and digest.
    pub async fn approval_for_operation(
        &self,
        task_id: &str,
        operation: &str,
        digest: &str,
    ) -> Option<Approval> {
        self.store()
            .read(|state| {
                state
                    .approvals
                    .values()
                    .filter(|approval| {
                        approval.task_id == task_id
                            && approval.operation == operation
                            && approval.digest == digest
                    })
                    .max_by_key(|approval| approval.created_at)
                    .cloned()
            })
            .await
    }

    /// Read an approval visible to the caller.
    pub async fn approval(&self, principal: &Principal, approval_id: &str) -> Result<Approval> {
        principal.require(scopes::APPROVAL_READ)?;
        let approval = self
            .store()
            .read(|state| state.approvals.get(approval_id).cloned())
            .await
            .ok_or_else(|| ApiError::new("approval.not_found", "approval is not visible"))?;
        principal.require_namespace(&approval.namespace)?;
        Ok(approval)
    }

    /// List approvals in the caller's namespace.
    pub async fn approvals(
        &self,
        principal: &Principal,
        pending_only: bool,
    ) -> Result<Vec<Approval>> {
        principal.require(scopes::APPROVAL_READ)?;
        let namespace = principal.namespace.clone();
        let mut approvals: Vec<Approval> = self
            .store()
            .read(|state| {
                state
                    .approvals
                    .values()
                    .filter(|approval| approval.namespace == namespace)
                    .filter(|approval| !pending_only || approval.status == "pending")
                    .cloned()
                    .collect()
            })
            .await;
        approvals.sort_by_key(|approval| approval.created_at);
        Ok(approvals)
    }

    /// Decide an approval. The digest must match the exact operation the
    /// approver saw. Repeating the same decision is idempotent and does not
    /// duplicate the decision event or audit record.
    pub async fn decide_approval(
        &self,
        principal: &Principal,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<Approval> {
        principal.require(scopes::APPROVAL_DECIDE)?;
        if decision.decision != "approved" && decision.decision != "rejected" {
            return Err(ApiError::new(
                "validation.approval_decision",
                "decision must be 'approved' or 'rejected'",
            ));
        }
        let (updated, changed) =
            self.store()
                .write(|state| {
                    let approval = state.approvals.get(approval_id).cloned().ok_or_else(|| {
                        ApiError::new("approval.not_found", "approval is not visible")
                    })?;
                    principal.require_namespace(&approval.namespace)?;
                    let task = state.tasks.get(&approval.task_id).ok_or_else(|| {
                        ApiError::new("task.not_found", "approval task is missing")
                    })?;
                    if task.caller != principal.id && !principal.has(scopes::ADMIN) {
                        return Err(ApiError::new(
                            "scope.approval_owner",
                            "approval belongs to another caller",
                        ));
                    }
                    if approval.digest != decision.digest {
                        return Err(ApiError::new(
                            "approval.digest_mismatch",
                            "decision digest does not match the requested operation",
                        ));
                    }
                    let approval = state.approvals.get_mut(approval_id).expect("checked above");
                    if approval.status != "pending" {
                        if approval.status == decision.decision {
                            return Ok((approval.clone(), false));
                        }
                        if approval.status == "expired" {
                            return Err(ApiError::new(
                                "approval.expired",
                                "approval expired before the decision",
                            ));
                        }
                        if approval.status == "revoked" {
                            return Err(ApiError::new(
                                "approval.revoked",
                                "approval authority was revoked before the decision",
                            ));
                        }
                        return Err(ApiError::new(
                            "approval.conflict",
                            "approval already has a different decision",
                        ));
                    }
                    if now() >= approval.expires_at {
                        approval.status = "expired".into();
                        return Err(ApiError::new(
                            "approval.expired",
                            "approval expired before the decision",
                        ));
                    }
                    approval.status = decision.decision.clone();
                    approval.decided_by = Some(principal.id.clone());
                    approval.decided_at = Some(now());
                    // Only a user question carries an answer. A permission
                    // approval cannot smuggle extra payload into its record.
                    approval.answer = (approval.operation == "user.ask"
                        && decision.decision == "approved")
                        .then(|| decision.answer.clone())
                        .flatten()
                        .map(|answer| {
                            let mut answer = answer;
                            answer.truncate(4096);
                            answer
                        });
                    Ok((approval.clone(), true))
                })
                .await?;
        if !changed {
            // A repeated decision is a replay of the recorded outcome. It may
            // still finish a resume that a crash interrupted after the durable
            // decision but before the native workflow continued.
            if updated.status == "approved" {
                self.resume_decided_approval(principal, &updated).await;
            }
            return Ok(updated);
        }
        crate::core::fault::exit_if(crate::core::fault::Point::AfterApprovalDecision);
        self.append_event(
            &updated.task_id,
            if updated.status == "approved" {
                "approval.approved"
            } else {
                "approval.rejected"
            },
            serde_json::json!({
                "approval_id": updated.id,
                "decided_by": updated.decided_by,
                "digest": updated.digest,
            }),
            true,
        )
        .await?;
        self.audit(
            &principal.id,
            "approval.decision",
            approval_id,
            &updated.status,
            &updated.operation,
        )
        .await?;

        if updated.status == "approved" {
            self.resume_decided_approval(principal, &updated).await;
        } else {
            // A denial ends the native confirmation wait, not the native turn.
            // Restore the execution slot so the denial response can be
            // delivered and the task can reach its truthful outcome.
            self.set_pause(&updated.task_id, crate::PAUSE_RUNNING).await;
            self.approval_notify().notify_waiters();
        }
        Ok(updated)
    }

    /// Resume the exact native workflow that was paused for this approval.
    ///
    /// An in-process native confirmation continues on the same invocation. A
    /// task that crossed a process boundary is resumed from its committed
    /// native graph checkpoint under the original identity. Unsupported work
    /// stays recovering with an explicit event instead of being reported as
    /// approved execution.
    async fn resume_decided_approval(&self, _principal: &Principal, approval: &Approval) {
        let task = self.task_unscoped(&approval.task_id).await;
        let state = task.as_ref().map(|task| task.state);
        match state {
            Some(TaskState::WaitingApproval) | Some(TaskState::Running) => {
                let _ = self
                    .store()
                    .write(|state| {
                        if let Some(task) = state.tasks.get_mut(&approval.task_id) {
                            if task.state == TaskState::WaitingApproval {
                                task.state = TaskState::Running;
                                task.updated_at = now();
                            }
                        }
                        Ok(())
                    })
                    .await;
                self.set_pause(&approval.task_id, crate::PAUSE_RUNNING)
                    .await;
            }
            Some(TaskState::Recovering) | Some(TaskState::Unknown) => {
                // The decision is the authorization to continue this same task.
                // Resume is revalidated against the task's pinned profile and
                // stored caller identity, never against a wider Client scope.
                let Some(task) = task else {
                    return;
                };
                let caller = Principal::new(
                    task.caller.clone(),
                    task.namespace.clone(),
                    &[scopes::TASK_SUBMIT],
                );
                if let Err(error) = self.resume_task(&caller, &approval.task_id).await {
                    let _ = self
                        .append_event(
                            &approval.task_id,
                            "approval.resume_unavailable",
                            serde_json::json!({
                                "approval_id": approval.id,
                                "operation": approval.operation,
                                "code": error.code,
                                "message": common::redact(&error.message),
                            }),
                            true,
                        )
                        .await;
                }
            }
            _ => {}
        }
        self.approval_notify().notify_waiters();
    }

    /// Revoke a pending approval. The exact operation stays blocked; the
    /// recorded revocation is not replaced by a later decision.
    pub async fn revoke_approval(
        &self,
        principal: &Principal,
        approval_id: &str,
    ) -> Result<Approval> {
        principal.require(scopes::APPROVAL_DECIDE)?;
        let updated =
            self.store()
                .write(|state| {
                    let approval = state.approvals.get_mut(approval_id).ok_or_else(|| {
                        ApiError::new("approval.not_found", "approval is not visible")
                    })?;
                    principal.require_namespace(&approval.namespace)?;
                    let task = state.tasks.get(&approval.task_id).ok_or_else(|| {
                        ApiError::new("task.not_found", "approval task is missing")
                    })?;
                    if task.caller != principal.id && !principal.has(scopes::ADMIN) {
                        return Err(ApiError::new(
                            "scope.approval_owner",
                            "approval belongs to another caller",
                        ));
                    }
                    match approval.status.as_str() {
                        "revoked" => return Ok(approval.clone()),
                        "pending" => {}
                        _ => {
                            return Err(ApiError::new(
                                "approval.conflict",
                                "a decided approval cannot be revoked",
                            ))
                        }
                    }
                    approval.status = "revoked".into();
                    approval.decided_by = Some(principal.id.clone());
                    approval.decided_at = Some(now());
                    Ok(approval.clone())
                })
                .await?;
        self.append_event(
            &updated.task_id,
            "approval.revoked",
            serde_json::json!({
                "approval_id": updated.id,
                "revoked_by": updated.decided_by,
                "digest": updated.digest,
            }),
            true,
        )
        .await?;
        self.audit(
            &principal.id,
            "approval.revoke",
            approval_id,
            "revoked",
            &updated.operation,
        )
        .await?;
        self.set_pause(&updated.task_id, crate::PAUSE_RUNNING).await;
        self.approval_notify().notify_waiters();
        Ok(updated)
    }

    /// Return a task to `running` after a bounded question wait ends without a
    /// decision. The approval record stays pending until its own expiry.
    pub(crate) async fn resume_after_question(&self, task_id: &str) {
        let _ = self
            .store()
            .write(|state| {
                let Some(task) = state.tasks.get_mut(task_id) else {
                    return Ok(());
                };
                if task.state == TaskState::WaitingApproval {
                    task.state = TaskState::Running;
                    task.updated_at = now();
                }
                Ok(())
            })
            .await;
        self.set_pause(task_id, crate::PAUSE_RUNNING).await;
    }

    /// Wait for a decision without holding an execution permit. The caller
    /// must have already paused the native workflow.
    pub async fn wait_for_approval(
        &self,
        approval_id: &str,
        timeout: Duration,
    ) -> Result<Approval> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let approval = self
                .store()
                .read(|state| state.approvals.get(approval_id).cloned())
                .await
                .ok_or_else(|| ApiError::new("approval.not_found", "approval disappeared"))?;
            if approval.status != "pending" {
                return Ok(approval);
            }
            if now() >= approval.expires_at {
                let expired = self
                    .store()
                    .write(|state| {
                        let Some(approval) = state.approvals.get_mut(approval_id) else {
                            return Ok(false);
                        };
                        if approval.status != "pending" {
                            return Ok(false);
                        }
                        approval.status = "expired".into();
                        Ok(true)
                    })
                    .await
                    .unwrap_or(false);
                if expired {
                    let _ = self
                        .append_event(
                            &approval.task_id,
                            "approval.expired",
                            serde_json::json!({"approval_id": approval.id}),
                            true,
                        )
                        .await;
                }
                return Err(ApiError::new(
                    "approval.expired",
                    "approval expired while waiting",
                ));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(ApiError::new(
                    "approval.timeout",
                    "approval wait exceeded the caller timeout",
                ));
            }
            let _ = tokio::time::timeout(remaining.min(Duration::from_secs(1)), async {
                self.approval_notify().notified().await;
            })
            .await;
        }
    }

    /// Write the receipt of the protected effect.
    pub async fn record_approval_receipt(&self, approval_id: &str, receipt: &str) -> Result<()> {
        let updated =
            self.store()
                .write(|state| {
                    let approval = state.approvals.get_mut(approval_id).ok_or_else(|| {
                        ApiError::new("approval.not_found", "approval disappeared")
                    })?;
                    approval.receipt = Some(common::redact(receipt));
                    Ok(approval.clone())
                })
                .await?;
        self.append_event(
            &updated.task_id,
            "approval.receipt",
            serde_json::json!({
                "approval_id": updated.id,
                "operation": updated.operation,
                "receipt": updated.receipt,
            }),
            true,
        )
        .await
        .map(|_| ())
    }

    /// Expire pending approvals whose deadline passed.
    pub async fn expire_approvals(&self) -> usize {
        let expired = self
            .store()
            .write(|state| {
                let now = now();
                let mut ids = Vec::new();
                for approval in state.approvals.values_mut() {
                    if approval.status == "pending" && now >= approval.expires_at {
                        approval.status = "expired".into();
                        ids.push(approval.id.clone());
                    }
                }
                Ok(ids)
            })
            .await
            .unwrap_or_default();
        for id in &expired {
            let task_id = self
                .store()
                .read(|state| {
                    state
                        .approvals
                        .get(id)
                        .map(|approval| approval.task_id.clone())
                        .unwrap_or_default()
                })
                .await;
            let _ = self
                .append_event(
                    &task_id,
                    "approval.expired",
                    serde_json::json!({"approval_id": id}),
                    true,
                )
                .await;
        }
        if !expired.is_empty() {
            self.set_pause_for_expired(&expired).await;
        }
        self.approval_notify().notify_waiters();
        expired.len()
    }

    async fn set_pause_for_expired(&self, ids: &[String]) {
        let tasks = self
            .store()
            .read(|state| {
                state
                    .approvals
                    .values()
                    .filter(|approval| ids.contains(&approval.id))
                    .map(|approval| approval.task_id.clone())
                    .collect::<Vec<_>>()
            })
            .await;
        for task_id in tasks {
            self.set_pause(&task_id, crate::PAUSE_RUNNING).await;
        }
    }
}
