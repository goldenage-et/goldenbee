//! Instance-owned MCP connections and execution policy.
//!
//! Native ADK MCP toolsets perform discovery and protocol calls. This module
//! owns operator configuration, local child lifecycle, remote destination and
//! credential policy, default-deny exposure checks, and per-call authority
//! rechecks that the framework does not supply.

mod config;
mod credentials;
mod local;
mod policy;
mod registry;
mod remote;
mod toolset;

pub use config::{McpConfig, McpCredentialConfig, McpServerConfig, McpServerPolicy};
pub use credentials::{CredentialGrant, McpCredentials};
pub use registry::McpRegistry;
pub use remote::{error_code, RemoteMcpRegistry};
