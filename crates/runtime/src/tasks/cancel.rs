//! Precise task cancellation.

use crate::core::ids::now;
use crate::{ApiError, Result, Runtime};
use common::{scopes, Principal, Task, TaskState};

impl Runtime {
    /// Cancel a precise task and recover resource permits when confirmed.
    pub async fn cancel_task(&self, principal: &Principal, task_id: &str) -> Result<Task> {
        principal.require(scopes::TASK_CANCEL)?;
        let task = self
            .store()
            .read(|state| state.tasks.get(task_id).cloned())
            .await
            .ok_or_else(|| ApiError::new("task.not_found", "task is not visible"))?;
        principal.require_namespace(&task.namespace)?;
        if task.caller != principal.id && !principal.has(scopes::ADMIN) {
            return Err(ApiError::new(
                "scope.task_owner",
                "only the owner may cancel this task",
            ));
        }
        if task.state.is_terminal() {
            return Ok(task);
        }
        let token = self.cancel_token(task_id).await;
        if let Some(token) = token.as_ref() {
            token.cancel();
        }
        let executor = self.executor().await;
        let confirmed = match executor {
            Some(executor) => executor.cancel(task_id).await,
            None => true,
        };
        let updated = self
            .store()
            .write(|state| {
                let task = state
                    .tasks
                    .get_mut(task_id)
                    .ok_or_else(|| ApiError::new("task.not_found", "task disappeared"))?;
                if task.state.is_terminal() {
                    return Ok(task.clone());
                }
                // Accepted/queued work has no native effect to unwind, so its
                // cancellation is terminal immediately. An executing task is
                // projected as cancelling until the native attempt returns.
                let executing = token.is_some()
                    && matches!(
                        task.state,
                        TaskState::Running | TaskState::WaitingApproval | TaskState::Cancelling
                    );
                task.state = if executing && !confirmed {
                    TaskState::Cancelling
                } else {
                    TaskState::Cancelled
                };
                task.updated_at = now();
                Ok(task.clone())
            })
            .await?;
        self.append_event(
            task_id,
            "task.cancellation_requested",
            serde_json::json!({"confirmed": confirmed, "state": updated.state}),
            true,
        )
        .await?;
        if updated.state == TaskState::Cancelled {
            self.append_event(
                task_id,
                "task.cancelled",
                serde_json::json!({"reason": "requested"}),
                true,
            )
            .await?;
        }
        self.audit(
            &principal.id,
            "task.cancel",
            task_id,
            if updated.state == TaskState::Cancelled {
                "cancelled"
            } else {
                "requested"
            },
            "",
        )
        .await?;
        Ok(updated)
    }
}
