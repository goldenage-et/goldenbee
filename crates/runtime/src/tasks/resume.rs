//! Authorized resume of a task from its native graph checkpoint.

use crate::core::ids::now;
use crate::execution::{GraphResume, ResumeInspection};
use crate::{ApiError, Result, Runtime};
use common::{scopes, Principal, Task, TaskState};
use tokio_util::sync::CancellationToken;

impl Runtime {
    /// Resume one uncertain graph task from its committed native checkpoint.
    ///
    /// Resume is scoped to the original authenticated identity (or an
    /// administrator) and to the pinned profile revision. The native graph
    /// checkpointer owns the checkpoint; this method only inspects it and then
    /// either completes the task from a terminal checkpoint or re-dispatches the
    /// same external task handle so `GraphAgent` resumes the same thread.
    pub async fn resume_task(&self, principal: &Principal, task_id: &str) -> Result<Task> {
        principal.require(scopes::TASK_SUBMIT)?;
        let task = self
            .task_unscoped(task_id)
            .await
            .ok_or_else(task_not_found)?;
        // A foreign caller or namespace sees the same not-found answer as an
        // unknown id, so a resume request cannot probe task identities.
        if task.namespace != principal.namespace
            || (task.caller != principal.id && !principal.has(scopes::ADMIN))
        {
            return Err(task_not_found());
        }
        if task.state.is_terminal() {
            let _ = self
                .audit(&principal.id, "task.resume", task_id, "replayed", "")
                .await;
            return Ok(task);
        }
        if matches!(
            task.state,
            TaskState::Accepted
                | TaskState::Queued
                | TaskState::Running
                | TaskState::WaitingApproval
                | TaskState::Cancelling
        ) {
            // The same resume may be retried while work is already in flight.
            return Ok(task);
        }
        if !matches!(task.state, TaskState::Recovering | TaskState::Unknown) {
            return self
                .resume_failure(
                    &task,
                    "graph.resume_unavailable",
                    "task state is not resumable",
                    "Only a recovering or unknown native graph task can be resumed.",
                )
                .await;
        }

        self.check_resume_profile(&task).await?;
        let Some(executor) = self.executor().await else {
            return self
                .resume_failure(
                    &task,
                    "execution.unavailable",
                    "no native executor is configured in this process",
                    "Start the Worker with its native executor before resuming.",
                )
                .await;
        };

        match executor.inspect_resume(&task).await {
            ResumeInspection::Missing => {
                self.resume_failure(
                    &task,
                    "graph.checkpoint_missing",
                    &format!(
                        "no native graph checkpoint exists for thread '{}'",
                        task.session_id
                    ),
                    "Inspect the task and its data directory. A missing checkpoint cannot be turned into a fresh run under the same task handle.",
                )
                .await
            }
            ResumeInspection::Corrupt { detail } => {
                self.resume_failure(
                    &task,
                    "graph.checkpoint_corrupt",
                    &format!("native graph checkpoint cannot be read: {detail}"),
                    "Quarantine the task, inspect or restore the native checkpoint store, then retry the same resume.",
                )
                .await
            }
            ResumeInspection::Incompatible { detail } => {
                self.resume_failure(
                    &task,
                    "graph.definition_incompatible",
                    &detail,
                    "Deploy the exact package, instance, and graph definition that produced the checkpoint, or start new work under a new task.",
                )
                .await
            }
            ResumeInspection::Unsupported => {
                self.resume_failure(
                    &task,
                    "graph.resume_unsupported",
                    "the task agent has no native resumable graph checkpoint",
                    "Resume only applies to tasks executed by a registered native graph workflow.",
                )
                .await
            }
            ResumeInspection::Complete(checkpoint) => {
                self.complete_from_checkpoint(&task, checkpoint).await
            }
            ResumeInspection::Paused(checkpoint) => {
                self.dispatch_resume(&task, checkpoint).await
            }
        }
    }

    async fn check_resume_profile(&self, task: &Task) -> Result<()> {
        let revision = self
            .profile_revision_unscoped(&task.namespace, &task.profile, task.profile_revision)
            .await
            .ok_or_else(|| {
                ApiError::new(
                    "profile.revision_unavailable",
                    format!(
                        "pinned profile revision {}:{} is no longer available for resume",
                        task.profile, task.profile_revision
                    ),
                )
            })?;
        if !revision
            .grants
            .iter()
            .any(|grant| grant == "*" || grant == scopes::TASK_SUBMIT)
        {
            return Err(ApiError::new(
                "profile.grant_revoked",
                "the pinned profile revision no longer grants task submission",
            ));
        }
        if !revision.capabilities.is_empty()
            && !revision
                .capabilities
                .iter()
                .any(|capability| capability == "*")
            && !revision.capabilities.contains(&task.agent)
        {
            return Err(ApiError::new(
                "profile.capability_denied",
                format!(
                    "the pinned profile revision does not include graph capability '{}'",
                    task.agent
                ),
            ));
        }
        Ok(())
    }

    async fn resume_failure(
        &self,
        task: &Task,
        code: &str,
        detail: &str,
        guidance: &str,
    ) -> Result<Task> {
        let at = now();
        let message = format!("{detail}. {guidance}");
        let changed = self
            .store()
            .write(|state| {
                let Some(task) = state.tasks.get_mut(&task.id) else {
                    return Err(task_not_found());
                };
                if !matches!(task.state, TaskState::Recovering | TaskState::Unknown) {
                    return Ok(false);
                }
                task.state = TaskState::Recovering;
                task.uncertainty = Some(message.clone());
                task.updated_at = at;
                Ok(true)
            })
            .await?;
        if !changed {
            return Err(ApiError::new(
                "task.resume_in_progress",
                "another operation changed the task before this resume failed",
            ));
        }
        let _ = self
            .append_event(
                &task.id,
                "task.resume_unavailable",
                serde_json::json!({
                    "code": code,
                    "detail": detail,
                    "guidance": guidance,
                    "thread_id": task.session_id,
                }),
                true,
            )
            .await;
        let _ = self
            .audit(&task.caller, code, &task.id, "unavailable", detail)
            .await;
        Err(ApiError::new(code, message))
    }

    async fn complete_from_checkpoint(&self, task: &Task, checkpoint: GraphResume) -> Result<Task> {
        let at = now();
        let output = checkpoint.output.clone().or_else(|| task.output.clone());
        let updated = self
            .store()
            .write(|state| {
                let Some(task) = state.tasks.get_mut(&task.id) else {
                    return Err(task_not_found());
                };
                if !matches!(task.state, TaskState::Recovering | TaskState::Unknown) {
                    return Ok(None);
                }
                task.state = TaskState::Succeeded;
                task.output = output.clone();
                task.error = None;
                task.uncertainty = None;
                task.native_thread_id = Some(checkpoint.thread_id.clone());
                task.updated_at = at;
                Ok(Some(task.clone()))
            })
            .await?;
        let Some(updated) = updated else {
            // A concurrent resume already advanced the task.
            return self
                .task_unscoped(&task.id)
                .await
                .ok_or_else(task_not_found);
        };
        let _ = self
            .append_event(
                &task.id,
                "task.resumed_from_checkpoint",
                serde_json::json!({
                    "thread_id": checkpoint.thread_id,
                    "step": checkpoint.step,
                    "pending_nodes": checkpoint.pending_nodes,
                    "replayed_nodes": false,
                }),
                true,
            )
            .await;
        let _ = self
            .append_event(
                &task.id,
                "task.succeeded",
                serde_json::json!({
                    "state": updated.state,
                    "output": updated.output,
                    "error": updated.error,
                    "uncertainty": updated.uncertainty,
                    "usage": updated.usage,
                }),
                true,
            )
            .await;
        let _ = self
            .audit(
                &task.caller,
                "task.resume",
                &task.id,
                "completed_from_checkpoint",
                "",
            )
            .await;
        Ok(updated)
    }

    async fn dispatch_resume(&self, task: &Task, checkpoint: GraphResume) -> Result<Task> {
        let at = now();
        let updated = self
            .store()
            .write(|state| {
                let Some(task) = state.tasks.get_mut(&task.id) else {
                    return Err(task_not_found());
                };
                if !matches!(task.state, TaskState::Recovering | TaskState::Unknown) {
                    return Ok(None);
                }
                task.state = TaskState::Accepted;
                task.uncertainty = None;
                task.native_thread_id = Some(checkpoint.thread_id.clone());
                task.updated_at = at;
                Ok(Some(task.clone()))
            })
            .await?;
        let Some(updated) = updated else {
            // A concurrent resume already advanced the task.
            return self
                .task_unscoped(&task.id)
                .await
                .ok_or_else(task_not_found);
        };
        let _ = self
            .append_event(
                &task.id,
                "task.resume_accepted",
                serde_json::json!({
                    "thread_id": checkpoint.thread_id,
                    "step": checkpoint.step,
                    "pending_nodes": checkpoint.pending_nodes,
                }),
                true,
            )
            .await;
        let _ = self
            .audit(&task.caller, "task.resume", &task.id, "accepted", "")
            .await;
        let cancel = CancellationToken::new();
        self.register_cancel_token(&task.id, cancel).await;
        let runtime = self.clone();
        let task_id = task.id.clone();
        tokio::spawn(async move {
            runtime.run_task(task_id).await;
        });
        Ok(updated)
    }
}

fn task_not_found() -> ApiError {
    ApiError::new("task.not_found", "task is not visible")
}
