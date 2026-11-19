//! Instance-owned registry and lifecycle for local MCP child processes.

use super::config::McpServerConfig;
use super::local::{connect, Connection};
use super::toolset::McpToolsetView;
use adk_rust::Toolset;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Default)]
struct RegistryState {
    configured: BTreeSet<String>,
    connections: BTreeMap<String, Arc<Connection>>,
    failures: BTreeMap<String, String>,
}

/// One process's MCP connections. Instances never share this registry, so two
/// Workers in one package get separate children, environments, and tools.
pub struct McpRegistry {
    grants: RwLock<BTreeSet<String>>,
    state: RwLock<RegistryState>,
}

impl McpRegistry {
    /// Create an empty registry bound to the instance grants in effect.
    pub fn new(grants: &[String]) -> Self {
        Self {
            grants: RwLock::new(grants.iter().cloned().collect()),
            state: RwLock::new(RegistryState::default()),
        }
    }

    /// Configured server ids, including disabled definitions.
    pub async fn configured_ids(&self) -> BTreeSet<String> {
        self.state.read().await.configured.clone()
    }

    /// Start enabled connections and stop connections removed by the new
    /// configuration. Connection failures are recorded and fail closed for the
    /// affected connection only.
    pub async fn reconcile(&self, configs: &BTreeMap<String, McpServerConfig>, grants: &[String]) {
        *self.grants.write().await = grants.iter().cloned().collect();
        let current = self.state.read().await.connections.clone();
        let mut next = BTreeMap::new();
        let mut failures = BTreeMap::new();
        for (id, config) in configs {
            if !config.enabled {
                continue;
            }
            let reuse = current
                .get(id)
                .filter(|connection| connection.config.signature() == config.signature())
                .cloned();
            if let Some(connection) = reuse {
                next.insert(id.clone(), connection);
                continue;
            }
            match connect(id, config).await {
                Ok(connection) => {
                    next.insert(id.clone(), Arc::new(connection));
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
        state.configured = configs.keys().cloned().collect();
        state.connections = next;
        state.failures = failures;
        drop(state);
        for connection in removed {
            connection.close().await;
        }
    }

    /// Stop every child owned by this instance.
    pub async fn shutdown(&self) {
        let connections = {
            let mut state = self.state.write().await;
            let connections = std::mem::take(&mut state.connections);
            state.failures.clear();
            connections
        };
        for connection in connections.into_values() {
            connection.close().await;
        }
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
        ))
    }

    async fn granted(&self, id: &str) -> bool {
        let grants = self.grants.read().await;
        grants.iter().any(|grant| grant == "*")
            || grants.iter().any(|grant| grant == &format!("mcp:{id}"))
    }
}
