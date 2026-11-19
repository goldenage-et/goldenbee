//! Approval, lease, and audit store interfaces plus the selected adapter.
//!
//! Routing and authorization policy live in `crate::gateway`. These traits own
//! durable state only. The file adapter is the existing single-writer registry;
//! the Postgres adapter reads and writes one transaction per call and keeps no
//! authoritative in-process copy.

use common::{
    AdminWorkerSummary, ApiError, RegistryAuditEntry, Team, TeamSpec, WorkerDiscoveryFilter,
    WorkerLease, WorkerLeaseRequest, WorkerRegistrationRequest, WorkerSummary,
};

use crate::registry::{Approval, ApprovalPatch, ApprovalRecord, NewApproval, Registry, Verified};

#[cfg(feature = "postgres")]
use std::sync::Arc;

#[cfg(feature = "postgres")]
use super::postgres::PostgresBackend;

/// Durable approvals and the control-token identity index.
pub trait ApprovalStore: Send + Sync {
    /// Current Gateway-side time.
    fn now(&self) -> i64;
    /// Resolve a control token to its approved identity.
    fn control_identity(&self, token: &str) -> Option<String>;
    /// Clone one approval.
    fn approval(&self, id: &str) -> Option<ApprovalRecord>;
    /// Seed config-owned approvals without overwriting runtime decisions.
    fn seed_config(&self, approvals: Vec<Approval>, now: i64) -> Result<(), ApiError>;
    /// Add a runtime approval.
    fn add(
        &self,
        input: NewApproval,
        principal: &str,
        now: i64,
    ) -> Result<(AdminWorkerSummary, bool), ApiError>;
    /// Update with an expected revision.
    fn update(
        &self,
        id: &str,
        patch: ApprovalPatch,
        principal: &str,
        now: i64,
    ) -> Result<AdminWorkerSummary, ApiError>;
    /// Remove a runtime approval that failed its add preflight.
    fn rollback_add(&self, id: &str, principal: &str, now: i64) -> Result<(), ApiError>;
    /// Disable registration and new routing.
    fn disable(&self, id: &str, principal: &str, now: i64) -> Result<AdminWorkerSummary, ApiError>;
    /// Enable registration again.
    fn enable(&self, id: &str, principal: &str, now: i64) -> Result<AdminWorkerSummary, ApiError>;
    /// Terminal decommission.
    fn decommission(
        &self,
        id: &str,
        principal: &str,
        now: i64,
    ) -> Result<AdminWorkerSummary, ApiError>;
    /// Admin inventory.
    fn admin_workers(&self) -> Vec<AdminWorkerSummary>;
    /// One admin view.
    fn admin_worker(&self, id: &str) -> Option<AdminWorkerSummary>;
}

/// Durable Worker records and leases.
pub trait LeaseStore: Send + Sync {
    /// Record a verified registration.
    fn register(
        &self,
        id: &str,
        request: &WorkerRegistrationRequest,
        verified: &Verified,
        now: i64,
    ) -> Result<WorkerLease, ApiError>;
    /// Renew a lease from any replica.
    fn renew(
        &self,
        id: &str,
        request: &WorkerLeaseRequest,
        verified: &Verified,
        now: i64,
    ) -> Result<WorkerLease, ApiError>;
    /// Install startup preflight state.
    fn bootstrap(&self, id: &str, verified: &Verified, now: i64) -> Result<WorkerLease, ApiError>;
    /// Record a failed endpoint verification.
    fn mark_unreachable(&self, id: &str, now: i64, detail: &str) -> Result<(), ApiError>;
    /// Expire stale leases. Returns rows actually transitioned.
    fn expire(&self, now: i64) -> usize;
    /// Current Worker state.
    fn worker(&self, id: &str) -> Option<WorkerSummary>;
    /// Discovery matching the store's filters. Caller allow-list policy is
    /// applied by the Gateway, not the store.
    fn discover(&self, filter: &WorkerDiscoveryFilter) -> Vec<WorkerSummary>;
    /// Approved and reachable counts.
    fn health(&self) -> (u32, u32);
}

/// Durable versioned team rosters.
pub trait TeamStore: Send + Sync {
    /// Create an inactive version-1 roster.
    fn create_team(
        &self,
        namespace: &str,
        name: &str,
        spec: TeamSpec,
        now: i64,
    ) -> Result<Team, ApiError>;
    /// Append a version when the expected version matches.
    fn update_team(
        &self,
        namespace: &str,
        name: &str,
        expected_version: u64,
        spec: TeamSpec,
        now: i64,
    ) -> Result<Team, ApiError>;
    /// Revalidate without changing the roster.
    fn validate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        now: i64,
    ) -> Result<Team, ApiError>;
    /// Activate after revalidation.
    fn activate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        now: i64,
    ) -> Result<Team, ApiError>;
    /// Deactivate future admission.
    fn deactivate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        now: i64,
    ) -> Result<Team, ApiError>;
    /// One roster.
    fn team(&self, namespace: &str, name: &str) -> Option<Team>;
    /// Rosters in a namespace.
    fn teams(&self, namespace: &str) -> Vec<Team>;
}

/// Append-only audit decisions.
pub trait AuditStore: Send + Sync {
    /// Audit a rejected control or operator decision.
    #[allow(clippy::too_many_arguments)]
    fn audit(
        &self,
        now: i64,
        action: &str,
        worker: &str,
        principal: &str,
        outcome: &str,
        detail: &str,
    );
    /// Retained audit entries, newest last.
    fn audit_entries(&self, worker: Option<&str>) -> Vec<RegistryAuditEntry>;
}

impl ApprovalStore for Registry {
    fn now(&self) -> i64 {
        Registry::now(self)
    }

    fn control_identity(&self, token: &str) -> Option<String> {
        self.control_worker(token)
    }

    fn approval(&self, id: &str) -> Option<ApprovalRecord> {
        self.approval_record(id)
    }

    fn seed_config(&self, approvals: Vec<Approval>, now: i64) -> Result<(), ApiError> {
        Registry::seed_config(self, approvals, now)
    }

    fn add(
        &self,
        input: NewApproval,
        principal: &str,
        now: i64,
    ) -> Result<(AdminWorkerSummary, bool), ApiError> {
        Registry::add(self, input, principal, now)
    }

    fn update(
        &self,
        id: &str,
        patch: ApprovalPatch,
        principal: &str,
        now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        Registry::update(self, id, patch, principal, now)
    }

    fn rollback_add(&self, id: &str, principal: &str, now: i64) -> Result<(), ApiError> {
        Registry::rollback_add(self, id, principal, now)
    }

    fn disable(&self, id: &str, principal: &str, now: i64) -> Result<AdminWorkerSummary, ApiError> {
        Registry::disable(self, id, principal, now)
    }

    fn enable(&self, id: &str, principal: &str, now: i64) -> Result<AdminWorkerSummary, ApiError> {
        Registry::enable(self, id, principal, now)
    }

    fn decommission(
        &self,
        id: &str,
        principal: &str,
        now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        Registry::decommission(self, id, principal, now)
    }

    fn admin_workers(&self) -> Vec<AdminWorkerSummary> {
        Registry::admin_workers(self)
    }

    fn admin_worker(&self, id: &str) -> Option<AdminWorkerSummary> {
        Registry::admin_worker(self, id)
    }
}

impl LeaseStore for Registry {
    fn register(
        &self,
        id: &str,
        request: &WorkerRegistrationRequest,
        verified: &Verified,
        now: i64,
    ) -> Result<WorkerLease, ApiError> {
        Registry::register(self, id, request, verified, now)
    }

    fn renew(
        &self,
        id: &str,
        request: &WorkerLeaseRequest,
        verified: &Verified,
        now: i64,
    ) -> Result<WorkerLease, ApiError> {
        Registry::renew(self, id, request, verified, now)
    }

    fn bootstrap(&self, id: &str, verified: &Verified, now: i64) -> Result<WorkerLease, ApiError> {
        Registry::bootstrap(self, id, verified, now)
    }

    fn mark_unreachable(&self, id: &str, now: i64, detail: &str) -> Result<(), ApiError> {
        Registry::mark_unreachable(self, id, now, detail)
    }

    fn expire(&self, now: i64) -> usize {
        Registry::expire(self, now)
    }

    fn worker(&self, id: &str) -> Option<WorkerSummary> {
        Registry::worker(self, id)
    }

    fn discover(&self, filter: &WorkerDiscoveryFilter) -> Vec<WorkerSummary> {
        Registry::discover(self, |_| true, filter)
    }

    fn health(&self) -> (u32, u32) {
        Registry::health(self)
    }
}

impl TeamStore for Registry {
    fn create_team(
        &self,
        namespace: &str,
        name: &str,
        spec: TeamSpec,
        now: i64,
    ) -> Result<Team, ApiError> {
        Registry::create_team(self, namespace, name, spec, now)
    }

    fn update_team(
        &self,
        namespace: &str,
        name: &str,
        expected_version: u64,
        spec: TeamSpec,
        now: i64,
    ) -> Result<Team, ApiError> {
        Registry::update_team(self, namespace, name, expected_version, spec, now)
    }

    fn validate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        now: i64,
    ) -> Result<Team, ApiError> {
        Registry::validate_team(self, namespace, name, version, now)
    }

    fn activate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        now: i64,
    ) -> Result<Team, ApiError> {
        Registry::activate_team(self, namespace, name, version, now)
    }

    fn deactivate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        now: i64,
    ) -> Result<Team, ApiError> {
        Registry::deactivate_team(self, namespace, name, version, now)
    }

    fn team(&self, namespace: &str, name: &str) -> Option<Team> {
        Registry::team(self, namespace, name)
    }

    fn teams(&self, namespace: &str) -> Vec<Team> {
        Registry::teams(self, namespace)
    }
}

impl AuditStore for Registry {
    fn audit(
        &self,
        now: i64,
        action: &str,
        worker: &str,
        principal: &str,
        outcome: &str,
        detail: &str,
    ) {
        Registry::audit(self, now, action, worker, principal, outcome, detail);
    }

    fn audit_entries(&self, worker: Option<&str>) -> Vec<RegistryAuditEntry> {
        Registry::audit_entries(self, worker)
    }
}

/// Selected control-plane storage adapter.
pub enum Control {
    /// Standalone single-writer JSON registry.
    File(Registry),
    /// Transaction-shaped shared registry.
    #[cfg(feature = "postgres")]
    Shared(Arc<PostgresBackend>),
}

impl Control {
    /// Current Gateway-side time.
    pub fn now(&self) -> i64 {
        match self {
            Self::File(registry) => ApprovalStore::now(registry),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => ApprovalStore::now(store.as_ref()),
        }
    }

    /// Resolve a control token.
    pub fn control_identity(&self, token: &str) -> Option<String> {
        match self {
            Self::File(registry) => registry.control_identity(token),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => ApprovalStore::control_identity(store.as_ref(), token),
        }
    }

    /// Clone one approval.
    pub fn approval_record(&self, id: &str) -> Option<ApprovalRecord> {
        match self {
            Self::File(registry) => registry.approval(id),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => ApprovalStore::approval(store.as_ref(), id),
        }
    }

    /// Seed config-owned approvals.
    pub fn seed_config(&self, approvals: Vec<Approval>, now: i64) -> Result<(), ApiError> {
        match self {
            Self::File(registry) => ApprovalStore::seed_config(registry, approvals, now),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => ApprovalStore::seed_config(store.as_ref(), approvals, now),
        }
    }

    /// Add a runtime approval.
    pub fn add(
        &self,
        input: NewApproval,
        principal: &str,
        now: i64,
    ) -> Result<(AdminWorkerSummary, bool), ApiError> {
        match self {
            Self::File(registry) => ApprovalStore::add(registry, input, principal, now),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => ApprovalStore::add(store.as_ref(), input, principal, now),
        }
    }

    /// Update an approval with an expected revision.
    pub fn update(
        &self,
        id: &str,
        patch: ApprovalPatch,
        principal: &str,
        now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        match self {
            Self::File(registry) => ApprovalStore::update(registry, id, patch, principal, now),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => ApprovalStore::update(store.as_ref(), id, patch, principal, now),
        }
    }

    /// Roll back a failed runtime add.
    pub fn rollback_add(&self, id: &str, principal: &str, now: i64) -> Result<(), ApiError> {
        match self {
            Self::File(registry) => ApprovalStore::rollback_add(registry, id, principal, now),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => ApprovalStore::rollback_add(store.as_ref(), id, principal, now),
        }
    }

    /// Disable an identity.
    pub fn disable(
        &self,
        id: &str,
        principal: &str,
        now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        match self {
            Self::File(registry) => ApprovalStore::disable(registry, id, principal, now),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => ApprovalStore::disable(store.as_ref(), id, principal, now),
        }
    }

    /// Enable an identity.
    pub fn enable(
        &self,
        id: &str,
        principal: &str,
        now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        match self {
            Self::File(registry) => ApprovalStore::enable(registry, id, principal, now),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => ApprovalStore::enable(store.as_ref(), id, principal, now),
        }
    }

    /// Decommission an identity.
    pub fn decommission(
        &self,
        id: &str,
        principal: &str,
        now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        match self {
            Self::File(registry) => ApprovalStore::decommission(registry, id, principal, now),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => ApprovalStore::decommission(store.as_ref(), id, principal, now),
        }
    }

    /// Admin inventory.
    pub fn admin_workers(&self) -> Vec<AdminWorkerSummary> {
        match self {
            Self::File(registry) => ApprovalStore::admin_workers(registry),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => ApprovalStore::admin_workers(store.as_ref()),
        }
    }

    /// One admin view.
    pub fn admin_worker(&self, id: &str) -> Option<AdminWorkerSummary> {
        match self {
            Self::File(registry) => ApprovalStore::admin_worker(registry, id),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => ApprovalStore::admin_worker(store.as_ref(), id),
        }
    }

    /// Register a verified Worker.
    pub fn register(
        &self,
        id: &str,
        request: &WorkerRegistrationRequest,
        verified: &Verified,
        now: i64,
    ) -> Result<WorkerLease, ApiError> {
        match self {
            Self::File(registry) => LeaseStore::register(registry, id, request, verified, now),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => LeaseStore::register(store.as_ref(), id, request, verified, now),
        }
    }

    /// Renew a Worker lease.
    pub fn renew(
        &self,
        id: &str,
        request: &WorkerLeaseRequest,
        verified: &Verified,
        now: i64,
    ) -> Result<WorkerLease, ApiError> {
        match self {
            Self::File(registry) => LeaseStore::renew(registry, id, request, verified, now),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => LeaseStore::renew(store.as_ref(), id, request, verified, now),
        }
    }

    /// Install preflight state.
    pub fn bootstrap(
        &self,
        id: &str,
        verified: &Verified,
        now: i64,
    ) -> Result<WorkerLease, ApiError> {
        match self {
            Self::File(registry) => LeaseStore::bootstrap(registry, id, verified, now),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => LeaseStore::bootstrap(store.as_ref(), id, verified, now),
        }
    }

    /// Record a failed verification.
    pub fn mark_unreachable(&self, id: &str, now: i64, detail: &str) -> Result<(), ApiError> {
        match self {
            Self::File(registry) => LeaseStore::mark_unreachable(registry, id, now, detail),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => LeaseStore::mark_unreachable(store.as_ref(), id, now, detail),
        }
    }

    /// Expire stale leases.
    pub fn expire(&self, now: i64) -> usize {
        match self {
            Self::File(registry) => LeaseStore::expire(registry, now),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => LeaseStore::expire(store.as_ref(), now),
        }
    }

    /// Current Worker state.
    pub fn worker(&self, id: &str) -> Option<WorkerSummary> {
        match self {
            Self::File(registry) => LeaseStore::worker(registry, id),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => LeaseStore::worker(store.as_ref(), id),
        }
    }

    /// Discovery matching the store's filters.
    pub fn discover(&self, filter: &WorkerDiscoveryFilter) -> Vec<WorkerSummary> {
        match self {
            Self::File(registry) => LeaseStore::discover(registry, filter),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => LeaseStore::discover(store.as_ref(), filter),
        }
    }

    /// Approved and reachable counts.
    pub fn health(&self) -> (u32, u32) {
        match self {
            Self::File(registry) => LeaseStore::health(registry),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => LeaseStore::health(store.as_ref()),
        }
    }

    /// Create an inactive team roster.
    pub fn create_team(
        &self,
        namespace: &str,
        name: &str,
        spec: TeamSpec,
        now: i64,
    ) -> Result<Team, ApiError> {
        match self {
            Self::File(registry) => TeamStore::create_team(registry, namespace, name, spec, now),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => {
                TeamStore::create_team(store.as_ref(), namespace, name, spec, now)
            }
        }
    }

    /// Append a team version with optimistic concurrency.
    pub fn update_team(
        &self,
        namespace: &str,
        name: &str,
        expected_version: u64,
        spec: TeamSpec,
        now: i64,
    ) -> Result<Team, ApiError> {
        match self {
            Self::File(registry) => {
                TeamStore::update_team(registry, namespace, name, expected_version, spec, now)
            }
            #[cfg(feature = "postgres")]
            Self::Shared(store) => {
                TeamStore::update_team(store.as_ref(), namespace, name, expected_version, spec, now)
            }
        }
    }

    /// Revalidate a team roster.
    pub fn validate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        now: i64,
    ) -> Result<Team, ApiError> {
        match self {
            Self::File(registry) => {
                TeamStore::validate_team(registry, namespace, name, version, now)
            }
            #[cfg(feature = "postgres")]
            Self::Shared(store) => {
                TeamStore::validate_team(store.as_ref(), namespace, name, version, now)
            }
        }
    }

    /// Activate a validated team roster.
    pub fn activate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        now: i64,
    ) -> Result<Team, ApiError> {
        match self {
            Self::File(registry) => {
                TeamStore::activate_team(registry, namespace, name, version, now)
            }
            #[cfg(feature = "postgres")]
            Self::Shared(store) => {
                TeamStore::activate_team(store.as_ref(), namespace, name, version, now)
            }
        }
    }

    /// Deactivate future team admission.
    pub fn deactivate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        now: i64,
    ) -> Result<Team, ApiError> {
        match self {
            Self::File(registry) => {
                TeamStore::deactivate_team(registry, namespace, name, version, now)
            }
            #[cfg(feature = "postgres")]
            Self::Shared(store) => {
                TeamStore::deactivate_team(store.as_ref(), namespace, name, version, now)
            }
        }
    }

    /// One team roster.
    pub fn team(&self, namespace: &str, name: &str) -> Option<Team> {
        match self {
            Self::File(registry) => TeamStore::team(registry, namespace, name),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => TeamStore::team(store.as_ref(), namespace, name),
        }
    }

    /// Team rosters in a namespace.
    pub fn teams(&self, namespace: &str) -> Vec<Team> {
        match self {
            Self::File(registry) => TeamStore::teams(registry, namespace),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => TeamStore::teams(store.as_ref(), namespace),
        }
    }

    /// Audit a denied decision.
    #[allow(clippy::too_many_arguments)]
    pub fn audit(
        &self,
        now: i64,
        action: &str,
        worker: &str,
        principal: &str,
        outcome: &str,
        detail: &str,
    ) {
        match self {
            Self::File(registry) => {
                AuditStore::audit(registry, now, action, worker, principal, outcome, detail)
            }
            #[cfg(feature = "postgres")]
            Self::Shared(store) => AuditStore::audit(
                store.as_ref(),
                now,
                action,
                worker,
                principal,
                outcome,
                detail,
            ),
        }
    }

    /// Retained audit entries.
    pub fn audit_entries(&self, worker: Option<&str>) -> Vec<RegistryAuditEntry> {
        match self {
            Self::File(registry) => AuditStore::audit_entries(registry, worker),
            #[cfg(feature = "postgres")]
            Self::Shared(store) => AuditStore::audit_entries(store.as_ref(), worker),
        }
    }
}
