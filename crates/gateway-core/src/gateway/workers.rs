//! Worker discovery, audit, registration, lease renewal, and decommission.

use crate::caller::Caller;
use common::{
    scopes, AdminWorkerSummary, ApiError, RegistryAuditEntry, WorkerDiscoveryFilter, WorkerLease,
    WorkerLeaseRequest, WorkerRegistrationRequest, WorkerSummary,
};

use super::support::normalize;
use super::worker_client::probe;
use super::Gateway;

impl Gateway {
    /// Authorized, filtered Worker discovery.
    pub fn discover(
        &self,
        caller: &Caller,
        filter: &WorkerDiscoveryFilter,
    ) -> Result<Vec<WorkerSummary>, ApiError> {
        caller.principal.require(scopes::TASK_READ)?;
        validate_filter(filter)?;
        self.state.registry.expire(self.state.registry.now());
        let mut workers = self.state.registry.discover(filter);
        workers.retain(|worker| caller.allows(&worker.id));
        Ok(workers)
    }

    /// Retained registry audit decisions. Requires the `admin` scope.
    pub fn audit(
        &self,
        caller: &Caller,
        worker: Option<&str>,
    ) -> Result<Vec<RegistryAuditEntry>, ApiError> {
        caller.principal.require(scopes::ADMIN)?;
        Ok(self.state.registry.audit_entries(worker))
    }

    /// Register an approved Worker with its bound control token.
    pub async fn register_worker(
        &self,
        control_token: Option<&str>,
        request: &WorkerRegistrationRequest,
    ) -> Result<WorkerLease, ApiError> {
        let id = self.control_identity(control_token, "worker.register", "unknown")?;
        let Some(approval) = self.state.registry.approval_record(&id) else {
            return Err(ApiError::new(
                "worker.not_found",
                "Worker is not approved by this Gateway",
            ));
        };
        if approval.decommissioned || !approval.enabled {
            self.state.registry.audit(
                self.state.registry.now(),
                "worker.register",
                &id,
                &format!("worker:{id}"),
                "denied",
                if approval.decommissioned {
                    "Worker identity was decommissioned by an operator"
                } else {
                    "Worker registration is disabled"
                },
            );
            return Err(if approval.decommissioned {
                ApiError::new(
                    "worker.decommissioned",
                    "Worker identity was decommissioned by an operator",
                )
            } else {
                ApiError::new(
                    "worker.disabled",
                    "Worker identity is disabled by an operator",
                )
            });
        }
        if normalize(&request.endpoint) != normalize(&approval.endpoint) {
            self.state.registry.audit(
                self.state.registry.now(),
                "worker.register",
                &id,
                &format!("worker:{id}"),
                "denied",
                "registration endpoint does not match the approved endpoint",
            );
            return Err(ApiError::new(
                "worker.endpoint_forbidden",
                "registration endpoint is not the approved endpoint",
            ));
        }
        let verified = match probe(&self.state.client, &self.state.secret, &approval.endpoint).await
        {
            Ok(verified) => verified,
            Err(error) => {
                self.state.registry.audit(
                    self.state.registry.now(),
                    "worker.register",
                    &id,
                    &format!("worker:{id}"),
                    "denied",
                    "registration endpoint identity could not be verified",
                );
                return Err(error);
            }
        };
        self.state
            .registry
            .register(&id, request, &verified, self.state.registry.now())
    }

    /// Renew an approved Worker lease with its bound control token.
    pub async fn renew_worker(
        &self,
        control_token: Option<&str>,
        id: &str,
        request: &WorkerLeaseRequest,
    ) -> Result<WorkerLease, ApiError> {
        let authenticated = self.control_identity(control_token, "worker.lease_renew", id)?;
        if authenticated != id {
            self.state.registry.audit(
                self.state.registry.now(),
                "worker.lease_renew",
                id,
                &format!("worker:{authenticated}"),
                "denied",
                "control token is bound to another Worker identity",
            );
            return Err(ApiError::new(
                "scope.worker_denied",
                "control token is bound to another Worker identity",
            ));
        }
        let Some(approval) = self.state.registry.approval_record(id) else {
            return Err(ApiError::new(
                "worker.not_found",
                "Worker is not approved by this Gateway",
            ));
        };
        if approval.decommissioned || !approval.enabled {
            return Err(if approval.decommissioned {
                ApiError::new(
                    "worker.decommissioned",
                    "Worker identity was decommissioned by an operator",
                )
            } else {
                ApiError::new(
                    "worker.disabled",
                    "Worker identity is disabled by an operator",
                )
            });
        }
        let verified = match probe(&self.state.client, &self.state.secret, &approval.endpoint).await
        {
            Ok(verified) => verified,
            Err(error) => {
                self.state.registry.audit(
                    self.state.registry.now(),
                    "worker.lease_renew",
                    id,
                    &format!("worker:{id}"),
                    "denied",
                    "renewal endpoint identity could not be verified",
                );
                return Err(error);
            }
        };
        self.state
            .registry
            .renew(id, request, &verified, self.state.registry.now())
    }

    /// Decommission an approved Worker. Requires the `admin` scope.
    pub fn decommission(&self, caller: &Caller, id: &str) -> Result<AdminWorkerSummary, ApiError> {
        self.require_admin(caller, "worker.decommission", id)?;
        self.state
            .registry
            .decommission(id, &caller.principal.id, self.state.registry.now())
            .inspect_err(|_| {
                self.state.registry.audit(
                    self.state.registry.now(),
                    "worker.decommission",
                    id,
                    &caller.principal.id,
                    "denied",
                    "Worker identity is not approved",
                );
            })
    }
}

fn validate_filter(filter: &WorkerDiscoveryFilter) -> Result<(), ApiError> {
    if let Some(locality) = &filter.locality {
        if !matches!(
            locality.as_str(),
            common::LOCALITY_LOCAL | common::LOCALITY_REMOTE
        ) {
            return Err(ApiError::new(
                "worker.filter.invalid",
                "locality must be 'local' or 'remote'",
            ));
        }
    }
    if let Some(health) = &filter.health {
        if !matches!(health.as_str(), "healthy" | "unreachable" | "any") {
            return Err(ApiError::new(
                "worker.filter.invalid",
                "health must be 'healthy', 'unreachable', or 'any'",
            ));
        }
    }
    Ok(())
}
