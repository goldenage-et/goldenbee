//! In-process local host lifecycle adapter.
//!
//! A Gateway that owns its own host implements this trait. The control plane
//! keeps registry, approval, lease, and reconciliation policy; the adapter only
//! performs host-local verification, process lifecycle, and inspection.

use async_trait::async_trait;
use common::{
    ApiError, HostDescriptor, HostInspection, HostOperationOutcome, HostProvisionRequest,
    HostRollbackRequest, HostUpgradeRequest,
};

/// Host-local lifecycle capability implemented by the Gateway process itself.
#[async_trait]
pub trait LocalHost: Send + Sync {
    /// Stable local host identity.
    fn id(&self) -> &str;

    /// Live descriptor for compatibility routes and diagnostics.
    fn descriptor(&self) -> HostDescriptor;

    /// Optional instance allow-list. `None` permits any valid instance id.
    fn allowed_instances(&self) -> Option<Vec<String>>;

    /// Optional operation allow-list. `None` permits every advertised one.
    fn allowed_operations(&self) -> Option<Vec<String>>;

    /// Provision one verified instance.
    async fn provision(
        &self,
        request: HostProvisionRequest,
    ) -> Result<HostOperationOutcome, ApiError>;

    /// Inspect one instance.
    async fn inspect(&self, instance: &str) -> Result<HostInspection, ApiError>;

    /// Apply one managed upgrade.
    async fn upgrade(
        &self,
        instance: &str,
        request: HostUpgradeRequest,
    ) -> Result<HostOperationOutcome, ApiError>;

    /// Recover one interrupted activation.
    async fn rollback(
        &self,
        instance: &str,
        request: HostRollbackRequest,
    ) -> Result<HostOperationOutcome, ApiError>;
}
