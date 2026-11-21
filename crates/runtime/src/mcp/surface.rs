//! Bounded resource and prompt operations over one live native MCP client.
//!
//! Discovery, protocol calls, and schema conversion stay in the pinned ADK
//! `McpToolset`. This module applies the runtime-owned parts: the per-server
//! allowlist, per-entry grants, a deadline, a serialized output ceiling, stable
//! namespaced identifiers, and typed errors. Local stdio and remote HTTP/SSE
//! connections both call it, so an operator sees one surface.

use crate::mcp::config::McpServerConfig;
use crate::mcp::names;
use adk_tool::mcp::{AdkClientHandler, McpToolset, ResourceContents};
use common::{
    ApiError, McpPromptArgument, McpPromptContent, McpPromptDescriptor, McpPromptMessage,
    McpResourceContent, McpResourceDescriptor,
};
use std::collections::BTreeSet;
use std::future::Future;
use std::time::Duration;

/// Largest number of prompt messages returned by one retrieval.
pub const MAX_PROMPT_MESSAGES: usize = 64;
/// Largest resource or prompt identifier accepted from a server.
pub const MAX_IDENTIFIER_CHARS: usize = 512;

type Client = McpToolset<AdkClientHandler>;

/// Allowlists, grants, and bounds for one configured server.
pub(super) struct SurfacePolicy<'a> {
    /// Configured connection id.
    pub server: &'a str,
    /// Resource URIs and templates this instance may expose.
    pub allowed_resources: &'a [String],
    /// Resources that additionally require an explicit grant.
    pub protected_resources: &'a [String],
    /// Prompt names this instance may expose.
    pub allowed_prompts: &'a [String],
    /// Prompts that additionally require an explicit grant.
    pub protected_prompts: &'a [String],
    /// Instance or profile grants already resolved for this caller.
    pub grants: &'a BTreeSet<String>,
    /// Deadline for one protocol call.
    pub timeout: Duration,
    /// Serialized text ceiling for one result.
    pub max_output_bytes: usize,
}

impl SurfacePolicy<'_> {
    fn resource_permitted(&self, uri: &str) -> Result<(), ApiError> {
        if !self.allowed_resources.iter().any(|allowed| allowed == uri) {
            return Err(ApiError::new(
                "mcp.resource_denied",
                "the resource is not in the configured allowlist",
            ));
        }
        if self.protected_resources.iter().any(|value| value == uri)
            && !names::granted(self.grants, &names::resource_grant(self.server, uri))
        {
            return Err(ApiError::new(
                "mcp.resource_denied",
                "the protected resource has no explicit grant",
            ));
        }
        Ok(())
    }

    fn prompt_permitted(&self, name: &str) -> Result<(), ApiError> {
        if !self.allowed_prompts.iter().any(|allowed| allowed == name) {
            return Err(ApiError::new(
                "mcp.prompt_denied",
                "the prompt is not in the configured allowlist",
            ));
        }
        if self.protected_prompts.iter().any(|value| value == name)
            && !names::granted(self.grants, &names::prompt_grant(self.server, name))
        {
            return Err(ApiError::new(
                "mcp.prompt_denied",
                "the protected prompt has no explicit grant",
            ));
        }
        Ok(())
    }
}

/// Project the policy from validated operator configuration.
pub(super) fn policy<'a>(
    config: &'a McpServerConfig,
    id: &'a str,
    grants: &'a BTreeSet<String>,
) -> SurfacePolicy<'a> {
    SurfacePolicy {
        server: id,
        allowed_resources: &config.allowed_resources,
        protected_resources: &config.protected_resources,
        allowed_prompts: &config.allowed_prompts,
        protected_prompts: &config.protected_prompts,
        grants,
        timeout: Duration::from_millis(config.call_timeout_ms.max(1)),
        max_output_bytes: config.max_output_bytes.max(1),
    }
}

/// List allowlisted static resources and templates.
pub(super) async fn list_resources(
    client: &Client,
    policy: &SurfacePolicy<'_>,
) -> Result<Vec<McpResourceDescriptor>, ApiError> {
    let resources = call(client.list_resources(), policy.timeout, "resource", None).await?;
    let templates = call(
        client.list_resource_templates(),
        policy.timeout,
        "resource",
        None,
    )
    .await?;
    let mut descriptors = Vec::new();
    for resource in resources {
        if policy.resource_permitted(&resource.uri).is_err() {
            continue;
        }
        if !valid_identifier(&resource.uri) {
            continue;
        }
        descriptors.push(McpResourceDescriptor {
            id: names::resource_id(policy.server, &resource.uri),
            server: policy.server.to_string(),
            uri: resource.uri,
            name: resource.name,
            title: resource.title,
            description: resource.description,
            mime_type: resource.mime_type,
            size: resource.size,
            template: false,
        });
    }
    for template in templates {
        if policy.resource_permitted(&template.uri_template).is_err() {
            continue;
        }
        if !valid_identifier(&template.uri_template) {
            continue;
        }
        descriptors.push(McpResourceDescriptor {
            id: names::resource_id(policy.server, &template.uri_template),
            server: policy.server.to_string(),
            uri: template.uri_template,
            name: template.name,
            title: template.title,
            description: template.description,
            mime_type: template.mime_type,
            size: None,
            template: true,
        });
    }
    descriptors.sort_by(|left, right| left.uri.cmp(&right.uri));
    Ok(descriptors)
}

/// Read one allowlisted resource with a bounded projection.
pub(super) async fn read_resource(
    client: &Client,
    policy: &SurfacePolicy<'_>,
    uri: &str,
) -> Result<McpResourceContent, ApiError> {
    if !valid_identifier(uri) {
        return Err(ApiError::new(
            "mcp.resource_invalid",
            "the resource URI is not a bounded identifier",
        ));
    }
    policy.resource_permitted(uri)?;
    let contents = call(
        client.read_resource(uri),
        policy.timeout,
        "resource",
        Some("mcp.resource_unknown"),
    )
    .await?;
    let mut text = String::new();
    let mut mime_type = None;
    let mut blob_bytes = None;
    let mut truncated = false;
    for content in contents {
        match content {
            ResourceContents::TextResourceContents {
                mime_type: mime,
                text: value,
                ..
            } => {
                mime_type = mime_type.or(mime);
                push_bounded(&mut text, &value, policy.max_output_bytes, &mut truncated);
            }
            ResourceContents::BlobResourceContents {
                mime_type: mime,
                blob,
                ..
            } => {
                mime_type = mime_type.or(mime);
                blob_bytes = Some(decoded_len(&blob));
            }
            _ => {}
        }
    }
    Ok(McpResourceContent {
        id: names::resource_id(policy.server, uri),
        server: policy.server.to_string(),
        uri: uri.to_string(),
        mime_type,
        text: (!text.is_empty()).then_some(text),
        blob_bytes,
        truncated,
    })
}

/// List allowlisted prompts.
pub(super) async fn list_prompts(
    client: &Client,
    policy: &SurfacePolicy<'_>,
) -> Result<Vec<McpPromptDescriptor>, ApiError> {
    let prompts = call(client.list_prompts(), policy.timeout, "prompt", None).await?;
    let mut descriptors = Vec::new();
    for prompt in prompts {
        if policy.prompt_permitted(&prompt.name).is_err() {
            continue;
        }
        if !valid_identifier(&prompt.name) {
            continue;
        }
        descriptors.push(McpPromptDescriptor {
            id: names::prompt_id(policy.server, &prompt.name),
            server: policy.server.to_string(),
            name: prompt.name,
            title: prompt.title,
            description: prompt.description,
            arguments: prompt
                .arguments
                .unwrap_or_default()
                .into_iter()
                .map(|argument| McpPromptArgument {
                    name: argument.name,
                    description: argument.description,
                    required: argument.required.unwrap_or(false),
                })
                .collect(),
        });
    }
    descriptors.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(descriptors)
}

/// Retrieve one allowlisted prompt with a bounded projection.
pub(super) async fn get_prompt(
    client: &Client,
    policy: &SurfacePolicy<'_>,
    name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<McpPromptContent, ApiError> {
    if !valid_identifier(name) {
        return Err(ApiError::new(
            "mcp.prompt_invalid",
            "the prompt name is not a bounded identifier",
        ));
    }
    policy.prompt_permitted(name)?;
    let result = call(
        client.get_prompt(name, Some(arguments)),
        policy.timeout,
        "prompt",
        Some("mcp.prompt_unknown"),
    )
    .await?;
    let mut messages = Vec::new();
    let mut used = 0usize;
    let mut truncated = result.messages.len() > MAX_PROMPT_MESSAGES;
    for message in result.messages.into_iter().take(MAX_PROMPT_MESSAGES) {
        let role = match message.role {
            adk_tool::mcp::rmcp::model::Role::User => "user",
            adk_tool::mcp::rmcp::model::Role::Assistant => "assistant",
        };
        let text = match message.content {
            adk_tool::mcp::rmcp::model::ContentBlock::Text(content) => content.text,
            adk_tool::mcp::rmcp::model::ContentBlock::Resource(resource) => {
                match resource.resource {
                    ResourceContents::TextResourceContents { text, .. } => text,
                    _ => "[non-text content omitted]".into(),
                }
            }
            _ => "[non-text content omitted]".into(),
        };
        let remaining = policy.max_output_bytes.saturating_sub(used);
        if remaining == 0 {
            truncated = true;
            break;
        }
        let mut bounded = String::new();
        let mut local_truncated = false;
        push_bounded(&mut bounded, &text, remaining, &mut local_truncated);
        truncated |= local_truncated;
        used += bounded.len();
        messages.push(McpPromptMessage {
            role: role.to_string(),
            text: bounded,
        });
        if local_truncated {
            break;
        }
    }
    Ok(McpPromptContent {
        id: names::prompt_id(policy.server, name),
        server: policy.server.to_string(),
        name: name.to_string(),
        description: result.description,
        messages,
        truncated,
    })
}

async fn call<T>(
    future: impl Future<Output = adk_rust::Result<T>>,
    timeout: Duration,
    surface: &'static str,
    unknown: Option<&'static str>,
) -> Result<T, ApiError> {
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| {
            ApiError::new(
                "mcp.timeout",
                format!("MCP {surface} call exceeded its deadline"),
            )
        })?
        .map_err(|error| transport_error(surface, unknown, &error.to_string()))
}

fn transport_error(
    surface: &'static str,
    unknown: Option<&'static str>,
    message: &str,
) -> ApiError {
    let lower = message.to_ascii_lowercase();
    if unknown.is_some() && (lower.contains("not found") || lower.contains("does not publish")) {
        return ApiError::new(
            unknown.unwrap_or("mcp.capability_unknown"),
            "the server does not publish the requested capability",
        );
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return ApiError::new("mcp.timeout", "the MCP server did not respond in time");
    }
    if lower.contains("connection closed")
        || lower.contains("transport")
        || lower.contains("broken pipe")
        || lower.contains("eof")
    {
        return ApiError::new(
            "mcp.transport_unavailable",
            "the MCP server connection is not available",
        );
    }
    ApiError::new(
        format!("mcp.{surface}_failed"),
        "the MCP server rejected the request",
    )
}

/// Bound one serialized result before it reaches the model or event stream.
pub(super) fn bound_value(value: serde_json::Value, max_bytes: usize) -> serde_json::Value {
    let text = value.to_string();
    if text.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    serde_json::json!({
        "truncated": true,
        "original_bytes": text.len(),
        "preview": &text[..end],
    })
}

/// Decoded byte length of a base64 blob without ever returning its content.
fn decoded_len(blob: &str) -> usize {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(blob)
        .map(|bytes| bytes.len())
        .unwrap_or(blob.len())
}

fn push_bounded(target: &mut String, value: &str, limit: usize, truncated: &mut bool) {
    if target.len() >= limit {
        *truncated = true;
        return;
    }
    let mut end = value.len().min(limit - target.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&value[..end]);
    if end < value.len() {
        *truncated = true;
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_IDENTIFIER_CHARS
        && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_bounded_and_control_free() {
        assert!(valid_identifier("file:///notes/a.txt"));
        assert!(!valid_identifier(""));
        assert!(!valid_identifier("line\nbreak"));
        assert!(!valid_identifier(&"a".repeat(MAX_IDENTIFIER_CHARS + 1)));
    }

    #[test]
    fn text_projection_stops_at_the_ceiling_on_a_char_boundary() {
        let mut text = String::new();
        let mut truncated = false;
        push_bounded(&mut text, "abcdef", 4, &mut truncated);
        assert_eq!(text, "abcd");
        assert!(truncated);

        let mut text = String::new();
        let mut truncated = false;
        push_bounded(&mut text, "éé", 3, &mut truncated);
        assert_eq!(text, "é");
        assert!(truncated);
    }
}
