//! Authorized approval routing to the Worker that owns the protected effect.
//!
//! The Gateway checks caller visibility, Worker approval scope, task ownership,
//! and the exact operation digest before forwarding. It stores no approval
//! decision: the Worker remains the authority for expiry, revalidation, resume,
//! and the protected effect. Only redacted routing decisions are audited.

use crate::caller::Caller;
use crate::store::{Connectivity, Submission};
use common::{
    scopes, ApiError, Approval, ApprovalDecision, GatewayApproval, Limits, WorkerSummary,
};
use serde::de::DeserializeOwned;

use super::submit::unknown_submission;
use super::worker_client::issue_worker_delegation_scopes;
use super::Gateway;

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

struct Target {
    submission: Submission,
    worker: WorkerSummary,
    worker_task_id: String,
}

impl Gateway {
    async fn approval_target(
        &self,
        caller: &Caller,
        execution_id: &str,
    ) -> Result<Target, ApiError> {
        let submission = self.submission_for(&caller.principal, execution_id).await?;
        let Some(worker_task_id) = submission.worker_task_id.clone() else {
            return Err(unknown_submission(
                &submission.execution_id,
                &submission.worker,
                "the Worker acceptance is not resolved",
            ));
        };
        let worker = self.worker(&submission.worker)?;
        Ok(Target {
            submission,
            worker,
            worker_task_id,
        })
    }

    fn approval_delegation(
        &self,
        target: &Target,
        caller: &Caller,
        scopes: &[&str],
    ) -> Result<String, ApiError> {
        issue_worker_delegation_scopes(
            &self.state.secret,
            &target.worker,
            &caller.principal,
            &target.submission.agent,
            scopes,
            &Limits::default(),
        )
    }

    fn audit_approval(
        &self,
        caller: &Caller,
        worker: &str,
        action: &str,
        outcome: &str,
        detail: &str,
    ) {
        self.state.registry.audit(
            self.state.registry.now(),
            action,
            worker,
            &caller.principal.id,
            outcome,
            &common::redact(detail),
        );
    }

    fn denied(&self, caller: &Caller, worker: &str, error: ApiError) -> ApiError {
        self.audit_approval(caller, worker, "approval.denied", "denied", &error.code);
        error
    }

    async fn approval_target_or_denied(
        &self,
        caller: &Caller,
        execution_id: &str,
    ) -> Result<Target, ApiError> {
        match self.approval_target(caller, execution_id).await {
            Ok(target) => Ok(target),
            Err(error) => Err(self.denied(caller, "unresolved", error)),
        }
    }

    async fn mark_approval_unreachable(&self, execution_id: &str, detail: &str) {
        let _ = self
            .state
            .dispatch
            .mark_connectivity(
                execution_id,
                Connectivity::Unreachable,
                "approval_unreachable",
                detail,
            )
            .await;
    }

    async fn approval_from_worker<T: DeserializeOwned>(
        &self,
        execution_id: &str,
        worker: &WorkerSummary,
        path: &str,
        token: &str,
    ) -> Result<T, ApiError> {
        match self.state.worker_get::<T>(worker, path, token).await {
            Ok(value) => Ok(value),
            Err(error) => {
                if error.code == "route.worker_unavailable" {
                    self.mark_approval_unreachable(
                        execution_id,
                        "approval state could not be read from the owning Worker",
                    )
                    .await;
                }
                Err(error)
            }
        }
    }

    /// List approvals the caller may see for one routed task.
    ///
    /// The Worker filters by authenticated namespace; the Gateway additionally
    /// binds every returned record to the routed Worker task, so another task's
    /// approval cannot appear through this execution id.
    pub async fn approvals(
        &self,
        caller: &Caller,
        execution_id: &str,
        pending_only: bool,
    ) -> Result<Vec<GatewayApproval>, ApiError> {
        let target = self.approval_target_or_denied(caller, execution_id).await?;
        if let Err(error) = caller.principal.require(scopes::APPROVAL_READ) {
            return Err(self.denied(caller, &target.worker.id, error));
        }
        let token = self.approval_delegation(&target, caller, &[scopes::APPROVAL_READ])?;
        let path = format!("/v1/approvals?pending={pending_only}");
        let approvals: Vec<Approval> = self
            .approval_from_worker(execution_id, &target.worker, &path, &token)
            .await?;
        Ok(approvals
            .into_iter()
            .filter(|approval| approval.task_id == target.worker_task_id)
            .map(|approval| GatewayApproval {
                task_id: execution_id.to_string(),
                worker: target.worker.id.clone(),
                approval,
            })
            .collect())
    }

    /// Read one exact approval for the routed task.
    pub async fn approval(
        &self,
        caller: &Caller,
        execution_id: &str,
        approval_id: &str,
    ) -> Result<GatewayApproval, ApiError> {
        let target = self.approval_target_or_denied(caller, execution_id).await?;
        if let Err(error) = caller.principal.require(scopes::APPROVAL_READ) {
            return Err(self.denied(caller, &target.worker.id, error));
        }
        let token = self.approval_delegation(
            &target,
            caller,
            &[scopes::APPROVAL_READ, scopes::APPROVAL_DECIDE],
        )?;
        let path = format!("/v1/approvals/{}", encode(approval_id));
        let approval: Approval = self
            .approval_from_worker(execution_id, &target.worker, &path, &token)
            .await?;
        self.check_approval_owner(caller, &target, &approval)?;
        Ok(GatewayApproval {
            task_id: execution_id.to_string(),
            worker: target.worker.id.clone(),
            approval,
        })
    }

    /// Decide one approval with the digest the approver observed.
    ///
    /// The Gateway reads the Worker-owned record first, rejects a foreign task,
    /// namespace, or changed digest, then forwards the unchanged decision. A
    /// repeated delivery is forwarded again so the Worker can replay its
    /// recorded outcome after a lost acknowledgment or Gateway restart.
    pub async fn decide_approval(
        &self,
        caller: &Caller,
        execution_id: &str,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<GatewayApproval, ApiError> {
        let target = self.approval_target_or_denied(caller, execution_id).await?;
        if let Err(error) = caller.principal.require(scopes::APPROVAL_READ) {
            return Err(self.denied(caller, &target.worker.id, error));
        }
        if let Err(error) = caller.principal.require(scopes::APPROVAL_DECIDE) {
            return Err(self.denied(caller, &target.worker.id, error));
        }
        if decision.decision != "approved" && decision.decision != "rejected" {
            return Err(self.denied(
                caller,
                &target.worker.id,
                ApiError::new(
                    "validation.approval_decision",
                    "decision must be 'approved' or 'rejected'",
                ),
            ));
        }
        let token = self.approval_delegation(
            &target,
            caller,
            &[scopes::APPROVAL_READ, scopes::APPROVAL_DECIDE],
        )?;
        let current: Approval = self
            .approval_from_worker(
                execution_id,
                &target.worker,
                &format!("/v1/approvals/{}", encode(approval_id)),
                &token,
            )
            .await?;
        self.check_approval_owner(caller, &target, &current)?;
        if current.digest != decision.digest {
            return Err(self.denied(
                caller,
                &target.worker.id,
                ApiError::new(
                    "approval.digest_mismatch",
                    "decision digest does not match the requested operation",
                ),
            ));
        }
        let updated: Approval = match self
            .state
            .worker_post::<Approval, _>(
                &target.worker,
                &format!("/v1/approvals/{}/decision", encode(approval_id)),
                &token,
                &decision,
            )
            .await
        {
            Ok(updated) => updated,
            Err(error) => {
                if error.code == "route.worker_unavailable" {
                    self.mark_approval_unreachable(
                        execution_id,
                        "effective decision was not acknowledged by the owning Worker",
                    )
                    .await;
                }
                return Err(self.denied(caller, &target.worker.id, error));
            }
        };
        if updated.task_id != target.worker_task_id {
            return Err(self.denied(
                caller,
                &target.worker.id,
                ApiError::new(
                    "route.invalid_worker_response",
                    "Worker returned an approval for a different task",
                ),
            ));
        }
        crate::fault::exit_if(crate::fault::Point::AfterApprovalForward);
        self.audit_approval(
            caller,
            &target.worker.id,
            "approval.decision",
            "accepted",
            &updated.status,
        );
        Ok(GatewayApproval {
            task_id: execution_id.to_string(),
            worker: target.worker.id.clone(),
            approval: updated,
        })
    }

    /// Revoke a pending approval while it remains undecided.
    pub async fn revoke_approval(
        &self,
        caller: &Caller,
        execution_id: &str,
        approval_id: &str,
    ) -> Result<GatewayApproval, ApiError> {
        let target = self.approval_target_or_denied(caller, execution_id).await?;
        if let Err(error) = caller.principal.require(scopes::APPROVAL_READ) {
            return Err(self.denied(caller, &target.worker.id, error));
        }
        if let Err(error) = caller.principal.require(scopes::APPROVAL_DECIDE) {
            return Err(self.denied(caller, &target.worker.id, error));
        }
        let token = self.approval_delegation(
            &target,
            caller,
            &[scopes::APPROVAL_READ, scopes::APPROVAL_DECIDE],
        )?;
        let current: Approval = self
            .approval_from_worker(
                execution_id,
                &target.worker,
                &format!("/v1/approvals/{}", encode(approval_id)),
                &token,
            )
            .await?;
        self.check_approval_owner(caller, &target, &current)?;
        let updated: Approval = match self
            .state
            .worker_post::<Approval, _>(
                &target.worker,
                &format!("/v1/approvals/{}/revoke", encode(approval_id)),
                &token,
                &serde_json::json!({}),
            )
            .await
        {
            Ok(updated) => updated,
            Err(error) => return Err(self.denied(caller, &target.worker.id, error)),
        };
        if updated.task_id != target.worker_task_id {
            return Err(self.denied(
                caller,
                &target.worker.id,
                ApiError::new(
                    "route.invalid_worker_response",
                    "Worker returned an approval for a different task",
                ),
            ));
        }
        self.audit_approval(
            caller,
            &target.worker.id,
            "approval.revoke",
            "accepted",
            &updated.status,
        );
        Ok(GatewayApproval {
            task_id: execution_id.to_string(),
            worker: target.worker.id,
            approval: updated,
        })
    }

    fn check_approval_owner(
        &self,
        caller: &Caller,
        target: &Target,
        approval: &Approval,
    ) -> Result<(), ApiError> {
        if approval.task_id != target.worker_task_id
            || approval.namespace != caller.principal.namespace
        {
            return Err(self.denied(
                caller,
                &target.worker.id,
                ApiError::new("approval.not_found", "approval is not visible"),
            ));
        }
        Ok(())
    }
}
