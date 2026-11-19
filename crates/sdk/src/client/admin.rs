//! Worker approval, inventory, lifecycle, and audit administration.

use super::transport::{delete, get, get_query, patch, post, post_empty};
use super::Client;
use common::{
    AddWorkerRequest, AddWorkerResponse, AdminWorkerSummary, ApiError, RegistryAuditEntry,
    UpdateWorkerRequest,
};

impl Client {
    /// Admin inventory. Disabled and decommissioned identities remain visible.
    pub async fn admin_workers(&self) -> Result<Vec<AdminWorkerSummary>, ApiError> {
        get(self, "/v1/admin/workers").await
    }

    /// Admin view of one approved identity.
    pub async fn admin_worker(&self, id: &str) -> Result<AdminWorkerSummary, ApiError> {
        get(self, &format!("/v1/admin/workers/{id}")).await
    }

    /// Add an approved Worker. A generated control token is returned exactly
    /// once in the response.
    pub async fn add_worker(
        &self,
        request: &AddWorkerRequest,
    ) -> Result<AddWorkerResponse, ApiError> {
        post(self, "/v1/admin/workers", request).await
    }

    /// Update an approval using its observed revision.
    pub async fn update_worker(
        &self,
        id: &str,
        request: &UpdateWorkerRequest,
    ) -> Result<AdminWorkerSummary, ApiError> {
        patch(self, &format!("/v1/admin/workers/{id}"), request).await
    }

    /// Verify the live endpoint and reconcile registration.
    pub async fn probe_worker(&self, id: &str) -> Result<AdminWorkerSummary, ApiError> {
        post_empty(self, &format!("/v1/admin/workers/{id}/probe")).await
    }

    /// Disable registration, renewal, discovery, and new routing.
    pub async fn disable_worker(&self, id: &str) -> Result<AdminWorkerSummary, ApiError> {
        post_empty(self, &format!("/v1/admin/workers/{id}/disable")).await
    }

    /// Enable registration again without restoring the previous lease.
    pub async fn enable_worker(&self, id: &str) -> Result<AdminWorkerSummary, ApiError> {
        post_empty(self, &format!("/v1/admin/workers/{id}/enable")).await
    }

    /// Decommission an identity. Task history is retained.
    pub async fn decommission_worker(&self, id: &str) -> Result<AdminWorkerSummary, ApiError> {
        delete(self, &format!("/v1/admin/workers/{id}")).await
    }

    /// Retained admin audit, optionally filtered to one Worker.
    pub async fn worker_audit(
        &self,
        worker: Option<&str>,
    ) -> Result<Vec<RegistryAuditEntry>, ApiError> {
        let query: Vec<(&str, &str)> = worker
            .into_iter()
            .map(|worker| ("worker", worker))
            .collect();
        get_query(self, "/v1/admin/workers/audit", &query).await
    }
}
