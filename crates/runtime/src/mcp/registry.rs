//! Instance-owned registry and lifecycle for local MCP child processes.

use super::config::McpServerConfig;
use super::health::{self, McpServerHealth, McpState};
use super::local::{connect, Connection};
use super::surface;
use super::toolset::McpToolsetView;
use adk_rust::Toolset;
use common::{
    ApiError, McpPromptContent, McpPromptDescriptor, McpResourceContent, McpResourceDescriptor,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

#[derive(Default)]
struct RegistryState {
    configured: BTreeMap<String, McpServerConfig>,
    connections: BTreeMap<String, Arc<Connection>>,
    failures: BTreeMap<String, String>,
    restarting: BTreeSet<String>,
    restarts: BTreeMap<String, u32>,
    connected_at: BTreeMap<String, i64>,
}

/// One process's MCP connections. Instances never share this registry, so two
/// Workers in one package get separate children, environments, and tools.
pub struct McpRegistry {
    grants: RwLock<BTreeSet<String>>,
    state: RwLock<RegistryState>,
    lifecycle: Mutex<()>,
    kernel: Option<crate::tools::ToolKernel>,
}

impl McpRegistry {
    /// Create an empty registry bound to the instance grants in effect.
    pub fn new(grants: &[String], kernel: Option<crate::tools::ToolKernel>) -> Self {
        Self {
            grants: RwLock::new(grants.iter().cloned().collect()),
            state: RwLock::new(RegistryState::default()),
            lifecycle: Mutex::new(()),
            kernel,
        }
    }

    /// Whether the operator configured this connection id.
    pub async fn known(&self, id: &str) -> bool {
        self.state.read().await.configured.contains_key(id)
    }

    /// Configured server ids, including disabled definitions.
    pub async fn configured_ids(&self) -> BTreeSet<String> {
        self.state.read().await.configured.keys().cloned().collect()
    }

    /// Start enabled connections and stop connections removed by the new
    /// configuration. Connection failures are recorded and fail closed for the
    /// affected connection only.
    pub async fn reconcile(&self, configs: &BTreeMap<String, McpServerConfig>, grants: &[String]) {
        let _lifecycle = self.lifecycle.lock().await;
        *self.grants.write().await = grants.iter().cloned().collect();
        let current = self.state.read().await.connections.clone();
        let mut next = BTreeMap::new();
        let mut failures = BTreeMap::new();
        let mut connected_at = BTreeMap::new();
        let previous_connected = self.state.read().await.connected_at.clone();
        for (id, config) in configs {
            if config.is_remote() {
                // Remote HTTP and SSE connections are owned by the remote
                // registry. Keeping them out of this child-process registry
                // prevents a duplicate health entry and a second transport.
                continue;
            }
            if !config.enabled {
                continue;
            }
            let reuse = current
                .get(id)
                .filter(|connection| connection.config.signature() == config.signature())
                .cloned();
            if let Some(connection) = reuse {
                next.insert(id.clone(), connection);
                if let Some(previous) = previous_connected.get(id) {
                    connected_at.insert(id.clone(), *previous);
                }
                continue;
            }
            match connect(id, config).await {
                Ok(connection) => {
                    next.insert(id.clone(), Arc::new(connection));
                    connected_at.insert(id.clone(), common::now_ms());
                }
                Err(error) => {
                    failures.insert(id.clone(), error);
                }
            }
        }
        let removed: Vec<Arc<Connection>> = current
            .values()
            .filter(|old| !next.values().any(|new| Arc::ptr_eq(old, new)))
            .cloned()
            .collect();
        let mut state = self.state.write().await;
        state.configured = configs
            .iter()
            .filter(|(_, config)| !config.is_remote())
            .map(|(id, config)| (id.clone(), config.clone()))
            .collect();
        state.connections = next;
        state.failures = failures;
        state.restarting.clear();
        state.connected_at = connected_at;
        drop(state);
        for connection in removed {
            connection.close().await;
        }
    }

    /// Stop every child owned by this instance.
    pub async fn shutdown(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        let connections = {
            let mut state = self.state.write().await;
            let connections = std::mem::take(&mut state.connections);
            state.failures.clear();
            state.restarting.clear();
            state.connected_at.clear();
            connections
        };
        for connection in connections.into_values() {
            connection.close().await;
        }
    }

    /// Observable state of every configured connection.
    ///
    /// A live entry is probed once so a child that exited without cancelling
    /// its native session is reported as unavailable instead of remaining
    /// `connected` forever. A failed probe removes the dead session, so
    /// dependent calls fail closed until an operator restart.
    pub async fn health(&self) -> Vec<McpServerHealth> {
        let (configured, connections) = {
            let state = self.state.read().await;
            (
                state.configured.clone(),
                state
                    .connections
                    .iter()
                    .map(|(id, connection)| (id.clone(), connection.clone()))
                    .collect::<BTreeMap<_, _>>(),
            )
        };
        let mut failures = BTreeMap::new();
        for (id, connection) in &connections {
            let Some(config) = configured.get(id) else {
                continue;
            };
            if !config.enabled {
                continue;
            }
            let deadline = Duration::from_millis(config.call_timeout_ms.clamp(1, 1_000));
            match tokio::time::timeout(deadline, connection.toolset.list_prompts()).await {
                Ok(Ok(_)) => {}
                Ok(Err(_)) => {
                    failures.insert(
                        id.clone(),
                        "mcp.connection_closed: MCP server is no longer responding".to_string(),
                    );
                }
                Err(_) => {
                    failures.insert(
                        id.clone(),
                        "mcp.timeout: MCP server did not answer a liveness probe".to_string(),
                    );
                }
            }
        }
        if !failures.is_empty() {
            let mut state = self.state.write().await;
            for (id, failure) in failures {
                state.connections.remove(&id);
                state.connected_at.remove(&id);
                state.failures.insert(id, failure);
            }
        }
        self.snapshot_health().await
    }

    /// Inventory projection for `/v1/capabilities` and `/v1/mcp/servers`.
    pub async fn inventory(&self) -> Vec<common::McpServerInventory> {
        let health = self.health().await;
        let state = self.state.read().await;
        health
            .iter()
            .filter_map(|record| {
                state
                    .configured
                    .get(&record.id)
                    .map(|config| record.inventory(config))
            })
            .collect()
    }

    async fn snapshot_health(&self) -> Vec<McpServerHealth> {
        let state = self.state.read().await;
        state
            .configured
            .iter()
            .map(|(id, config)| {
                let current = if !config.enabled {
                    McpState::Disabled
                } else if state.restarting.contains(id) {
                    McpState::Restarting
                } else if state.connections.contains_key(id) {
                    McpState::Connected
                } else {
                    McpState::Unavailable
                };
                health::record(
                    id,
                    config,
                    current,
                    state.restarts.get(id).copied().unwrap_or(0),
                    state.connected_at.get(id).copied(),
                    state
                        .failures
                        .get(id)
                        .map(|failure| health::failure_view(failure)),
                )
            })
            .collect()
    }

    /// Restart one local connection, replacing a failed or stale child.
    ///
    /// A restart is an operator action. Dependent calls are unavailable for the
    /// whole operation, and a failed restart leaves the connection unavailable.
    pub async fn restart(&self, id: &str) -> Result<(), ApiError> {
        let _lifecycle = self.lifecycle.lock().await;
        let config = {
            let state = self.state.read().await;
            state.configured.get(id).cloned().ok_or_else(|| {
                ApiError::new(
                    "mcp.connection_unknown",
                    format!("MCP connection '{id}' is not configured"),
                )
            })?
        };
        if !config.enabled {
            return Err(ApiError::new(
                "mcp.connection_disabled",
                format!("MCP connection '{id}' is disabled"),
            ));
        }
        if config.is_remote() {
            return Err(ApiError::new(
                "mcp.transport_unsupported",
                "remote MCP connections are restarted by the remote registry",
            ));
        }
        if !self.granted(id).await {
            return Err(ApiError::new(
                "mcp.grant_denied",
                "connection is outside instance authority",
            ));
        }
        {
            let mut state = self.state.write().await;
            state.failures.remove(id);
            state.connected_at.remove(id);
            state.restarting.insert(id.to_string());
        }
        let previous = self.state.write().await.connections.remove(id);
        if let Some(previous) = previous {
            previous.close().await;
        }
        let outcome = connect(id, &config).await;
        let mut state = self.state.write().await;
        state.restarting.remove(id);
        match outcome {
            Ok(connection) => {
                state
                    .connections
                    .insert(id.to_string(), Arc::new(connection));
                state.connected_at.insert(id.to_string(), common::now_ms());
                *state.restarts.entry(id.to_string()).or_insert(0) += 1;
                Ok(())
            }
            Err(failure) => {
                state.failures.insert(id.to_string(), failure.clone());
                Err(health_error(&failure))
            }
        }
    }

    /// Discover the allowlisted resources of one connection.
    pub async fn resources(&self, id: &str) -> Result<Vec<McpResourceDescriptor>, ApiError> {
        let connection = self
            .connection(id)
            .await
            .map_err(|failure| health_error(&failure))?;
        let grants = self.grants_snapshot().await;
        let policy = surface::policy(&connection.config, id, &grants);
        surface::list_resources(&connection.toolset, &policy).await
    }

    /// Read one allowlisted resource with a bounded result.
    pub async fn read_resource(&self, id: &str, uri: &str) -> Result<McpResourceContent, ApiError> {
        let connection = self
            .connection(id)
            .await
            .map_err(|failure| health_error(&failure))?;
        let grants = self.grants_snapshot().await;
        let policy = surface::policy(&connection.config, id, &grants);
        surface::read_resource(&connection.toolset, &policy, uri).await
    }

    /// Discover the allowlisted prompts of one connection.
    pub async fn prompts(&self, id: &str) -> Result<Vec<McpPromptDescriptor>, ApiError> {
        let connection = self
            .connection(id)
            .await
            .map_err(|failure| health_error(&failure))?;
        let grants = self.grants_snapshot().await;
        let policy = surface::policy(&connection.config, id, &grants);
        surface::list_prompts(&connection.toolset, &policy).await
    }

    /// Retrieve one allowlisted prompt with a bounded result.
    pub async fn prompt(
        &self,
        id: &str,
        name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<McpPromptContent, ApiError> {
        let connection = self
            .connection(id)
            .await
            .map_err(|failure| health_error(&failure))?;
        let grants = self.grants_snapshot().await;
        let policy = surface::policy(&connection.config, id, &grants);
        surface::get_prompt(&connection.toolset, &policy, name, arguments).await
    }

    /// Resolve a live connection for an already-authorized exposure.
    ///
    /// Instance grants are re-read on every call so a reload that narrows them
    /// takes effect without rebuilding an admitted agent.
    pub(super) async fn connection(&self, id: &str) -> Result<Arc<Connection>, String> {
        if !self.granted(id).await {
            return Err("mcp.grant_denied: connection is outside instance authority".into());
        }
        let state = self.state.read().await;
        let Some(connection) = state.connections.get(id).cloned() else {
            if state.restarting.contains(id) {
                return Err("mcp.connection_unavailable: connection is restarting".into());
            }
            if let Some(failure) = state.failures.get(id) {
                return Err(failure.clone());
            }
            return Err(
                "mcp.connection_unavailable: connection is disabled or not configured".into(),
            );
        };
        drop(state);
        if connection.toolset.is_closed().await {
            return Err("mcp.connection_closed: MCP connection is not running".into());
        }
        Ok(connection)
    }

    /// Create a per-exposure toolset view. The view borrows the registry, so a
    /// removed connection cannot keep an unowned child alive.
    pub(crate) fn view(self: &Arc<Self>, id: &str, grants: BTreeSet<String>) -> Arc<dyn Toolset> {
        Arc::new(McpToolsetView::new(
            Arc::downgrade(self),
            id.to_string(),
            grants,
            self.kernel.clone(),
        ))
    }

    async fn grants_snapshot(&self) -> BTreeSet<String> {
        self.grants.read().await.clone()
    }

    async fn granted(&self, id: &str) -> bool {
        let grants = self.grants.read().await;
        grants.iter().any(|grant| grant == "*")
            || grants
                .iter()
                .any(|grant| grant == &super::names::connection_grant(id))
    }
}

/// Convert a connection failure string into a stable typed error.
pub(super) fn health_error(failure: &str) -> ApiError {
    let view = health::failure_view(failure);
    ApiError::new(view.code, view.message)
}
