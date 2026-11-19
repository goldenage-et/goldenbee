//! Deterministic and HTTP-backed model adapters for the native ADK Runner.
//!
//! These implement `adk_core::Llm`; they do not replace the Runner, agent
//! loop, sessions, tools, or events.

use adk_rust::{
    AdkError, Content, ErrorCategory, ErrorComponent, FinishReason, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, UsageMetadata,
};
use async_trait::async_trait;
use futures::stream;
use std::result::Result as StdResult;
use std::sync::Arc;

/// Deterministic model used by CI and standalone proofs. It streams a response
/// and fails explicitly for the `provider-fail` sentinel.
#[derive(Debug, Clone)]
pub struct DeterministicModel {
    /// Model name reported to ADK.
    pub model: String,
    /// Response prefix.
    pub prefix: String,
}

impl DeterministicModel {
    /// Construct the default deterministic model.
    pub fn new() -> Self {
        Self {
            model: "deterministic".into(),
            prefix: "echo".into(),
        }
    }

    /// Construct an echo model.
    pub fn echo() -> Self {
        Self {
            model: "echo".into(),
            prefix: "echo".into(),
        }
    }

    /// Construct a named model with a response prefix.
    pub fn named(model: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            prefix: prefix.into(),
        }
    }
}

impl Default for DeterministicModel {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Llm for DeterministicModel {
    fn name(&self) -> &str {
        &self.model
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        stream_response: bool,
    ) -> Result<LlmResponseStream> {
        let last_user_index = req
            .contents
            .iter()
            .rposition(|content| content.role == "user");
        let last_user = last_user_index
            .and_then(|index| req.contents.get(index))
            .and_then(|content| content.parts.iter().find_map(text_of))
            .unwrap_or_default();

        // Test control: `delay:<milliseconds>` holds the deterministic response
        // so concurrency and serialization can be observed without timing a
        // real provider.
        if let Some(value) = last_user.split("delay:").nth(1) {
            if let Ok(milliseconds) = value
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .parse::<u64>()
            {
                tokio::time::sleep(std::time::Duration::from_millis(milliseconds.min(30_000)))
                    .await;
            }
        }

        if last_user.to_ascii_lowercase().contains("provider-fail") {
            return Err(AdkError::model("deterministic provider failure"));
        }

        if last_user.to_ascii_lowercase().contains("rate-limit") {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::RateLimited,
                "provider.rate_limited",
                "deterministic provider rate limit",
            ));
        }

        // Test control: request the same tool repeatedly until the provider has
        // produced the requested number of function results. This exercises
        // tool-call budgets without a private tool engine.
        if let Some(rest) = last_user.strip_prefix("tool-loop:") {
            let mut fields = rest.splitn(3, |value: char| value == ':' || value.is_whitespace());
            let requested = fields
                .next()
                .unwrap_or_default()
                .parse::<usize>()
                .unwrap_or(0);
            let name = fields.next().unwrap_or_default().trim();
            let prior_results = req
                .contents
                .iter()
                .skip(last_user_index.map(|index| index + 1).unwrap_or(0))
                .flat_map(|content| content.parts.iter())
                .filter(|part| matches!(part, Part::FunctionResponse { .. }))
                .count();
            if requested > 0 && prior_results < requested && !name.is_empty() {
                let args = fields
                    .next()
                    .map(str::trim)
                    .and_then(|value| serde_json::from_str(value).ok())
                    .unwrap_or_else(|| serde_json::json!({}));
                let mut call = Content::new("model");
                call.parts.push(Part::FunctionCall {
                    name: name.to_string(),
                    args,
                    id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                    thought_signature: None,
                });
                let response = LlmResponse {
                    content: Some(call),
                    usage_metadata: Some(usage(&last_user, &self.model)),
                    finish_reason: Some(FinishReason::Stop),
                    partial: false,
                    turn_complete: true,
                    interrupted: false,
                    error_code: None,
                    error_message: None,
                    citation_metadata: None,
                    provider_metadata: None,
                    interaction_id: None,
                };
                return Ok(Box::pin(stream::iter(vec![Ok(response)])));
            }
        }

        // Only tool results produced after the latest user turn belong to this
        // response cycle. A prior turn's result must not suppress a new call.
        let has_tool_result = req
            .contents
            .iter()
            .skip(last_user_index.map(|index| index + 1).unwrap_or(0))
            .any(|content| {
                content
                    .parts
                    .iter()
                    .any(|part| matches!(part, Part::FunctionResponse { .. }))
            });

        // Test control: call an exact tool name with JSON arguments. This
        // exercises native tool discovery and dispatch without hardcoding a
        // production provider or a private tool engine.
        if let Some(rest) = last_user.strip_prefix("call:") {
            if !has_tool_result {
                let mut parts = rest.trim().splitn(2, char::is_whitespace);
                let name = parts.next().unwrap_or_default().trim();
                if !name.is_empty() {
                    let args = parts
                        .next()
                        .map(str::trim)
                        .and_then(|value| serde_json::from_str(value).ok())
                        .unwrap_or_else(|| serde_json::json!({}));
                    let mut call = Content::new("model");
                    call.parts.push(Part::FunctionCall {
                        name: name.to_string(),
                        args,
                        id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                        thought_signature: None,
                    });
                    let response = LlmResponse {
                        content: Some(call),
                        usage_metadata: Some(usage(&last_user, "")),
                        finish_reason: Some(FinishReason::Stop),
                        partial: false,
                        turn_complete: true,
                        interrupted: false,
                        error_code: None,
                        error_message: None,
                        citation_metadata: None,
                        provider_metadata: None,
                        interaction_id: None,
                    };
                    return Ok(Box::pin(stream::iter(vec![Ok(response)])));
                }
            }
        }

        // Test control for remote MCP dispatch: `mcp:<tool> <json>` calls the
        // exact discovered tool and echoes the deterministic tool result.
        let tool_response = req.contents.iter().rev().find_map(|content| {
            content.parts.iter().find_map(|part| match part {
                Part::FunctionResponse {
                    function_response, ..
                } => Some(function_response.response.to_string()),
                _ => None,
            })
        });

        if let Some(rest) = last_user.strip_prefix("mcp:") {
            if !has_tool_result {
                let mut parts = rest.trim().splitn(2, char::is_whitespace);
                let name = parts.next().unwrap_or_default().trim().to_string();
                let args = parts
                    .next()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
                    .unwrap_or_else(|| serde_json::json!({}));
                let mut call = Content::new("model");
                call.parts.push(Part::FunctionCall {
                    name,
                    args,
                    id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                    thought_signature: None,
                });
                let response = LlmResponse {
                    content: Some(call),
                    usage_metadata: Some(usage(&last_user, "")),
                    finish_reason: Some(FinishReason::Stop),
                    partial: false,
                    turn_complete: true,
                    interrupted: false,
                    error_code: None,
                    error_message: None,
                    citation_metadata: None,
                    provider_metadata: None,
                    interaction_id: None,
                };
                return Ok(Box::pin(stream::iter(vec![Ok(response)])));
            }
            let text = format!(
                "{}: mcp result {}",
                self.prefix,
                tool_response.clone().unwrap_or_default()
            );
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                text,
                &last_user,
                true,
            ))])));
        }

        // Test control: drive native devtools and the contained command tool
        // through the real Runner. The payload is fixed so this exercises tool
        // dispatch, path containment, and evidence rather than model quality.
        //
        // `coding-edit:<path>:<base64 old>:<base64 new>:<check command>` performs
        // read_file -> edit_file -> exec_command. `coding-fix` performs
        // read_file -> write_file -> exec_command. `coding-write`,
        // `coding-read`, and `coding-check` perform one native call.
        let tool_results: Vec<(&str, &serde_json::Value)> = req
            .contents
            .iter()
            .skip(last_user_index.map(|index| index + 1).unwrap_or(0))
            .flat_map(|content| content.parts.iter())
            .filter_map(|part| match part {
                Part::FunctionResponse {
                    function_response, ..
                } => Some((function_response.name.as_str(), &function_response.response)),
                _ => None,
            })
            .collect();
        if last_user.contains("coding-") {
            let last_tool = tool_results.last().map(|(name, _)| *name);
            let finalize = || {
                let rendered = tool_results
                    .iter()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect::<Vec<_>>()
                    .join(" | ");
                format!("coding result {rendered}")
            };
            if let Some(payload) = last_user.split("coding-edit:").nth(1) {
                let mut parts = payload.splitn(4, ':');
                let path = parts.next().unwrap_or_default().trim();
                let old = decode_text(parts.next().unwrap_or_default());
                let new = decode_text(parts.next().unwrap_or_default());
                let command = parts.next().unwrap_or("true").trim();
                let call = match last_tool {
                    None => Some(("read_file", serde_json::json!({"path": path}))),
                    Some("read_file") => Some((
                        "edit_file",
                        serde_json::json!({
                            "path": path,
                            "old_string": old,
                            "new_string": new,
                        }),
                    )),
                    Some("edit_file") => {
                        Some(("exec_command", serde_json::json!({"command": command})))
                    }
                    _ => None,
                };
                if let Some((name, args)) = call {
                    return Ok(Box::pin(stream::iter(vec![Ok(function_call_response(
                        &last_user, name, args,
                    ))])));
                }
                return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                    &self.model,
                    finalize(),
                    &last_user,
                    true,
                ))])));
            }
            if let Some(payload) = last_user.split("coding-fix:").nth(1) {
                let mut parts = payload.splitn(3, ':');
                let path = parts.next().unwrap_or_default().trim();
                let content = parts
                    .next()
                    .and_then(|value| {
                        use base64::Engine;
                        base64::engine::general_purpose::STANDARD
                            .decode(value.trim())
                            .ok()
                    })
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .unwrap_or_default();
                let command = parts.next().unwrap_or("true").trim();
                let call = match last_tool {
                    None => Some(("read_file", serde_json::json!({"path": path}))),
                    Some("read_file") => Some((
                        "write_file",
                        serde_json::json!({"path": path, "content": content}),
                    )),
                    Some("write_file") => {
                        Some(("exec_command", serde_json::json!({"command": command})))
                    }
                    _ => None,
                };
                if let Some((name, args)) = call {
                    return Ok(Box::pin(stream::iter(vec![Ok(function_call_response(
                        &last_user, name, args,
                    ))])));
                }
                return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                    &self.model,
                    finalize(),
                    &last_user,
                    true,
                ))])));
            }
            if let Some(payload) = last_user.split("coding-write:").nth(1) {
                if last_tool.is_none() {
                    let mut parts = payload.splitn(2, ':');
                    let path = parts.next().unwrap_or_default().trim();
                    let content = parts
                        .next()
                        .and_then(|value| {
                            use base64::Engine;
                            base64::engine::general_purpose::STANDARD
                                .decode(value.trim())
                                .ok()
                        })
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_default();
                    return Ok(Box::pin(stream::iter(vec![Ok(function_call_response(
                        &last_user,
                        "write_file",
                        serde_json::json!({"path": path, "content": content}),
                    ))])));
                }
                return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                    &self.model,
                    finalize(),
                    &last_user,
                    true,
                ))])));
            }
            if let Some(path) = last_user.split("coding-read:").nth(1) {
                if last_tool.is_none() {
                    return Ok(Box::pin(stream::iter(vec![Ok(function_call_response(
                        &last_user,
                        "read_file",
                        serde_json::json!({"path": path.trim()}),
                    ))])));
                }
                return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                    &self.model,
                    finalize(),
                    &last_user,
                    true,
                ))])));
            }
            if let Some(command) = last_user.split("coding-check:").nth(1) {
                if last_tool.is_none() {
                    return Ok(Box::pin(stream::iter(vec![Ok(function_call_response(
                        &last_user,
                        "exec_command",
                        serde_json::json!({"command": command.trim()}),
                    ))])));
                }
                return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                    &self.model,
                    finalize(),
                    &last_user,
                    true,
                ))])));
            }
        }

        // Deterministic composition sentinels. They drive the real native
        // workflow/team agents; they do not orchestrate anything themselves.
        let instructions = instructions_text(&req);

        if instructions.contains("CHAIN") {
            if instructions.contains("the 'general' specialist") {
                return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                    &self.model,
                    "step-one".into(),
                    &last_user,
                    true,
                ))])));
            }
            if instructions.contains("the 'echo' specialist") {
                let prior = req
                    .contents
                    .iter()
                    .filter(|content| content.role == "model")
                    .filter_map(|content| content.parts.iter().find_map(text_of))
                    .next_back()
                    .unwrap_or_default();
                let text = if prior.contains("step-one") {
                    "step-two-saw-step-one".to_string()
                } else {
                    "step-two-missing-step-one".to_string()
                };
                return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                    &self.model,
                    text,
                    &last_user,
                    true,
                ))])));
            }
        }

        if instructions.contains("PARALLEL") {
            let text = if instructions.contains("the 'echo' specialist") {
                "branch-echo"
            } else {
                "branch-general"
            };
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                text.into(),
                &last_user,
                true,
            ))])));
        }

        if instructions.contains("MANUAL-FIRST") {
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                "MANUAL-FIRST-output".into(),
                &last_user,
                true,
            ))])));
        }
        if instructions.contains("MANUAL-SECOND") {
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                "MANUAL-SECOND-output".into(),
                &last_user,
                true,
            ))])));
        }

        if instructions.contains("HANDOFF-COORDINATOR") && last_user.contains("handoff:") {
            let target = last_user
                .split("handoff:")
                .nth(1)
                .unwrap_or_default()
                .split_whitespace()
                .next()
                .unwrap_or("echo");
            let mut content = Content::new("model");
            content.parts.push(Part::FunctionCall {
                name: "transfer_to_agent".into(),
                args: serde_json::json!({"agent_name": target}),
                id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                thought_signature: None,
            });
            let response = LlmResponse {
                content: Some(content),
                usage_metadata: Some(usage(&last_user, &self.model)),
                finish_reason: Some(FinishReason::Stop),
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                citation_metadata: None,
                provider_metadata: None,
                interaction_id: None,
            };
            return Ok(Box::pin(stream::iter(vec![Ok(response)])));
        }

        if instructions.contains("team coordinator") && last_user.contains("delegate:") {
            if !has_tool_result {
                let targets = last_user
                    .split("delegate:")
                    .nth(1)
                    .unwrap_or_default()
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter(|target| !target.is_empty())
                    .collect::<Vec<_>>();
                let mut content = Content::new("model").with_text("coordinating");
                for target in targets {
                    content.parts.push(Part::FunctionCall {
                        name: target.to_string(),
                        args: serde_json::json!({"request": format!("child:{target}")}),
                        id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                        thought_signature: None,
                    });
                }
                let response = LlmResponse {
                    content: Some(content),
                    usage_metadata: Some(usage(&last_user, &self.model)),
                    finish_reason: Some(FinishReason::Stop),
                    partial: false,
                    turn_complete: true,
                    interrupted: false,
                    error_code: None,
                    error_message: None,
                    citation_metadata: None,
                    provider_metadata: None,
                    interaction_id: None,
                };
                return Ok(Box::pin(stream::iter(vec![Ok(response)])));
            }
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                "team-final".into(),
                &last_user,
                true,
            ))])));
        }

        if let Some(child) = last_user.strip_prefix("child:") {
            let text = format!("child-output:{child}");
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                text,
                &last_user,
                true,
            ))])));
        }

        if last_user.contains("artifact-large:") && !has_tool_result {
            let size = last_user
                .split("artifact-large:")
                .nth(1)
                .map(str::trim)
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(1_048_576);
            let mut call = Content::new("model");
            call.parts.push(Part::FunctionCall {
                name: "save_artifact".into(),
                args: serde_json::json!({
                    "name": "large.bin",
                    "content_type": "application/octet-stream",
                    "size": size,
                }),
                id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                thought_signature: None,
            });
            let response = LlmResponse {
                content: Some(call),
                usage_metadata: Some(usage(&last_user, "")),
                finish_reason: Some(FinishReason::Stop),
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                citation_metadata: None,
                provider_metadata: None,
                interaction_id: None,
            };
            return Ok(Box::pin(stream::iter(vec![Ok(response)])));
        }

        if last_user.contains("artifact:") && !has_tool_result {
            let text = last_user
                .split("artifact:")
                .nth(1)
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            let mut call = Content::new("model");
            call.parts.push(Part::FunctionCall {
                name: "save_artifact".into(),
                args: serde_json::json!({
                    "name": "evidence.txt",
                    "content_type": "text/plain",
                    "text": text,
                }),
                id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                thought_signature: None,
            });
            let response = LlmResponse {
                content: Some(call),
                usage_metadata: Some(usage(&last_user, "")),
                finish_reason: Some(FinishReason::Stop),
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                citation_metadata: None,
                provider_metadata: None,
                interaction_id: None,
            };
            return Ok(Box::pin(stream::iter(vec![Ok(response)])));
        }

        if last_user.contains("effect:") && !has_tool_result {
            let target = last_user
                .split("effect:")
                .nth(1)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "receipt.txt".into());
            let mut call = Content::new("model");
            call.parts.push(Part::FunctionCall {
                name: "protected_write".into(),
                args: serde_json::json!({"target": target, "payload": last_user}),
                id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                thought_signature: None,
            });
            let response = LlmResponse {
                content: Some(call),
                usage_metadata: Some(usage(&last_user, "")),
                finish_reason: Some(FinishReason::Stop),
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                citation_metadata: None,
                provider_metadata: None,
                interaction_id: None,
            };
            return Ok(Box::pin(stream::iter(vec![Ok(response)])));
        }

        // Knowledge package test controls. These call the package's real native
        // tools; they do not replace retrieval or ingestion. The optional
        // `|allowed=a,b` suffix sets the document ACL used by the fixture.
        if last_user.contains("knowledge-ingest:") && !has_tool_result {
            let payload = last_user
                .split("knowledge-ingest:")
                .nth(1)
                .map(str::trim)
                .unwrap_or_default();
            let (uri, options) = payload.split_once('|').unwrap_or((payload, ""));
            let allowed: Vec<String> = options
                .strip_prefix("allowed=")
                .map(|values| {
                    values
                        .split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let mut call = Content::new("model");
            call.parts.push(Part::FunctionCall {
                name: "knowledge_ingest".into(),
                args: serde_json::json!({"uri": uri.trim(), "allowed": allowed}),
                id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                thought_signature: None,
            });
            let response = LlmResponse {
                content: Some(call),
                usage_metadata: Some(usage(&last_user, "")),
                finish_reason: Some(FinishReason::Stop),
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                citation_metadata: None,
                provider_metadata: None,
                interaction_id: None,
            };
            return Ok(Box::pin(stream::iter(vec![Ok(response)])));
        }

        if last_user.contains("knowledge-ask:") && !has_tool_result {
            let payload = last_user
                .split("knowledge-ask:")
                .nth(1)
                .map(str::trim)
                .unwrap_or_default();
            let (query, options) = payload.split_once('|').unwrap_or((payload, ""));
            let top_k = options
                .strip_prefix("top_k=")
                .and_then(|value| value.trim().parse::<u64>().ok());
            let mut args = serde_json::json!({"query": query.trim()});
            if let Some(top_k) = top_k {
                args["top_k"] = serde_json::json!(top_k);
            }
            let mut call = Content::new("model");
            call.parts.push(Part::FunctionCall {
                name: "knowledge_ask".into(),
                args,
                id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                thought_signature: None,
            });
            let response = LlmResponse {
                content: Some(call),
                usage_metadata: Some(usage(&last_user, "")),
                finish_reason: Some(FinishReason::Stop),
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                citation_metadata: None,
                provider_metadata: None,
                interaction_id: None,
            };
            return Ok(Box::pin(stream::iter(vec![Ok(response)])));
        }

        if last_user.contains("knowledge-remove:") && !has_tool_result {
            let uri = last_user
                .split("knowledge-remove:")
                .nth(1)
                .map(str::trim)
                .unwrap_or_default();
            let mut call = Content::new("model");
            call.parts.push(Part::FunctionCall {
                name: "knowledge_remove".into(),
                args: serde_json::json!({"uri": uri}),
                id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                thought_signature: None,
            });
            let response = LlmResponse {
                content: Some(call),
                usage_metadata: Some(usage(&last_user, "")),
                finish_reason: Some(FinishReason::Stop),
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                citation_metadata: None,
                provider_metadata: None,
                interaction_id: None,
            };
            return Ok(Box::pin(stream::iter(vec![Ok(response)])));
        }

        if last_user.contains("sandbox:") && !has_tool_result {
            let command = last_user
                .split("sandbox:")
                .nth(1)
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            let mut call = Content::new("model");
            call.parts.push(Part::FunctionCall {
                name: "exec_command".into(),
                args: serde_json::json!({"command": command}),
                id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                thought_signature: None,
            });
            let response = LlmResponse {
                content: Some(call),
                usage_metadata: Some(usage(&last_user, "")),
                finish_reason: Some(FinishReason::Stop),
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                citation_metadata: None,
                provider_metadata: None,
                interaction_id: None,
            };
            return Ok(Box::pin(stream::iter(vec![Ok(response)])));
        }

        // Test control: expose the resolved instruction preamble so immutable
        // prompt/asset revisions can be observed through real Runner execution.
        if last_user.to_ascii_lowercase().contains("prompt:") {
            let instruction = instruction_of(&req);
            let text = if last_user.contains("[skill:") {
                // Expose native skill injection alongside the resolved system
                // instruction so skill selection behavior is observable.
                format!("prompt={instruction}\nskill={last_user}")
            } else {
                format!("prompt={instruction}")
            };
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                text,
                &last_user,
                true,
            ))])));
        }

        let tool_response = req.contents.iter().rev().find_map(|content| {
            content.parts.iter().find_map(|part| match part {
                Part::FunctionResponse {
                    function_response, ..
                } => Some(function_response.response.to_string()),
                _ => None,
            })
        });

        // Test control: drive a native browser tool flow through the real
        // Runner. The fixture site is deterministic and the model only selects
        // tool calls; navigation policy remains outside the model.
        if let Some(spec) = last_user.strip_prefix("browser-flow:") {
            let spec: serde_json::Value =
                serde_json::from_str(spec.trim()).unwrap_or(serde_json::Value::Null);
            let responses = req
                .contents
                .iter()
                .skip(last_user_index.map(|index| index + 1).unwrap_or(0))
                .flat_map(|content| content.parts.iter())
                .filter(|part| matches!(part, Part::FunctionResponse { .. }))
                .count();
            let call = match responses {
                0 => Some((
                    "browser_navigate",
                    serde_json::json!({ "url": spec["url"].as_str().unwrap_or_default() }),
                )),
                1 if spec["text"].as_str().is_some() => Some((
                    "browser_type",
                    serde_json::json!({
                        "selector": spec["selector"].as_str().unwrap_or("#name"),
                        "text": spec["text"].as_str().unwrap_or_default()
                    }),
                )),
                2 if spec["button"].as_str().is_some() => Some((
                    "browser_click",
                    serde_json::json!({ "selector": spec["button"].as_str().unwrap_or("#go") }),
                )),
                3 => Some((
                    "browser_extract_text",
                    serde_json::json!({ "selector": spec["extract"].as_str().unwrap_or("body") }),
                )),
                _ => None,
            };
            if let Some((name, args)) = call {
                let mut content = Content::new("model");
                content.parts.push(Part::FunctionCall {
                    name: name.into(),
                    args,
                    id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                    thought_signature: None,
                });
                let response = LlmResponse {
                    content: Some(content),
                    usage_metadata: Some(usage(&last_user, "")),
                    finish_reason: Some(FinishReason::Stop),
                    partial: false,
                    turn_complete: true,
                    interrupted: false,
                    error_code: None,
                    error_message: None,
                    citation_metadata: None,
                    provider_metadata: None,
                    interaction_id: None,
                };
                return Ok(Box::pin(stream::iter(vec![Ok(response)])));
            }
            let text = format!(
                "{}: browser result {}",
                self.prefix,
                tool_response.clone().unwrap_or_default()
            );
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                text,
                &last_user,
                true,
            ))])));
        }

        if let Some(fact) = last_user.strip_prefix("remember:") {
            if !has_tool_result {
                let mut call = Content::new("model");
                call.parts.push(Part::FunctionCall {
                    name: "remember_memory".into(),
                    args: serde_json::json!({
                        "text": fact.trim(),
                        "source": "deterministic-model"
                    }),
                    id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                    thought_signature: None,
                });
                let response = LlmResponse {
                    content: Some(call),
                    usage_metadata: Some(usage(&last_user, "")),
                    finish_reason: Some(FinishReason::Stop),
                    partial: false,
                    turn_complete: true,
                    interrupted: false,
                    error_code: None,
                    error_message: None,
                    citation_metadata: None,
                    provider_metadata: None,
                    interaction_id: None,
                };
                return Ok(Box::pin(stream::iter(vec![Ok(response)])));
            }
            let text = format!(
                "{}: remembered {}",
                self.prefix,
                tool_response.clone().unwrap_or_default()
            );
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                text,
                &last_user,
                true,
            ))])));
        }

        if let Some(rest) = last_user.strip_prefix("observe:") {
            let parts: Vec<&str> = rest.split('|').map(str::trim).collect();
            if parts.len() != 4 || parts.iter().any(|part| part.is_empty()) {
                return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                    &self.model,
                    "observe: expected subject|predicate|object|source".into(),
                    &last_user,
                    true,
                ))])));
            }
            if !has_tool_result {
                let mut call = Content::new("model");
                call.parts.push(Part::FunctionCall {
                    name: "observe_knowledge".into(),
                    args: serde_json::json!({
                        "subject": parts[0],
                        "predicate": parts[1],
                        "object": parts[2],
                        "source": parts[3]
                    }),
                    id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                    thought_signature: None,
                });
                let response = LlmResponse {
                    content: Some(call),
                    usage_metadata: Some(usage(&last_user, "")),
                    finish_reason: Some(FinishReason::Stop),
                    partial: false,
                    turn_complete: true,
                    interrupted: false,
                    error_code: None,
                    error_message: None,
                    citation_metadata: None,
                    provider_metadata: None,
                    interaction_id: None,
                };
                return Ok(Box::pin(stream::iter(vec![Ok(response)])));
            }
            let text = format!(
                "knowledge: observed {}",
                tool_response.clone().unwrap_or_default()
            );
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                text,
                &last_user,
                true,
            ))])));
        }

        if let Some(query) = last_user.strip_prefix("recall:") {
            if !has_tool_result {
                let mut call = Content::new("model");
                call.parts.push(Part::FunctionCall {
                    name: "recall_knowledge".into(),
                    args: serde_json::json!({"query": query.trim(), "limit": 10}),
                    id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                    thought_signature: None,
                });
                let response = LlmResponse {
                    content: Some(call),
                    usage_metadata: Some(usage(&last_user, "")),
                    finish_reason: Some(FinishReason::Stop),
                    partial: false,
                    turn_complete: true,
                    interrupted: false,
                    error_code: None,
                    error_message: None,
                    citation_metadata: None,
                    provider_metadata: None,
                    interaction_id: None,
                };
                return Ok(Box::pin(stream::iter(vec![Ok(response)])));
            }
            let text = format!(
                "knowledge: recalled {}",
                tool_response.clone().unwrap_or_default()
            );
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                text,
                &last_user,
                true,
            ))])));
        }

        if last_user.contains("knowledge-ingest:")
            || last_user.contains("knowledge-ask:")
            || last_user.contains("knowledge-remove:")
        {
            let text = format!(
                "knowledge:{}",
                tool_response.clone().unwrap_or_else(|| "{}".into())
            );
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                text,
                &last_user,
                true,
            ))])));
        }

        let prior = format!(" [context:{}]", req.contents.len());
        let memory_context = req
            .contents
            .iter()
            .filter(|content| content.role == "system")
            .flat_map(|content| content.parts.iter())
            .filter_map(text_of)
            .filter(|text| text.contains("Relevant Memories"))
            .collect::<Vec<_>>()
            .join(" ");
        let text = if last_user.starts_with("call:") {
            format!(
                "{}: tool result {}",
                self.prefix,
                tool_response.clone().unwrap_or_default()
            )
        } else if last_user.contains("effect:") {
            format!(
                "{}: protected effect completed {}",
                self.prefix,
                tool_response.clone().unwrap_or_default()
            )
        } else if last_user.contains("sandbox:") {
            format!(
                "{}: sandbox result {}",
                self.prefix,
                tool_response.clone().unwrap_or_default()
            )
        } else if memory_context.is_empty() {
            format!("{}: {}{}", self.prefix, last_user, prior)
        } else {
            format!(
                "{}: {}{} [memory:{}]",
                self.prefix,
                last_user,
                prior,
                memory_context.replace(['\n', '\r'], " ")
            )
        };

        if !stream_response {
            return Ok(Box::pin(stream::iter(vec![Ok(response_for(
                &self.model,
                text,
                &last_user,
                false,
            ))])));
        }

        let split = text
            .char_indices()
            .nth(text.chars().count().saturating_sub(1).min(8))
            .map(|(index, _)| index)
            .unwrap_or(text.len());
        let (head, tail) = text.split_at(split);
        let first = LlmResponse {
            content: Some(Content::new("model").with_text(head.to_string())),
            usage_metadata: None,
            finish_reason: None,
            partial: true,
            turn_complete: false,
            interrupted: false,
            error_code: None,
            error_message: None,
            citation_metadata: None,
            provider_metadata: None,
            interaction_id: None,
        };
        let second = response_for(&self.model, tail.to_string(), &last_user, true);
        Ok(Box::pin(stream::iter(vec![Ok(first), Ok(second)])))
    }
}

/// HTTP model that sends the ADK request to a deterministic local endpoint.
#[derive(Debug, Clone)]
pub struct HttpModel {
    /// Model name.
    pub model: String,
    /// Endpoint URL.
    pub endpoint: String,
    client: reqwest::Client,
}

impl HttpModel {
    /// Construct an HTTP model.
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            endpoint: endpoint.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Llm for HttpModel {
    fn name(&self) -> &str {
        &self.model
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let body = serde_json::json!({
            "model": req.model,
            "contents": req.contents,
            "config": req.config,
        });
        let response = self
            .client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|error| AdkError::model(format!("deterministic endpoint failed: {error}")))?;
        let status = response.status();
        let value: serde_json::Value = response.json().await.map_err(|error| {
            AdkError::model(format!(
                "deterministic endpoint returned invalid JSON: {error}"
            ))
        })?;
        if !status.is_success() {
            // Transient statuses are classified so policy fallback can act on
            // them. The body is never echoed; it may contain provider secrets.
            return Err(match status.as_u16() {
                429 => AdkError::rate_limited(
                    adk_rust::ErrorComponent::Model,
                    "model.http.rate_limited",
                    "deterministic endpoint rate limited the request",
                ),
                code if code >= 500 => AdkError::new(
                    adk_rust::ErrorComponent::Model,
                    adk_rust::ErrorCategory::Unavailable,
                    "model.http.unavailable",
                    "deterministic endpoint is unavailable",
                ),
                _ => AdkError::model(format!("deterministic endpoint returned {status}")),
            });
        }
        let text = value
            .get("text")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .or_else(|| {
                value
                    .pointer("/content/parts/0/text")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string())
            })
            .ok_or_else(|| AdkError::model("deterministic endpoint omitted text"))?;
        Ok(Box::pin(stream::iter(vec![Ok(response_for(
            &self.model,
            text,
            "",
            true,
        ))])))
    }
}

fn function_call_response(prompt: &str, name: &str, args: serde_json::Value) -> LlmResponse {
    let mut call = Content::new("model");
    call.parts.push(Part::FunctionCall {
        name: name.into(),
        args,
        id: Some(format!("call-{}", uuid::Uuid::new_v4())),
        thought_signature: None,
    });
    LlmResponse {
        content: Some(call),
        usage_metadata: Some(usage(prompt, name)),
        finish_reason: Some(FinishReason::Stop),
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: None,
        error_message: None,
        citation_metadata: None,
        provider_metadata: None,
        interaction_id: None,
    }
}

fn response_for(model: &str, text: String, prompt: &str, final_chunk: bool) -> LlmResponse {
    LlmResponse {
        content: Some(Content::new("model").with_text(text)),
        usage_metadata: if prompt.contains("usage-unknown") {
            None
        } else {
            Some(usage(prompt, model))
        },
        finish_reason: Some(FinishReason::Stop),
        partial: !final_chunk,
        turn_complete: final_chunk,
        interrupted: false,
        error_code: None,
        error_message: None,
        citation_metadata: None,
        provider_metadata: None,
        interaction_id: None,
    }
}

fn usage(prompt: &str, model: &str) -> UsageMetadata {
    let prompt_tokens = (prompt.len() / 4).max(1) as i32;
    UsageMetadata {
        prompt_token_count: prompt_tokens,
        candidates_token_count: (model.len() / 4).max(1) as i32,
        total_token_count: prompt_tokens + (model.len() / 4).max(1) as i32,
        ..Default::default()
    }
}

fn text_of(part: &Part) -> Option<String> {
    match part {
        Part::Text { text } => Some(text.clone()),
        _ => None,
    }
}

fn decode_text(value: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

/// Resolve the agent instruction preamble: the last user content before the
/// current user turn.
fn instructions_text(req: &LlmRequest) -> String {
    req.contents
        .iter()
        .filter(|content| content.role == "user")
        .flat_map(|content| content.parts.iter())
        .filter_map(text_of)
        .collect::<Vec<_>>()
        .join("\n")
}

fn instruction_of(req: &LlmRequest) -> String {
    req.contents
        .iter()
        .find(|content| content.role == "user")
        .map(|content| {
            content
                .parts
                .iter()
                .filter_map(text_of)
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Build the deterministic diagnostic model for tests and the standalone
/// proof. Production model selection goes through configured provider
/// profiles, never process environment reads.
pub fn model_for(reference: &str) -> StdResult<Arc<dyn Llm>, AdkError> {
    crate::execution::providers::diagnostic_model(reference)
}
