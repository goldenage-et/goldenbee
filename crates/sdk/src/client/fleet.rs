//! Authorized fleet lifecycle through Gateway host composers.

use super::transport::{get, post};
use super::Client;
use common::{
    ApiError, HostDescriptor, HostInspection, HostOperationOutcome, HostProvisionRequest,
    HostRollbackRequest, HostUpgradeRequest,
};

impl Client {
    /// Configured host composers and their live supported operations.
    pub async fn hosts(&self) -> Result<Vec<HostDescriptor>, ApiError> {
        get(self, "/v1/hosts").await
    }

    /// One configured host composer.
    pub async fn host(&self, host: &str) -> Result<HostDescriptor, ApiError> {
        get(self, &format!("/v1/hosts/{host}")).await
    }

    /// Provision one verified instance and register the resulting Worker.
    pub async fn provision_instance(
        &self,
        host: &str,
        request: &HostProvisionRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        post(self, &format!("/v1/hosts/{host}/instances"), request).await
    }

    /// Inspect digest, identity, lifecycle, health, and compatibility.
    pub async fn inspect_instance(
        &self,
        host: &str,
        instance: &str,
    ) -> Result<HostInspection, ApiError> {
        get(self, &format!("/v1/hosts/{host}/instances/{instance}")).await
    }

    /// Apply a verified managed upgrade.
    pub async fn upgrade_instance(
        &self,
        host: &str,
        instance: &str,
        request: &HostUpgradeRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        post(
            self,
            &format!("/v1/hosts/{host}/instances/{instance}/upgrade"),
            request,
        )
        .await
    }

    /// Recover a compatible interrupted activation.
    pub async fn rollback_instance(
        &self,
        host: &str,
        instance: &str,
        request: &HostRollbackRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        post(
            self,
            &format!("/v1/hosts/{host}/instances/{instance}/rollback"),
            request,
        )
        .await
    }
}
