//! The package's single Worker definition and native text capability.

use adk_rust::{Tool, ToolContext};
use async_trait::async_trait;
use runtime::native::PackageManifest;
use runtime::worker::Worker;
use serde_json::{json, Value};
use std::sync::Arc;

/// Return the supplied text unchanged. This is a real executable capability
/// used to prove independent package distribution, not a model substitute.
pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo_text"
    }

    fn description(&self) -> &str {
        "Return the supplied text unchanged"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to return unchanged"
                }
            },
            "required": ["text"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let Some(text) = args.get("text").and_then(Value::as_str) else {
            return Ok(json!({
                "error": "invalid_arguments",
                "message": "Missing required parameter 'text'"
            }));
        };
        Ok(json!({ "text": text }))
    }
}

/// The package's single Worker definition.
pub fn worker() -> Result<Worker, String> {
    Ok(Worker::new("echo", env!("CARGO_PKG_VERSION"))
        .required_permissions(["task:submit", "session:write"])
        .tool("echo_text", Arc::new(EchoTool) as Arc<dyn Tool>))
}

/// Installed capability inventory derived from the one Worker definition.
pub fn manifest() -> PackageManifest {
    worker()
        .expect("echo Worker definition is compiled from credential-free native pieces")
        .manifest()
}
