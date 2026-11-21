//! Startup reconciliation for tasks left non-terminal.

use crate::core::ids::now;
use crate::{Result, Runtime};
use common::TaskState;

/// Work resumed or flagged during startup recovery.
#[derive(Debug, Clone, Default)]
pub struct Recovery {
    /// Durably accepted tasks whose execution was dispatched again.
    pub resumed: Vec<String>,
    /// Tasks whose native outcome is uncertain and must be reconciled.
    pub uncertain: Vec<String>,
}

impl Runtime {
    /// Plan recovery for tasks left non-terminal by a previous incarnation.
    ///
    /// This records the truthful projection and durable events but does not
    /// dispatch anything. Startup runs the package startup hook after this
    /// plan and only then resumes accepted work, so "recovered work ran" can
    /// never precede "startup hook failed".
    pub async fn recover_plan(&self) -> Result<Recovery> {
        let at = now();
        let (resumable, uncertain, cancelled) = self
            .store()
            .write(|state| {
                // A lineage owner that ended without success must not leave
                // owned work resumable after a restart. Cancellation is durable;
                // recovery obeys it instead of resurrecting the child.
                let stopped: std::collections::BTreeSet<String> = state
                    .tasks
                    .values()
                    .filter(|task| task.state.is_terminal() && task.state != TaskState::Succeeded)
                    .map(|task| task.id.clone())
                    .collect();
                let mut resumable = Vec::new();
                let mut uncertain = Vec::new();
                let mut cancelled = Vec::new();
                for task in state.tasks.values_mut() {
                    if task.state.is_terminal() {
                        continue;
                    }
                    let lineage_stopped = task
                        .parent_task_id
                        .as_ref()
                        .is_some_and(|parent| stopped.contains(parent))
                        || task
                            .root_task_id
                            .as_ref()
                            .is_some_and(|root| stopped.contains(root));
                    match task.state {
                        // Admission and queue position are process-local; no
                        // native execution was requested for these identities.
                        TaskState::Accepted | TaskState::Queued if lineage_stopped => {
                            task.state = TaskState::Cancelled;
                            task.updated_at = at;
                            cancelled.push(task.id.clone());
                        }
                        TaskState::Accepted | TaskState::Queued => {
                            task.state = TaskState::Accepted;
                            task.updated_at = at;
                            resumable.push(task.id.clone());
                        }
                        // Native start may have happened. The truthful
                        // projection is uncertainty, not a retry. An approval
                        // wait also crossed a process boundary and may no
                        // longer be decidable under the same evidence.
                        TaskState::Running | TaskState::WaitingApproval | TaskState::Cancelling => {
                            task.state = TaskState::Recovering;
                            task.uncertainty = Some(
                                "process restarted before the native outcome was confirmed".into(),
                            );
                            task.updated_at = at;
                            uncertain.push(task.id.clone());
                        }
                        // Already uncertain from a forced stop. Keep the
                        // truthful projection; it was flagged when it became
                        // uncertain and needs no duplicate restart event.
                        TaskState::Recovering | TaskState::Unknown => {}
                        _ => {}
                    }
                }
                Ok((resumable, uncertain, cancelled))
            })
            .await?;

        for id in &cancelled {
            let _ = self
                .append_event(
                    id,
                    "task.cancelled",
                    serde_json::json!({"reason": "parent_terminal"}),
                    true,
                )
                .await;
        }

        for id in &uncertain {
            let _ = self
                .append_event(
                    id,
                    "task.recovery_required",
                    serde_json::json!({"reason": "process_restart"}),
                    true,
                )
                .await;
        }
        for id in &resumable {
            let _ = self
                .append_event(
                    id,
                    "task.recovered",
                    serde_json::json!({"reason": "durable_acceptance"}),
                    true,
                )
                .await;
        }
        Ok(Recovery {
            resumed: resumable,
            uncertain,
        })
    }

    /// Dispatch work that is safe to resume. Called only after startup hooks
    /// have succeeded and the process is otherwise ready.
    pub fn resume_recovered(&self, recovery: &Recovery) {
        for id in &recovery.resumed {
            let runtime = self.clone();
            let task_id = id.clone();
            tokio::spawn(async move {
                runtime.run_task(task_id).await;
            });
        }
    }

    /// Plan and immediately resume recovery for callers that do not have a
    /// package startup hook between those steps.
    pub async fn recover(&self) -> Result<Recovery> {
        let recovery = self.recover_plan().await?;
        self.resume_recovered(&recovery);
        Ok(recovery)
    }
}
