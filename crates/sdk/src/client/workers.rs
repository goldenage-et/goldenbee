//! Authorized Worker discovery.

use super::transport::{get, get_query};
use super::Client;
use common::{ApiError, WorkerDiscoveryFilter, WorkerSummary, WorkspaceFile, WorkspaceTreePage};

impl Client {
    /// Authorized Worker discovery.
    pub async fn workers(&self) -> Result<Vec<WorkerSummary>, ApiError> {
        get(self, "/v1/workers").await
    }

    /// Authorized Worker discovery filtered by capability, protocol, locality,
    /// and lease-derived health.
    pub async fn discover(
        &self,
        filter: &WorkerDiscoveryFilter,
    ) -> Result<Vec<WorkerSummary>, ApiError> {
        let mut query: Vec<(&str, &str)> = Vec::new();
        if let Some(capability) = &filter.capability {
            query.push(("capability", capability));
        }
        if let Some(protocol) = &filter.protocol {
            query.push(("protocol", protocol));
        }
        if let Some(locality) = &filter.locality {
            query.push(("locality", locality));
        }
        if let Some(health) = &filter.health {
            query.push(("health", health));
        }
        get_query(self, "/v1/workers", &query).await
    }

    /// List one directory level in a visible Worker workspace.
    pub async fn workspace_tree(
        &self,
        worker: &str,
        path: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<WorkspaceTreePage, ApiError> {
        let limit = limit.clamp(1, 500).to_string();
        let mut query = vec![("path", path), ("limit", limit.as_str())];
        if let Some(cursor) = cursor.filter(|value| !value.is_empty()) {
            query.push(("cursor", cursor));
        }
        get_query(
            self,
            &format!("/v1/workers/{worker}/workspace/tree"),
            &query,
        )
        .await
    }

    /// Read one bounded text preview or file metadata from a visible Worker.
    pub async fn workspace_file(
        &self,
        worker: &str,
        path: &str,
    ) -> Result<WorkspaceFile, ApiError> {
        get_query(
            self,
            &format!("/v1/workers/{worker}/workspace/file"),
            &[("path", path)],
        )
        .await
    }
}
