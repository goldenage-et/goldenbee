//! Observable MCP server state.
//!
//! A connection that failed or was never established is reported as
//! `unavailable`, never as `connected`. Dependent tool, resource, or prompt
//! calls fail closed while the state is not `connected`.

use crate::mcp::config::McpServerConfig;
use common::{McpFailureView, McpServerInventory};

/// Lifecycle state of one configured connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpState {
    /// A live session exists.
    Connected,
    /// Configured but not live; dependent calls fail closed.
    Unavailable,
    /// The operator disabled the definition.
    Disabled,
    /// A restart or reconnect is in progress.
    Restarting,
}

impl McpState {
    /// Stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Unavailable => "unavailable",
            Self::Disabled => "disabled",
            Self::Restarting => "restarting",
        }
    }

    /// Whether dependent calls are allowed to proceed.
    pub fn admits_calls(self) -> bool {
        matches!(self, Self::Connected)
    }
}

/// Observed state and counters for one connection.
#[derive(Debug, Clone)]
pub struct McpServerHealth {
    /// Configured connection id.
    pub id: String,
    /// `stdio`, `http`, or `sse`.
    pub transport: String,
    /// Current lifecycle state.
    pub state: McpState,
    /// Number of successful explicit restarts.
    pub restart_count: u32,
    /// Unix milliseconds when the current session was established.
    pub connected_at: Option<i64>,
    /// Redacted last failure.
    pub last_error: Option<McpFailureView>,
}

impl McpServerHealth {
    /// Inventory projection for `/v1/capabilities` and `/v1/mcp/servers`.
    pub fn inventory(&self, config: &McpServerConfig) -> McpServerInventory {
        McpServerInventory {
            id: self.id.clone(),
            transport: self.transport.clone(),
            state: self.state.as_str().to_string(),
            enabled: config.enabled,
            restart_count: self.restart_count,
            connected_at: self.connected_at,
            last_error: self.last_error.clone(),
            allowed_tools: config.allowed_tools.len(),
            allowed_resources: config.allowed_resources.len(),
            allowed_prompts: config.allowed_prompts.len(),
        }
    }
}

/// Transport label derived from validated operator configuration.
pub fn transport_name(config: &McpServerConfig) -> String {
    if config.is_remote() {
        if config.uses_sse() {
            "sse".into()
        } else {
            "http".into()
        }
    } else {
        "stdio".into()
    }
}

/// Split a `code: message` connection failure into a redacted view.
///
/// Local connections report failures as strings; remote connections report
/// typed codes. Both become one inventory shape.
pub fn failure_view(failure: &str) -> McpFailureView {
    let (code, message) = match failure.split_once(": ") {
        Some((code, message)) if code.starts_with("mcp.") => {
            (code.to_string(), message.to_string())
        }
        _ if failure.starts_with("mcp.") => (failure.to_string(), String::new()),
        _ => ("mcp.error".to_string(), failure.to_string()),
    };
    McpFailureView {
        code,
        message: common::redact(&message).chars().take(512).collect(),
    }
}

/// Build one health record for a configured connection.
pub fn record(
    id: &str,
    config: &McpServerConfig,
    state: McpState,
    restart_count: u32,
    connected_at: Option<i64>,
    last_error: Option<McpFailureView>,
) -> McpServerHealth {
    McpServerHealth {
        id: id.to_string(),
        transport: transport_name(config),
        state,
        restart_count,
        connected_at,
        last_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_views_keep_the_stable_code_and_redact_the_detail() {
        let view = failure_view("mcp.start_failed: cannot start sk-canary-value");
        assert_eq!(view.code, "mcp.start_failed");
        assert!(!view.message.contains("sk-canary-value"));
        let view = failure_view("mcp.transport_unavailable");
        assert_eq!(view.code, "mcp.transport_unavailable");
    }

    #[test]
    fn only_connected_admits_dependent_calls() {
        assert!(McpState::Connected.admits_calls());
        assert!(!McpState::Unavailable.admits_calls());
        assert!(!McpState::Restarting.admits_calls());
        assert!(!McpState::Disabled.admits_calls());
    }
}
