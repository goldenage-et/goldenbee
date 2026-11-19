//! Native ADK command tool bound to the instance worktree.
//!
//! The tool keeps the pinned `exec_command` name, schema, and result fields.
//! Execution still happens through the shared runtime sandbox; this adapter
//! only selects the instance worktree and build policy.

use crate::package::CodingPackage;
use adk_rust::{Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Command tool shared by manually registered and file-defined coding agents.
pub struct CodingCommandTool {
    package: Arc<CodingPackage>,
}

impl CodingCommandTool {
    /// Bind the tool to one package instance.
    pub fn new(package: Arc<CodingPackage>) -> Self {
        Self { package }
    }
}

#[async_trait]
impl Tool for CodingCommandTool {
    fn name(&self) -> &str {
        "exec_command"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the sandboxed worktree"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                },
                "working_dir": {
                    "type": "string",
                    "description": "Optional working directory relative to the worktree root"
                }
            },
            "required": ["command"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let services = match self.package.current() {
            Ok(services) => services,
            Err(error) => {
                return Ok(json!({
                    "error": "sandbox_unavailable",
                    "message": error.message,
                }))
            }
        };
        services.command_tool().execute(ctx, args).await
    }
}
