//! Authorized Worker discovery.

use super::transport::{get, get_query};
use super::Client;
use common::{ApiError, WorkerDiscoveryFilter, WorkerSummary};

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
}
