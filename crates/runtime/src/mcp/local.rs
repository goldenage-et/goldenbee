//! Child-process construction for one local stdio MCP connection.

use super::config::McpServerConfig;
use adk_tool::mcp::rmcp::transport::TokioChildProcess;
use adk_tool::mcp::{
    AdkClientHandler, AutoDeclineElicitationHandler, ClientLifecycleMode, ClientServiceExt,
    McpSchemaLimits, McpToolset,
};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

/// Environment variable that receives a resolved local credential reference.
pub(super) const CREDENTIAL_ENV: &str = "GOLDENBEE_MCP_CREDENTIAL";

/// One initialized native MCP connection owned by the registry.
pub(super) struct Connection {
    pub(super) config: McpServerConfig,
    pub(super) toolset: Arc<McpToolset<AdkClientHandler>>,
}

impl Connection {
    /// Ask the native service loop to stop. Dropping the transport kills the
    /// child, including when a discovered tool still holds a client handle.
    pub(super) async fn close(&self) {
        self.toolset.cancellation_token().await.cancel();
    }
}

/// Start one child process with a cleared environment and initialize the
/// pinned native MCP client within the configured startup deadline.
pub(super) async fn connect(id: &str, config: &McpServerConfig) -> Result<Connection, String> {
    let mut command = Command::new(config.command());
    command.args(&config.args).env_clear().kill_on_drop(true);
    for (name, value) in &config.env {
        let resolved = if let Some(reference) = value.strip_prefix("env:") {
            std::env::var(reference).map_err(|_| {
                format!("mcp.env_unavailable: environment reference for '{name}' is not set")
            })?
        } else {
            value.clone()
        };
        command.env(name, resolved);
    }
    if let Some(credential) = &config.credential {
        let Some(reference) = credential.strip_prefix("env:") else {
            return Err(
                "mcp.credential_unavailable: local MCP credentials must use env: references".into(),
            );
        };
        let value = std::env::var(reference).map_err(|_| {
            "mcp.credential_unavailable: referenced environment credential is not set".to_string()
        })?;
        command.env(CREDENTIAL_ENV, value);
    }

    let (transport, _stderr) = TokioChildProcess::builder(command)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "mcp.start_failed: child process could not be started".to_string())?;
    let handler = AdkClientHandler::new(Arc::new(AutoDeclineElicitationHandler));
    let client = tokio::time::timeout(
        Duration::from_millis(config.startup_timeout_ms.max(1)),
        handler.serve_with_lifecycle(transport, ClientLifecycleMode::Initialize),
    )
    .await
    .map_err(|_| "mcp.startup_timeout: MCP initialization exceeded its deadline".to_string())?
    .map_err(|error| {
        format!(
            "mcp.initialize_failed: {}",
            common::redact(&error.to_string())
        )
    })?;

    let allowed = config.allowed_tools.clone();
    let toolset = McpToolset::new(client)
        .with_name(format!("mcp:{id}"))
        .with_filter(move |name| allowed.iter().any(|allowed| allowed == name))
        .with_schema_limits(McpSchemaLimits::default());
    Ok(Connection {
        config: config.clone(),
        toolset: Arc::new(toolset),
    })
}
