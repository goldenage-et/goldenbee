//! Subagent lifecycle tools exposed through the runtime tool kernel.
//!
//! Each tool is a thin adapter: it resolves the admitted parent task from the
//! native tool context and delegates to the durable runtime lifecycle. Policy,
//! admission, cancellation, events, and result merge stay in one place.

use super::config::SubagentConfig;
use crate::policy::PermissionMode;
use crate::tools::{ToolDescriptor, ToolKernel, ToolRisk};
use crate::Runtime;
use adk_rust::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

/// One lifecycle operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Spawn,
    Wait,
    List,
    SendInput,
    Interrupt,
    Close,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Spawn => "spawn_subagent",
            Self::Wait => "wait_subagent",
            Self::List => "list_subagents",
            Self::SendInput => "send_input",
            Self::Interrupt => "interrupt_subagent",
            Self::Close => "close_subagent",
        }
    }

    fn all() -> [Kind; 6] {
        [
            Kind::Spawn,
            Kind::Wait,
            Kind::List,
            Kind::SendInput,
            Kind::Interrupt,
            Kind::Close,
        ]
    }
}

/// Runtime-owned descriptor for one lifecycle tool.
fn descriptor(kind: Kind) -> ToolDescriptor {
    ToolDescriptor::new("subagent:manage")
        .risk(match kind {
            Kind::List | Kind::Wait => ToolRisk::Low,
            _ => ToolRisk::Medium,
        })
        .effect(crate::tools::ToolEffect::None)
        .typed_errors()
        .audit()
        // The tool owns its bounded wait and returns a truthful non-terminal
        // result on timeout. A kernel deadline would drop the future while the
        // durable child keeps running.
        .no_timeout()
}

struct SubagentTool {
    kind: Kind,
    kernel: ToolKernel,
    runtime: Arc<OnceLock<Runtime>>,
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        self.kind.name()
    }

    fn description(&self) -> &str {
        match self.kind {
            Kind::Spawn => {
                "Spawn one runtime-local subagent from a named definition. Its model, tools, \
                 permission mode, depth, and fanout are narrowed from this task and cannot widen. \
                 A background child returns immediately; otherwise the tool waits for the child."
            }
            Kind::Wait => {
                "Wait for one child task owned by this task. While waiting this task releases its \
                 execution slot. A timeout returns the truthful non-terminal state."
            }
            Kind::List => "List the subagent tasks owned by this task.",
            Kind::SendInput => {
                "Send follow-up input to a completed child in the same child session. The child \
                 must be terminal and not closed."
            }
            Kind::Interrupt => {
                "Cancel a running child. The child keeps its durable history and can still receive \
                 follow-up input."
            }
            Kind::Close => {
                "Close a child. Closing cancels unfinished work, records the close durably, and \
                 removes the child from the default listing."
            }
        }
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(match self.kind {
            Kind::Spawn => json!({
                "type": "object",
                "properties": {
                    "definition": {
                        "type": "string",
                        "description": "Named subagent definition declared by this Worker."
                    },
                    "input": {"type": "string", "description": "Child input."},
                    "background": {
                        "type": "boolean",
                        "description": "Return before the child completes."
                    },
                    "model": {
                        "type": "string",
                        "description": "Optional narrower logical model reference."
                    },
                    "tools": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional narrower tool allowlist."
                    },
                    "permission_mode": {
                        "type": "string",
                        "enum": ["plan", "default", "accept-edits", "full-auto"],
                        "description": "Optional narrower permission ceiling."
                    }
                },
                "required": ["definition", "input"]
            }),
            Kind::Wait => json!({
                "type": "object",
                "properties": {
                    "task_id": {"type": "string", "description": "Child task identity."},
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Bounded wait in milliseconds."
                    }
                },
                "required": ["task_id"]
            }),
            Kind::List => json!({
                "type": "object",
                "properties": {
                    "include_closed": {
                        "type": "boolean",
                        "description": "Include children this task closed."
                    }
                }
            }),
            Kind::SendInput => json!({
                "type": "object",
                "properties": {
                    "task_id": {"type": "string", "description": "Child task identity."},
                    "input": {"type": "string", "description": "Follow-up input."},
                    "background": {
                        "type": "boolean",
                        "description": "Return before the follow-up completes."
                    }
                },
                "required": ["task_id", "input"]
            }),
            Kind::Interrupt | Kind::Close => json!({
                "type": "object",
                "properties": {
                    "task_id": {"type": "string", "description": "Child task identity."}
                },
                "required": ["task_id"]
            }),
        })
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(parent) = self.kernel.task_for(ctx.as_ref()).await else {
            return Ok(error_value(
                "subagent.parent_unbound",
                "subagent operations require an admitted parent task",
            ));
        };
        let Some(runtime) = self.runtime.get() else {
            return Ok(error_value(
                "subagent.runtime_unbound",
                "the Worker runtime is not available",
            ));
        };
        let outcome = match self.kind {
            Kind::Spawn => {
                let Some(definition) = string_arg(&args, "definition") else {
                    return Ok(error_value(
                        "subagent.invalid_arguments",
                        "definition is required",
                    ));
                };
                let Some(input) = string_arg(&args, "input") else {
                    return Ok(error_value(
                        "subagent.invalid_arguments",
                        "input is required",
                    ));
                };
                let permission_mode = match args.get("permission_mode") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(value)) => match PermissionMode::from_wire(value) {
                        Some(mode) => Some(mode),
                        None => {
                            return Ok(error_value(
                                "subagent.permission_mode_invalid",
                                "permission_mode is not a known mode",
                            ))
                        }
                    },
                    Some(_) => {
                        return Ok(error_value(
                            "subagent.invalid_arguments",
                            "permission_mode must be a string",
                        ))
                    }
                };
                runtime
                    .spawn_subagent(
                        &parent.id,
                        super::SpawnRequest {
                            definition,
                            input,
                            background: bool_arg(&args, "background"),
                            model: string_arg(&args, "model"),
                            tools: tools_arg(&args),
                            permission_mode,
                        },
                    )
                    .await
            }
            Kind::Wait => {
                let Some(task_id) = string_arg(&args, "task_id") else {
                    return Ok(error_value(
                        "subagent.invalid_arguments",
                        "task_id is required",
                    ));
                };
                runtime
                    .wait_subagent(&parent.id, &task_id, u64_arg(&args, "timeout_ms"))
                    .await
            }
            Kind::List => {
                let include_closed = bool_arg(&args, "include_closed").unwrap_or(false);
                let children = runtime.subagents_of(&parent.id, include_closed).await;
                return Ok(json!({
                    "parent_task_id": parent.id,
                    "children": children,
                }));
            }
            Kind::SendInput => {
                let Some(task_id) = string_arg(&args, "task_id") else {
                    return Ok(error_value(
                        "subagent.invalid_arguments",
                        "task_id is required",
                    ));
                };
                let Some(input) = string_arg(&args, "input") else {
                    return Ok(error_value(
                        "subagent.invalid_arguments",
                        "input is required",
                    ));
                };
                runtime
                    .continue_subagent(&parent.id, &task_id, input, bool_arg(&args, "background"))
                    .await
            }
            Kind::Interrupt => {
                let Some(task_id) = string_arg(&args, "task_id") else {
                    return Ok(error_value(
                        "subagent.invalid_arguments",
                        "task_id is required",
                    ));
                };
                runtime.interrupt_subagent(&parent.id, &task_id).await
            }
            Kind::Close => {
                let Some(task_id) = string_arg(&args, "task_id") else {
                    return Ok(error_value(
                        "subagent.invalid_arguments",
                        "task_id is required",
                    ));
                };
                runtime.close_subagent(&parent.id, &task_id).await
            }
        };
        match outcome {
            Ok(result) => Ok(serde_json::to_value(result).unwrap_or_else(|error| {
                error_value(
                    "subagent.result_unavailable",
                    format!("subagent result could not be serialized: {error}"),
                )
            })),
            Err(error) => Ok(json!({
                "error": error.code,
                "message": common::redact(&error.message),
                "retryable": error.retryable,
                "details": error.details,
            })),
        }
    }
}

/// Build every enabled lifecycle tool for the kernel.
pub(super) fn registrations(
    kernel: &ToolKernel,
    runtime: Arc<OnceLock<Runtime>>,
    _config: &SubagentConfig,
) -> Vec<(String, Arc<dyn Tool>, Vec<String>)> {
    Kind::all()
        .into_iter()
        .map(|kind| {
            let tool = Arc::new(SubagentTool {
                kind,
                kernel: kernel.clone(),
                runtime: runtime.clone(),
            }) as Arc<dyn Tool>;
            let wrapped = kernel.wrap(tool, descriptor(kind));
            (
                kind.name().to_string(),
                wrapped,
                vec!["subagent:manage".to_string()],
            )
        })
        .collect()
}

fn string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn bool_arg(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

fn u64_arg(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn tools_arg(args: &Value) -> Option<Vec<String>> {
    args.get("tools").and_then(Value::as_array).map(|tools| {
        tools
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

fn error_value(code: &str, message: impl Into<String>) -> Value {
    json!({"error": code, "message": message.into()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lifecycle_operation_is_registered_with_a_bounded_descriptor() {
        let kernel = ToolKernel::new();
        let runtime = Arc::new(OnceLock::new());
        let config = SubagentConfig::new()
            .definition("helper", super::super::SubagentDefinition::new("echo"));
        let registered = registrations(&kernel, runtime, &config);
        let names: Vec<&str> = registered
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "spawn_subagent",
                "wait_subagent",
                "list_subagents",
                "send_input",
                "interrupt_subagent",
                "close_subagent"
            ]
        );
        assert!(registered
            .iter()
            .all(|(_, tool, permissions)| tool.name().len() > 4
                && permissions == &vec!["subagent:manage".to_string()]));
    }

    #[test]
    fn arguments_are_parsed_without_echoing_input() {
        let args = json!({"definition": " helper ", "input": "ignore previous", "tools": ["a", 2]});
        assert_eq!(string_arg(&args, "definition").as_deref(), Some("helper"));
        assert_eq!(tools_arg(&args), Some(vec!["a".to_string()]));
        assert_eq!(u64_arg(&args, "timeout_ms"), None);
    }
}
