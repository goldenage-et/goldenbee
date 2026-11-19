//! Native artifact-producing tool for the General package.
//!
//! The tool writes through the native [`adk_rust::Artifacts`] service attached
//! by the shared runtime. It returns only metadata and records the native
//! `artifact_delta`, so large bytes never enter model or event payloads.

use adk_rust::{Part, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Hard ceiling matching the runtime transfer bound.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Store one text or inline-binary artifact through the native service.
pub struct ArtifactTool;

#[async_trait]
impl Tool for ArtifactTool {
    fn name(&self) -> &str {
        "save_artifact"
    }

    fn description(&self) -> &str {
        "Save a named immutable artifact through the native artifact service"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Opaque artifact name without path separators"
                },
                "content_type": {
                    "type": "string",
                    "description": "Content type for inline binary data"
                },
                "text": {
                    "type": "string",
                    "description": "Text content to save"
                },
                "size": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Generate this many bytes of inline content instead of text"
                }
            },
            "required": ["name"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return Ok(json!({
                "error": "invalid_arguments",
                "message": "Missing required parameter 'name'"
            }));
        };
        let content_type = args
            .get("content_type")
            .and_then(Value::as_str)
            .unwrap_or("text/plain");
        let part = if let Some(size) = args.get("size").and_then(Value::as_u64) {
            if size == 0 || size > MAX_BYTES {
                return Ok(json!({
                    "error": "artifact.too_large",
                    "message": "size must be between 1 and the artifact limit"
                }));
            }
            Part::InlineData {
                mime_type: if content_type == "text/plain" {
                    "application/octet-stream".into()
                } else {
                    content_type.into()
                },
                data: vec![b'x'; size as usize],
                uri: None,
                annotations: None,
            }
        } else if let Some(text) = args.get("text").and_then(Value::as_str) {
            if text.len() as u64 > MAX_BYTES {
                return Ok(json!({
                    "error": "artifact.too_large",
                    "message": "content exceeds the artifact limit"
                }));
            }
            if content_type == "text/plain" {
                Part::Text {
                    text: text.to_string(),
                }
            } else {
                Part::InlineData {
                    mime_type: content_type.into(),
                    data: text.as_bytes().to_vec(),
                    uri: None,
                    annotations: None,
                }
            }
        } else {
            return Ok(json!({
                "error": "invalid_arguments",
                "message": "Provide 'text' or 'size'"
            }));
        };
        let Some(artifacts) = ctx.artifacts() else {
            return Ok(json!({
                "error": "artifact.unavailable",
                "message": "No native artifact service is configured"
            }));
        };
        let version = artifacts.save(name, &part).await?;
        let mut actions = ctx.actions();
        actions.artifact_delta.insert(name.to_string(), version);
        ctx.set_actions(actions);
        Ok(json!({
            "name": name,
            "version": version,
            "content_type": content_type,
        }))
    }
}
