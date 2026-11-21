//! Image and user-question tools.
//!
//! Both return typed values with no runtime-owned presentation. The image tool
//! reports a bounded descriptor a Client can render; the question tool reuses
//! the existing durable approval record and task event stream instead of adding
//! a second interaction store.

use super::kernel::{ToolDescriptor, ToolEffect, ToolKernel, ToolRisk};
use crate::Runtime;
use adk_rust::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Default bound for one user question.
const DEFAULT_QUESTION_TTL_MS: i64 = 60_000;
/// Hard upper bound for one user question.
const MAX_QUESTION_TTL_MS: i64 = 600_000;
/// Hard bound for one inline image payload.
///
/// The value keeps the base64 envelope well below the default tool result
/// ceiling. A larger image is a typed error, never a truncated data URL that a
/// Client would try to render.
const DEFAULT_MAX_IMAGE_BYTES: usize = 384 * 1024;

/// Ask the task caller a question through the approval channel.
pub struct AskUserTool {
    kernel: ToolKernel,
    runtime: Arc<OnceLock<Runtime>>,
}

impl AskUserTool {
    /// Build one question tool bound to the process-wide runtime.
    pub fn new(kernel: ToolKernel, runtime: Arc<OnceLock<Runtime>>) -> Self {
        Self { kernel, runtime }
    }

    /// Runtime tool descriptor.
    pub fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new("user:ask")
            .risk(ToolRisk::Low)
            .effect(ToolEffect::None)
            .builtin()
            .typed_errors()
            .audit()
    }
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the caller one question and wait a bounded time for an answer. \
         Supply `question` and optional `options`. The answer arrives through \
         the task approval channel; no decision returns a truthful pending \
         status instead of a fabricated answer."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the caller."
                },
                "options": {
                    "type": "array",
                    "description": "Optional bounded set of accepted answers.",
                    "items": { "type": "string" }
                },
                "ttl_ms": {
                    "type": "integer",
                    "description": "How long to wait for an answer, in milliseconds."
                }
            },
            "required": ["question"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(task) = self.kernel.task_for(ctx.as_ref()).await else {
            return Ok(json!({
                "error": "user.task_unbound",
                "message": "a user question must execute inside an admitted running task",
            }));
        };
        let Some(runtime) = self.runtime.get() else {
            return Ok(json!({
                "error": "user.runtime_unbound",
                "message": "the Worker runtime is not available",
            }));
        };
        let question = args
            .get("question")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|question| !question.is_empty())
            .ok_or_else(|| AdkError::tool("missing required string argument 'question'"))?;
        if question.chars().count() > 4096 {
            return Ok(json!({
                "error": "user.invalid_question",
                "message": "the question exceeds 4096 characters",
            }));
        }
        let options: Vec<String> = match args.get("options") {
            None | Some(Value::Null) => Vec::new(),
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|error| AdkError::tool(format!("invalid options: {error}")))?,
        };
        if options.len() > 8 {
            return Ok(json!({
                "error": "user.invalid_options",
                "message": "at most 8 options may be offered",
            }));
        }
        if options
            .iter()
            .any(|option| option.is_empty() || option.chars().count() > 256)
        {
            return Ok(json!({
                "error": "user.invalid_options",
                "message": "each option must be 1 to 256 characters",
            }));
        }
        let ttl = args
            .get("ttl_ms")
            .and_then(Value::as_i64)
            .unwrap_or(DEFAULT_QUESTION_TTL_MS)
            .clamp(1_000, MAX_QUESTION_TTL_MS);
        let digest = common::canonical_digest(&json!({
            "operation": "user.ask",
            "question": question,
            "options": options,
        }));
        let target: String = question.chars().take(512).collect();
        let approval = runtime
            .request_approval(&task.id, "user.ask", &target, &digest, "user_question", ttl)
            .await
            .map_err(|error| {
                AdkError::tool(format!("question could not be recorded: {}", error.code))
            })?;
        let _ = runtime
            .append_event(
                &task.id,
                "user.question",
                json!({
                    "approval_id": approval.id,
                    "question": question,
                    "options": options,
                    "digest": digest,
                    "expires_at": approval.expires_at,
                }),
                true,
            )
            .await;
        let remaining = (approval.expires_at - common::now_ms()).max(1) as u64;
        let decided = runtime
            .wait_for_approval(&approval.id, Duration::from_millis(remaining))
            .await;
        let decided = match decided {
            Ok(decided) => decided,
            Err(error) if error.code == "approval.expired" => {
                runtime.resume_after_question(&task.id).await;
                return Ok(json!({
                    "status": "pending",
                    "error": "user.pending",
                    "approval_id": approval.id,
                    "message": "no answer arrived before the question expired",
                }));
            }
            Err(error) if error.code == "approval.timeout" => {
                runtime.resume_after_question(&task.id).await;
                return Ok(json!({
                    "status": "pending",
                    "error": "user.pending",
                    "approval_id": approval.id,
                    "message": "no answer arrived before the wait deadline",
                }));
            }
            Err(error) => {
                return Ok(json!({
                    "error": "user.unavailable",
                    "approval_id": approval.id,
                    "message": common::redact(&error.message),
                }));
            }
        };
        match decided.status.as_str() {
            "approved" => {
                if !options.is_empty() {
                    match decided.answer.as_deref() {
                        Some(answer) if options.iter().any(|option| option == answer) => {}
                        _ => {
                            return Ok(json!({
                                "status": "invalid",
                                "error": "user.invalid_answer",
                                "approval_id": decided.id,
                                "options": options,
                                "message": "the answer is not one of the offered options",
                            }));
                        }
                    }
                }
                Ok(json!({
                    "status": "answered",
                    "approval_id": decided.id,
                    "question": question,
                    "answer": decided.answer,
                }))
            }
            "rejected" => Ok(json!({
                "status": "declined",
                "approval_id": decided.id,
                "question": question,
            })),
            status => {
                runtime.resume_after_question(&task.id).await;
                Ok(json!({
                    "status": "pending",
                    "error": "user.pending",
                    "approval_id": decided.id,
                    "approval_status": status,
                    "message": "the question has no recorded answer",
                }))
            }
        }
    }
}

/// Read one workspace image and return a typed descriptor.
pub struct ViewImageTool {
    workspace: adk_devtools::Workspace,
}

impl ViewImageTool {
    /// Build the image tool over the configured workspace.
    pub fn new(workspace: adk_devtools::Workspace) -> Self {
        Self { workspace }
    }

    /// Runtime tool descriptor.
    pub fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new("workspace:read")
            .risk(ToolRisk::Low)
            .effect(ToolEffect::None)
            .builtin()
            .typed_errors()
            .paths(image_path)
    }
}

fn image_path(args: &Value) -> Vec<String> {
    args.get("path")
        .and_then(Value::as_str)
        .map(|path| vec![path.to_string()])
        .unwrap_or_default()
}

#[async_trait]
impl Tool for ViewImageTool {
    fn name(&self) -> &str {
        "view_image"
    }

    fn description(&self) -> &str {
        "Read a workspace image and return a typed descriptor with its media \
         type, size, digest, and base64 data URL for a Client to render."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Image path relative to the workspace root."
                },
                "max_bytes": {
                    "type": "integer",
                    "description": "Maximum image size to return (default 384 KiB)."
                }
            },
            "required": ["path"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| AdkError::tool("missing required string argument 'path'"))?;
        let limit = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_MAX_IMAGE_BYTES)
            .clamp(1, DEFAULT_MAX_IMAGE_BYTES);
        let resolved = self.workspace.resolve(&path)?;
        let bytes = match tokio::fs::read(&resolved).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({
                    "error": "tool.not_found",
                    "message": format!("image '{path}' does not exist"),
                    "path": path,
                }));
            }
            Err(error) => {
                return Ok(json!({
                    "error": "image.unreadable",
                    "message": format!("image '{path}' could not be read: {error}"),
                    "path": path,
                }));
            }
        };
        let Some(media_type) = image_media_type(&bytes) else {
            return Ok(json!({
                "error": "image.unsupported",
                "message": "the file is not a PNG, JPEG, GIF, or WebP image",
                "path": path,
            }));
        };
        if bytes.len() > limit {
            return Ok(json!({
                "error": "image.too_large",
                "message": format!("the image exceeds the {limit} byte limit"),
                "path": path,
                "bytes": bytes.len(),
                "max_bytes": limit,
            }));
        }
        let digest = common::bytes_digest(&bytes);
        Ok(json!({
            "kind": "image",
            "path": self.workspace.display(&resolved),
            "media_type": media_type,
            "bytes": bytes.len(),
            "digest": digest,
            "encoding": "base64",
            "data_url": format!(
                "data:{media_type};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            ),
        }))
    }
}

fn image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}
