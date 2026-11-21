//! Default-deny toolset and tool adapter for one MCP connection.

use super::registry::McpRegistry;
use adk_rust::{AdkError, ReadonlyContext, Tool, ToolContext, Toolset};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::{Arc, Weak};
use std::time::Duration;

/// A toolset resolved for one immutable profile revision and one agent member.
/// It borrows the instance registry so removing the connection also removes the
/// tools on the next model turn.
pub(super) struct McpToolsetView {
    registry: Weak<McpRegistry>,
    server: String,
    name: String,
    grants: BTreeSet<String>,
    kernel: Option<crate::tools::ToolKernel>,
}

impl McpToolsetView {
    pub(super) fn new(
        registry: Weak<McpRegistry>,
        server: String,
        grants: BTreeSet<String>,
        kernel: Option<crate::tools::ToolKernel>,
    ) -> Self {
        let name = format!("mcp:{server}");
        Self {
            registry,
            server,
            name,
            grants,
            kernel,
        }
    }
}

#[async_trait]
impl Toolset for McpToolsetView {
    fn name(&self) -> &str {
        &self.name
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> adk_rust::Result<Vec<Arc<dyn Tool>>> {
        let registry = self.registry.upgrade().ok_or_else(|| {
            AdkError::tool("mcp.connection_removed: worker instance is shutting down")
        })?;
        let connection = registry
            .connection(&self.server)
            .await
            .map_err(AdkError::tool)?;
        let discovered = tokio::time::timeout(
            Duration::from_millis(connection.config.call_timeout_ms.max(1)),
            connection.toolset.tools(ctx),
        )
        .await
        .map_err(|_| {
            AdkError::tool(format!(
                "mcp.timeout: discovery for '{}' exceeded {} ms",
                self.server, connection.config.call_timeout_ms
            ))
        })??;
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        let mut exposed_names = BTreeSet::new();
        for inner in discovered {
            let original = inner.name().to_string();
            if !connection
                .config
                .allowed_tools
                .iter()
                .any(|allowed| allowed == &original)
            {
                continue;
            }
            if !self.permitted(&connection.config, &original) {
                continue;
            }
            let exposed = format!("mcp__{}__{}", sanitize(&self.server), sanitize(&original));
            if !exposed_names.insert(exposed.clone()) {
                continue;
            }
            let guard = Arc::new(McpToolGuard {
                registry: self.registry.clone(),
                server: self.server.clone(),
                original,
                exposed,
                description: inner.description().to_string(),
                parameters: inner.parameters_schema(),
                response: inner.response_schema(),
                call_timeout_ms: connection.config.call_timeout_ms,
                max_output_bytes: connection.config.max_output_bytes,
                grants: self.grants.clone(),
            }) as Arc<dyn Tool>;
            let guard = match &self.kernel {
                Some(kernel) => kernel.wrap(
                    guard,
                    crate::tools::ToolDescriptor::new("mcp:call")
                        .risk(crate::tools::ToolRisk::Medium)
                        .effect(crate::tools::ToolEffect::None),
                ),
                None => guard,
            };
            tools.push(guard);
        }
        Ok(tools)
    }
}

impl McpToolsetView {
    fn permitted(&self, config: &super::config::McpServerConfig, tool: &str) -> bool {
        if config.protected_tools.iter().any(|name| name == tool) {
            return self.grants.contains("*")
                || self.grants.contains(&format!("mcp:{}:{tool}", self.server));
        }
        true
    }
}

struct McpToolGuard {
    registry: Weak<McpRegistry>,
    server: String,
    original: String,
    exposed: String,
    description: String,
    parameters: Option<Value>,
    response: Option<Value>,
    call_timeout_ms: u64,
    max_output_bytes: usize,
    grants: BTreeSet<String>,
}

#[async_trait]
impl Tool for McpToolGuard {
    fn name(&self) -> &str {
        &self.exposed
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.parameters.clone()
    }

    fn response_schema(&self) -> Option<Value> {
        self.response.clone()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let registry = self.registry.upgrade().ok_or_else(|| {
            AdkError::tool("mcp.connection_removed: worker instance is shutting down")
        })?;
        // Re-read instance grants, connection state, and the configured
        // allowlist on every call. Discovery is a snapshot, not authority.
        let connection = registry
            .connection(&self.server)
            .await
            .map_err(AdkError::tool)?;
        if !connection
            .config
            .allowed_tools
            .iter()
            .any(|allowed| allowed == &self.original)
        {
            return Err(AdkError::tool(format!(
                "mcp.denied: tool '{}' is not in the configured allowlist",
                self.original
            )));
        }
        if connection
            .config
            .protected_tools
            .iter()
            .any(|name| name == &self.original)
            && !self.grants.contains("*")
            && !self
                .grants
                .contains(&format!("mcp:{}:{}", self.server, self.original))
        {
            return Err(AdkError::tool(format!(
                "mcp.denied: protected tool '{}' has no profile grant",
                self.original
            )));
        }
        let discovered = tokio::time::timeout(
            Duration::from_millis(connection.config.call_timeout_ms.max(1)),
            connection.toolset.tools(Arc::new(StaticContext)),
        )
        .await
        .map_err(|_| {
            AdkError::tool(format!(
                "mcp.timeout: discovery for '{}' exceeded {} ms",
                self.server, connection.config.call_timeout_ms
            ))
        })??;
        let inner = discovered
            .into_iter()
            .find(|tool| tool.name() == self.original)
            .ok_or_else(|| {
                AdkError::tool(format!(
                    "mcp.capability_missing: server no longer publishes tool '{}'",
                    self.original
                ))
            })?;
        let result = tokio::time::timeout(
            Duration::from_millis(self.call_timeout_ms.max(1)),
            inner.execute(ctx, args),
        )
        .await
        .map_err(|_| {
            AdkError::tool(format!(
                "mcp.timeout: tool '{}' exceeded {} ms",
                self.original, self.call_timeout_ms
            ))
        })?
        .map_err(|error| {
            AdkError::tool(format!(
                "mcp.call_failed: {}",
                common::redact(&error.to_string())
            ))
        })?;
        Ok(bounded(result, self.max_output_bytes.max(1)))
    }
}

/// Bound the serialized result before it reaches the model or event stream.
fn bounded(value: Value, max_bytes: usize) -> Value {
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

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// Discovery needs a context. The inner native toolset ignores it for
/// filtering, but the pinned trait requires one; no session data is used.
struct StaticContext;

#[async_trait]
impl ReadonlyContext for StaticContext {
    fn invocation_id(&self) -> &str {
        "mcp-discovery"
    }

    fn agent_name(&self) -> &str {
        "mcp"
    }

    fn user_id(&self) -> &str {
        "mcp"
    }

    fn app_name(&self) -> &str {
        "mcp"
    }

    fn session_id(&self) -> &str {
        "mcp-discovery"
    }

    fn branch(&self) -> &str {
        ""
    }

    fn user_content(&self) -> &adk_rust::Content {
        static CONTENT: std::sync::OnceLock<adk_rust::Content> = std::sync::OnceLock::new();
        CONTENT.get_or_init(|| adk_rust::Content::new("user").with_text("mcp discovery"))
    }
}
