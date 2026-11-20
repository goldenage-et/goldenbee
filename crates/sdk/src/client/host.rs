//! Gateway-owned local package and instance operations.

use super::transport::{get, post, post_empty};
use super::Client;
use common::{
    ApiError, HostInspection, HostInstanceView, HostOperationOutcome, HostProvisionRequest,
    HostRollbackRequest, HostUpgradeRequest, InstanceLogs, PackageInstallOutcome,
    PackageInstallRequest, PackageList,
};

impl Client {
    /// Verified packages in the Gateway-owned cache.
    pub async fn packages(&self) -> Result<PackageList, ApiError> {
        get(self, "/v1/packages").await
    }

    /// Install one package from a folder or URL.
    pub async fn install_package(
        &self,
        request: &PackageInstallRequest,
    ) -> Result<PackageInstallOutcome, ApiError> {
        post(self, "/v1/packages/install", request).await
    }

    /// Local instances owned by this Gateway.
    pub async fn instances(&self) -> Result<Vec<HostInstanceView>, ApiError> {
        get(self, "/v1/instances").await
    }

    /// One local instance and its Worker registration.
    pub async fn local_instance(&self, instance: &str) -> Result<HostInspection, ApiError> {
        get(self, &format!("/v1/instances/{instance}")).await
    }

    /// Provision a local instance and register its Worker identity.
    pub async fn provision_local_instance(
        &self,
        request: &HostProvisionRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        post(self, "/v1/instances", request).await
    }

    /// Start a local instance.
    pub async fn start_instance(&self, instance: &str) -> Result<HostInspection, ApiError> {
        post_empty(self, &format!("/v1/instances/{instance}/start")).await
    }

    /// Stop a local instance.
    pub async fn stop_instance(&self, instance: &str) -> Result<HostInspection, ApiError> {
        post_empty(self, &format!("/v1/instances/{instance}/stop")).await
    }

    /// Restart a local instance and re-register its Worker.
    pub async fn restart_instance(&self, instance: &str) -> Result<HostInspection, ApiError> {
        post_empty(self, &format!("/v1/instances/{instance}/restart")).await
    }

    /// Apply a managed upgrade to a local instance.
    pub async fn upgrade_local_instance(
        &self,
        instance: &str,
        request: &HostUpgradeRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        post(self, &format!("/v1/instances/{instance}/upgrade"), request).await
    }

    /// Recover a compatible interrupted activation on a local instance.
    pub async fn rollback_local_instance(
        &self,
        instance: &str,
        request: &HostRollbackRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        post(self, &format!("/v1/instances/{instance}/rollback"), request).await
    }

    /// Bounded local instance log tail.
    pub async fn instance_logs(
        &self,
        instance: &str,
        tail: usize,
    ) -> Result<InstanceLogs, ApiError> {
        get(
            self,
            &format!("/v1/instances/{instance}/logs?tail={}", tail.clamp(1, 1000)),
        )
        .await
    }
}
