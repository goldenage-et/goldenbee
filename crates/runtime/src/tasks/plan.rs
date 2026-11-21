//! Durable per-task plan state recorded by the planning tool.
//!
//! A plan belongs to exactly one task. The planning tool replaces it in full,
//! and the runtime keeps one durable copy so a Client can render the current
//! plan after a restart. Plan state is not authority: it never grants a tool
//! call or widens a task limit.

use crate::{ApiError, Result, Runtime};
use common::{scopes, Principal, TaskPlan};

impl Runtime {
    /// Replace the plan owned by one task and return the stored value.
    pub(crate) async fn set_task_plan(
        &self,
        task_id: &str,
        items: Vec<common::PlanItem>,
    ) -> Result<TaskPlan> {
        let at = crate::now();
        self.store()
            .write(|state| {
                if !state.tasks.contains_key(task_id) {
                    return Err(ApiError::new("task.not_found", "task does not exist"));
                }
                let plan = TaskPlan {
                    task_id: task_id.to_string(),
                    items,
                    updated_at: at,
                };
                state.task_plans.insert(task_id.to_string(), plan.clone());
                Ok(plan)
            })
            .await
    }

    /// Read the plan owned by one visible task.
    pub async fn task_plan(
        &self,
        principal: &Principal,
        task_id: &str,
    ) -> Result<Option<TaskPlan>> {
        let task = self.task(principal, task_id).await?;
        principal.require(scopes::TASK_READ)?;
        Ok(self
            .store()
            .read(|state| state.task_plans.get(&task.id).cloned())
            .await)
    }
}
