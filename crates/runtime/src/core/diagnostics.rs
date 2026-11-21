//! Operator diagnostic composition from existing runtime evidence.

use crate::{Result, Runtime};
use common::{
    scopes, DiagnosticApproval, DiagnosticBundle, DiagnosticCorrelation, DiagnosticDelegation,
    DiagnosticFinding, DiagnosticStorage, DiagnosticTask, Principal, TaskState,
};

impl Runtime {
    /// Compose one diagnostic bundle from existing status, audit, trace, task,
    /// delegation, and approval evidence.
    ///
    /// This does not execute work, inspect native objects, or invent a second
    /// execution authority. Prompt text is never included.
    pub async fn diagnostics(
        &self,
        principal: &Principal,
        task_id: Option<&str>,
    ) -> Result<DiagnosticBundle> {
        principal.require(scopes::TASK_READ)?;
        let health = self.health().await;
        let task = match task_id {
            Some(id) if !id.is_empty() => Some(self.task(principal, id).await?),
            _ => None,
        };
        let namespace = principal.namespace.clone();
        let selected_task = task.clone();
        let redactor = self.redactor();
        let redact = |text: &str| redactor.text(text);
        let (approvals, delegations, hook_failures, audit, traces) = self
            .store()
            .read(|state| {
                let approvals = state
                    .approvals
                    .values()
                    .filter(|approval| approval.namespace == namespace)
                    .filter(|approval| {
                        selected_task
                            .as_ref()
                            .is_none_or(|task| approval.task_id == task.id)
                    })
                    .filter(|approval| approval.status == "pending")
                    .map(|approval| DiagnosticApproval {
                        id: approval.id.clone(),
                        operation: approval.operation.clone(),
                        digest: approval.digest.clone(),
                        status: approval.status.clone(),
                        risk: redact(&approval.risk),
                        expires_at: approval.expires_at,
                        created_at: approval.created_at,
                    })
                    .collect::<Vec<_>>();
                let mut delegations = state
                    .peer_delegations
                    .values()
                    .filter(|binding| {
                        selected_task
                            .as_ref()
                            .is_none_or(|task| binding.parent_task_id.as_deref() == Some(&task.id))
                    })
                    .filter(|binding| binding.namespace == namespace)
                    .map(|binding| DiagnosticDelegation {
                        id: binding.id.clone(),
                        peer: binding.peer.clone(),
                        state: binding.state.clone(),
                        remote_task_id: binding.remote_task_id.clone(),
                        trace_id: binding.trace_id.clone(),
                        error_code: binding.last_error.as_ref().map(|error| error.code.clone()),
                        error_message: binding
                            .last_error
                            .as_ref()
                            .map(|error| redact(&error.message)),
                    })
                    .collect::<Vec<_>>();
                // A failed child may be recorded on the Master plan before a
                // durable A2A binding exists (for example, endpoint resolution
                // failed). The plan remains the actionable evidence owner.
                if let Some(task) = &selected_task {
                    for plan in state.plans.values().filter(|plan| {
                        plan.parent_task_id == task.id && plan.namespace == namespace
                    }) {
                        for child in &plan.children {
                            let Some(error) = &child.last_error else {
                                continue;
                            };
                            delegations.push(DiagnosticDelegation {
                                id: format!("{}:{}", plan.id, child.id),
                                peer: child
                                    .member
                                    .clone()
                                    .or_else(|| child.peer.clone())
                                    .unwrap_or_default(),
                                state: child.state.clone(),
                                remote_task_id: child.remote_task_id.clone(),
                                trace_id: Some(task.trace_id.clone())
                                    .filter(|trace| !trace.is_empty()),
                                error_code: Some(error.code.clone()),
                                error_message: Some(redact(&error.message)),
                            });
                        }
                    }
                }
                let mut hook_failures = Vec::new();
                if let Some(task) = &selected_task {
                    if let Some(events) = state.events.get(&task.id) {
                        for event in events.iter().rev().take(200) {
                            match event.kind.as_str() {
                                "hook.after_task_failed" | "hook.before_task_failed" => {
                                    hook_failures.push(DiagnosticFinding {
                                        kind: "hook_failure".into(),
                                        source: event.kind.clone(),
                                        code: event
                                            .payload
                                            .get("code")
                                            .and_then(|code| code.as_str())
                                            .unwrap_or("hook.failed")
                                            .to_string(),
                                        message: event
                                            .payload
                                            .get("message")
                                            .and_then(|message| message.as_str())
                                            .map(redact)
                                            .unwrap_or_default(),
                                        action:
                                            "inspect the hook configuration and task error; the native outcome was committed before this observation"
                                                .into(),
                                        trace_id: event.trace_id.clone(),
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                }
                let audit = state
                    .audit
                    .iter()
                    .rev()
                    .filter(|record| {
                        selected_task
                            .as_ref()
                            .is_none_or(|task| record.target == task.id)
                    })
                    .take(50)
                    .map(|record| common::AuditEntry {
                        seq: record.seq,
                        at: record.at,
                        principal: redact(&record.principal),
                        action: record.action.clone(),
                        decision: record.outcome.clone(),
                        reason: redact(&record.detail),
                        resource: redact(&record.target),
                        source: record.source.clone(),
                        policy_revision: record.policy_revision,
                        trace_id: Some(record.correlation.clone())
                            .filter(|trace| !trace.is_empty()),
                    })
                    .collect::<Vec<_>>();
                let traces = state
                    .traces
                    .iter()
                    .rev()
                    .filter(|sample| {
                        selected_task
                            .as_ref()
                            .is_none_or(|task| sample.task_id == task.id)
                    })
                    .take(100)
                    .cloned()
                    .collect::<Vec<_>>();
                (approvals, delegations, hook_failures, audit, traces)
            })
            .await;

        let task = task.map(|task| DiagnosticTask {
            task_id: task.id.clone(),
            state: task.state,
            attempt: task.attempt,
            trace_id: Some(task.trace_id.clone()).filter(|trace| !trace.is_empty()),
            profile: task.profile.clone(),
            profile_revision: task.profile_revision,
            asset_revision: task.asset_revision,
            traffic: task.traffic.clone(),
            native_invocation_id: task.native_invocation_id.clone(),
            error: task.error.clone(),
            delegations,
            approvals: approvals.clone(),
        });
        let mut findings = hook_failures;
        if let Some(task) = &task {
            findings.extend(task_findings(task));
        }
        if !health.degraded.is_empty() {
            findings.push(DiagnosticFinding {
                kind: "degraded_storage".into(),
                source: "runtime".into(),
                code: "storage.degraded".into(),
                message: redact(&health.degraded.join(", ")),
                action: "inspect storage pressure and the reported degradation before accepting more work"
                    .into(),
                trace_id: None,
            });
        }
        if health.capacity.limit > 0
            && health.capacity.active >= health.capacity.limit
            && health.capacity.queued > 0
        {
            findings.push(DiagnosticFinding {
                kind: "saturation".into(),
                source: "admission".into(),
                code: "capacity.saturated".into(),
                message: format!(
                    "{} active and {} queued against a limit of {}",
                    health.capacity.active, health.capacity.queued, health.capacity.limit
                ),
                action: "wait for admitted work to drain or raise the operator limit".into(),
                trace_id: None,
            });
        }
        if health.lifecycle == common::LifecyclePhase::Draining {
            findings.push(DiagnosticFinding {
                kind: "recovery".into(),
                source: "lifecycle".into(),
                code: "lifecycle.draining".into(),
                message: "new admission is closed while admitted work drains".into(),
                action: "wait for the drain report before restarting the process".into(),
                trace_id: None,
            });
        }
        let storage = DiagnosticStorage {
            metadata: if health.degraded.is_empty() {
                "ready".into()
            } else {
                "degraded".into()
            },
            artifacts: "native_adk".into(),
            degraded: health.degraded.clone(),
        };
        let correlation = DiagnosticCorrelation {
            trace_id: task.as_ref().and_then(|task| task.trace_id.clone()),
            task_id: task.as_ref().map(|task| task.task_id.clone()),
            attempt: task.as_ref().map(|task| task.attempt).unwrap_or(0),
            native_invocation_id: task
                .as_ref()
                .and_then(|task| task.native_invocation_id.clone()),
        };
        let subsystems = health.subsystems.clone();
        let metrics = health.metrics.clone();
        Ok(DiagnosticBundle {
            generated_at: common::now_ms(),
            health,
            metrics,
            storage,
            findings,
            task,
            correlation,
            audit,
            traces,
            subsystems,
        })
    }
}

fn task_findings(task: &DiagnosticTask) -> Vec<DiagnosticFinding> {
    let mut findings = Vec::new();
    if let Some(error) = &task.error {
        let (kind, source) = if error.code.starts_with("policy.") {
            ("hook_failure", "worker_hook")
        } else {
            ("native_error", "task")
        };
        findings.push(DiagnosticFinding {
            kind: kind.into(),
            source: source.into(),
            code: error.code.clone(),
            message: common::redact(&error.message),
            action: if task.native_invocation_id.is_none() {
                "the failure occurred before a native invocation; inspect the owning policy or hook"
                    .into()
            } else {
                "inspect the retained native error; do not resubmit an uncertain side effect".into()
            },
            trace_id: task.trace_id.clone(),
        });
    }
    if task.state == TaskState::Recovering || task.state == TaskState::Unknown {
        findings.push(DiagnosticFinding {
            kind: "recovery".into(),
            source: "task".into(),
            code: "task.recovery_required".into(),
            message: format!("task is in state {}", state_name(task.state)),
            action: "query retained child handles and effect receipts before resubmitting".into(),
            trace_id: task.trace_id.clone(),
        });
    }
    for approval in &task.approvals {
        findings.push(DiagnosticFinding {
            kind: "blocked_approval".into(),
            source: "approval".into(),
            code: "approval.pending".into(),
            message: format!(
                "{} is waiting for an exact {} decision",
                approval.operation, approval.digest
            ),
            action: "inspect the approval through the SDK and decide the exact digest".into(),
            trace_id: task.trace_id.clone(),
        });
    }
    for delegation in &task.delegations {
        if let Some(code) = &delegation.error_code {
            findings.push(DiagnosticFinding {
                kind: "unreachable_child".into(),
                source: "delegation".into(),
                code: code.clone(),
                message: delegation
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "the child did not confirm a result".into()),
                action: "inspect the retained child handle and reattach after the child returns"
                    .into(),
                trace_id: delegation
                    .trace_id
                    .clone()
                    .or_else(|| task.trace_id.clone()),
            });
        }
    }
    findings
}

fn state_name(state: TaskState) -> &'static str {
    match state {
        TaskState::Accepted => "accepted",
        TaskState::Queued => "queued",
        TaskState::Running => "running",
        TaskState::WaitingApproval => "waiting_approval",
        TaskState::Cancelling => "cancelling",
        TaskState::Recovering => "recovering",
        TaskState::Unknown => "unknown",
        TaskState::Succeeded => "succeeded",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
    }
}
