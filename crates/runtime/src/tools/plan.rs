//! Per-task plan tool.
//!
//! Item semantics come from the pinned `adk_agent::coding::TodoTool`. The
//! runtime adds the parts ADK does not own: durable per-task state, the task
//! event that a Client renders, and a bounded result. The ADK tool's own
//! per-instance list is never used for storage.

use super::kernel::{ToolDescriptor, ToolEffect, ToolKernel, ToolRisk};
use crate::Runtime;
use adk_agent::coding::TodoTool;
use adk_rust::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use common::PlanItem;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

/// Runtime-owned wrapper around the pinned planning tool.
pub struct PlanTool {
    kernel: ToolKernel,
    runtime: Arc<OnceLock<Runtime>>,
}

impl PlanTool {
    /// Build one planning tool bound to the process-wide runtime.
    pub fn new(kernel: ToolKernel, runtime: Arc<OnceLock<Runtime>>) -> Self {
        Self { kernel, runtime }
    }

    /// Runtime tool descriptor.
    pub fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new("plan:write")
            .risk(ToolRisk::Low)
            .effect(ToolEffect::None)
            .builtin()
            .typed_errors()
    }
}

#[async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &str {
        "write_todos"
    }

    fn description(&self) -> &str {
        "Record or update the plan for this task. Pass the full list each time; \
         it replaces the previous plan. Each item has `content` and a `status` of \
         'pending', 'in_progress', or 'completed'. Keep at most one item \
         'in_progress' while you work."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The full, updated task plan.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "What this step does." },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "Current status."
                            }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(task) = self.kernel.task_for(ctx.as_ref()).await else {
            return Ok(json!({
                "error": "plan.task_unbound",
                "message": "a plan update must execute inside an admitted running task",
            }));
        };
        let Some(runtime) = self.runtime.get() else {
            return Ok(json!({
                "error": "plan.runtime_unbound",
                "message": "the Worker runtime is not available",
            }));
        };
        // Validate through the pinned ADK tool so status/content semantics stay
        // identical to the native planning contract, then persist the result.
        let value = TodoTool::new().execute(ctx, args).await?;
        let items: Vec<PlanItem> = serde_json::from_value(
            value
                .get("todos")
                .cloned()
                .ok_or_else(|| AdkError::tool("planning tool returned no todos"))?,
        )
        .map_err(|error| AdkError::tool(format!("invalid plan items: {error}")))?;
        let mut in_progress = 0usize;
        for item in &items {
            if !matches!(
                item.status.as_str(),
                "pending" | "in_progress" | "completed"
            ) {
                return Ok(json!({
                    "error": "plan.invalid_status",
                    "message": format!(
                        "plan item status '{}' is not pending, in_progress, or completed",
                        item.status
                    ),
                }));
            }
            if item.status == "in_progress" {
                in_progress += 1;
            }
        }
        if in_progress > 1 {
            return Ok(json!({
                "error": "plan.multiple_in_progress",
                "message": "at most one plan item may be in_progress",
            }));
        }
        runtime
            .set_task_plan(&task.id, items.clone())
            .await
            .map_err(|error| AdkError::tool(format!("plan could not be stored: {}", error.code)))?;
        let remaining = items
            .iter()
            .filter(|item| item.status != "completed")
            .count();
        runtime
            .append_event(
                &task.id,
                "plan.updated",
                json!({
                    "task_id": task.id,
                    "items": items,
                    "total": items.len(),
                    "remaining": remaining,
                }),
                true,
            )
            .await
            .map_err(|error| {
                AdkError::tool(format!("plan event could not be appended: {}", error.code))
            })?;
        Ok(value)
    }
}
