//! Instance-owned MCP connections and execution policy.
//!
//! Native ADK MCP toolsets perform discovery and protocol calls. This module
//! owns operator configuration, local child lifecycle, remote destination and
//! credential policy, default-deny exposure checks, resource and prompt
//! operations, observable health, and per-call authority rechecks that the
//! framework does not supply.
//!
//! Transports are stdio and Streamable HTTP through the pinned client, plus one
//! documented legacy SSE compatibility adapter (`sse`) for `2024-11-05`
//! clients. Every transport runs through the same registry, credential, and
//! allowlist boundary.

mod config;
mod credentials;
mod health;
mod local;
mod names;
mod policy;
mod registry;
mod remote;
mod sse;
mod surface;
mod toolset;

/// Root of the namespaced MCP identifier grammar.
pub(crate) const IDENTIFIER_ROOT: &str = "mcp";

pub(crate) use config::validate_surface;
pub use config::{McpConfig, McpCredentialConfig, McpServerConfig, McpServerPolicy};
pub use credentials::{CredentialGrant, McpCredentials};
pub use health::{McpServerHealth, McpState};
pub use names::{prompt_id, resource_id, server_of_tool, tool_id};
pub use registry::McpRegistry;
pub use remote::{error_code, RemoteMcpRegistry};
