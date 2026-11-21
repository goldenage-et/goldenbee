//! Wire shapes for MCP server health, resources, and prompts.
//!
//! The runtime owns discovery and policy. These types carry the bounded,
//! namespaced projection a Client or operator needs; they never contain a
//! credential value, a child environment, or a raw host path.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Stable identifier prefix for one namespaced MCP resource.
pub const RESOURCE_ID_PREFIX: &str = "mcp:";
/// Stable identifier segment that distinguishes a resource from a tool.
pub const RESOURCE_ID_KIND: &str = "resource";
/// Stable identifier segment that distinguishes a prompt from a tool.
pub const PROMPT_ID_KIND: &str = "prompt";

/// Health and policy projection of one configured MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerInventory {
    /// Operator-assigned connection id.
    pub id: String,
    /// `stdio`, `http`, or `sse`.
    pub transport: String,
    /// `connected`, `unavailable`, `disabled`, or `restarting`.
    pub state: String,
    /// Whether the operator enabled the definition.
    pub enabled: bool,
    /// Number of successful explicit restarts since startup.
    #[serde(default)]
    pub restart_count: u32,
    /// Unix milliseconds of the current connection, when connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_at: Option<i64>,
    /// Redacted last connection or call failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<McpFailureView>,
    /// Configured tool ceiling size.
    #[serde(default)]
    pub allowed_tools: usize,
    /// Configured resource ceiling size.
    #[serde(default)]
    pub allowed_resources: usize,
    /// Configured prompt ceiling size.
    #[serde(default)]
    pub allowed_prompts: usize,
}

/// Redacted stable failure observed for one MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpFailureView {
    /// Stable `mcp.*` code.
    pub code: String,
    /// Redacted operator-facing detail.
    pub message: String,
}

/// Health and policy projection of every configured MCP server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpCapabilityInventory {
    /// Servers ordered by id.
    #[serde(default)]
    pub servers: Vec<McpServerInventory>,
}

/// One discoverable resource with a stable namespaced id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpResourceDescriptor {
    /// Namespaced id, `mcp:<server>:resource:<uri>`.
    pub id: String,
    /// Owning connection id.
    pub server: String,
    /// Native resource URI.
    pub uri: String,
    /// Programmatic resource name.
    pub name: String,
    /// Optional human-readable title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Advertised size in bytes, when the server reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Whether this entry is a URI template rather than a static resource.
    #[serde(default)]
    pub template: bool,
}

/// One bounded resource read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpResourceContent {
    /// Namespaced id of the resource that was read.
    pub id: String,
    /// Owning connection id.
    pub server: String,
    /// Native resource URI returned by the server.
    pub uri: String,
    /// MIME type the server declared for the content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Text content, bounded by the configured result ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Binary content length. The runtime never returns the bytes themselves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_bytes: Option<usize>,
    /// Whether text was truncated at the configured ceiling.
    #[serde(default)]
    pub truncated: bool,
}

/// One discoverable prompt with a stable namespaced id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpPromptDescriptor {
    /// Namespaced id, `mcp:<server>:prompt:<name>`.
    pub id: String,
    /// Owning connection id.
    pub server: String,
    /// Native prompt name.
    pub name: String,
    /// Optional human-readable title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Declared prompt arguments.
    #[serde(default)]
    pub arguments: Vec<McpPromptArgument>,
}

/// One declared prompt argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpPromptArgument {
    /// Argument name.
    pub name: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the server requires this argument.
    #[serde(default)]
    pub required: bool,
}

/// One resource read request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpResourceReadRequest {
    /// Resource URI, which must also appear in the operator allowlist.
    pub uri: String,
}

/// Arguments for one prompt retrieval.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpPromptRequest {
    /// String arguments by declared name.
    #[serde(default)]
    pub arguments: BTreeMap<String, String>,
}

/// One bounded prompt retrieval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpPromptContent {
    /// Namespaced id of the prompt that was retrieved.
    pub id: String,
    /// Owning connection id.
    pub server: String,
    /// Native prompt name.
    pub name: String,
    /// Optional prompt description returned with the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Ordered prompt messages.
    #[serde(default)]
    pub messages: Vec<McpPromptMessage>,
    /// Whether the message list or total text was truncated.
    #[serde(default)]
    pub truncated: bool,
}

/// One prompt message projected as role plus bounded text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpPromptMessage {
    /// `user` or `assistant`.
    pub role: String,
    /// Message text. Non-text content is omitted and never returned raw.
    pub text: String,
}
