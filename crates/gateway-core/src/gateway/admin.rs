//! Operator administration of approved Worker identities.

use crate::caller::Caller;
use crate::registry::{ApprovalPatch, ApprovalRecord, NewApproval};
use common::{
    scopes, AddWorkerRequest, AddWorkerResponse, AdminWorkerSummary, ApiError, UpdateWorkerRequest,
};

use super::support::normalize;
use super::worker_client::probe;
use super::Gateway;

impl Gateway {
    /// Admin inventory. Decommissioned identities remain visible.
    pub fn admin_workers(&self, caller: &Caller) -> Result<Vec<AdminWorkerSummary>, ApiError> {
        self.require_admin(caller, "worker.inventory", "*")?;
        self.state.registry.expire(self.state.registry.now());
        Ok(self.state.registry.admin_workers())
    }

    /// Admin view of one approved identity.
    pub fn admin_worker(&self, caller: &Caller, id: &str) -> Result<AdminWorkerSummary, ApiError> {
        self.require_admin(caller, "worker.inspect", id)?;
        self.state.registry.expire(self.state.registry.now());
        self.state
            .registry
            .admin_worker(id)
            .ok_or_else(|| worker_not_found(id))
    }

    /// Add an approved Worker identity. A generated control token is returned
    /// only in the response that created it.
    pub async fn add_worker(
        &self,
        caller: &Caller,
        request: AddWorkerRequest,
    ) -> Result<AddWorkerResponse, ApiError> {
        self.require_admin(caller, "worker.add", &request.id)?;
        if request.id.trim().is_empty() || request.id.len() > 128 {
            return Err(ApiError::new(
                "validation.worker_id",
                "Worker id must be 1-128 characters",
            ));
        }
        if !request.endpoint.starts_with("http://") && !request.endpoint.starts_with("https://") {
            return Err(ApiError::new(
                "validation.endpoint",
                "Worker endpoint must be an HTTP or HTTPS base URL",
            ));
        }
        if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 200 {
            return Err(ApiError::new(
                "validation.idempotency_key",
                "idempotency_key must be 1-200 characters",
            ));
        }
        if request
            .control_token
            .as_deref()
            .is_some_and(|token| token.trim().is_empty())
        {
            return Err(ApiError::new(
                "validation.control_token",
                "control_token must not be empty",
            ));
        }
        let digest = common::canonical_digest(&serde_json::json!({
            "id": request.id,
            "endpoint": request.endpoint,
            "instance": request.instance,
            "package": request.package,
            "capabilities": request.capabilities,
            "control_token": request.control_token,
            "preflight": request.preflight,
            "idempotency_key": request.idempotency_key,
        }));
        let generated = request.control_token.is_none();
        let control_token = request
            .control_token
            .clone()
            .unwrap_or_else(|| format!("gwb_{}", uuid::Uuid::new_v4().simple()));
        let endpoint = normalize(&request.endpoint);
        let (summary, created) = self.state.registry.add(
            NewApproval {
                id: request.id.clone(),
                endpoint: endpoint.clone(),
                instance: request.instance.clone(),
                package: request.package.clone(),
                capabilities: request.capabilities.clone(),
                control_token: Some(control_token.clone()),
                request_digest: digest,
            },
            &caller.principal.id,
            self.state.registry.now(),
        )?;
        if !created {
            return Ok(AddWorkerResponse {
                worker: summary,
                control_token: None,
            });
        }
        if request.preflight {
            let verified = match probe(&self.state.client, &self.state.secret, &endpoint).await {
                Ok(verified) => verified,
                Err(error) => {
                    let _ = self.state.registry.rollback_add(
                        &request.id,
                        &caller.principal.id,
                        self.state.registry.now(),
                    );
                    return Err(error);
                }
            };
            if let Err(error) =
                self.state
                    .registry
                    .bootstrap(&request.id, &verified, self.state.registry.now())
            {
                let _ = self.state.registry.rollback_add(
                    &request.id,
                    &caller.principal.id,
                    self.state.registry.now(),
                );
                return Err(error);
            }
        }
        let summary = self
            .state
            .registry
            .admin_worker(&request.id)
            .ok_or_else(|| worker_not_found(&request.id))?;
        Ok(AddWorkerResponse {
            worker: summary,
            control_token: generated.then_some(control_token),
        })
    }

    /// Update a runtime approval with optimistic concurrency.
    pub async fn update_worker(
        &self,
        caller: &Caller,
        id: &str,
        request: UpdateWorkerRequest,
    ) -> Result<AdminWorkerSummary, ApiError> {
        self.require_admin(caller, "worker.update", id)?;
        let Some(approval) = self.state.registry.approval_record(id) else {
            return Err(worker_not_found(id));
        };
        if request.endpoint.as_deref().is_some_and(|endpoint| {
            !endpoint.starts_with("http://") && !endpoint.starts_with("https://")
        }) {
            return Err(ApiError::new(
                "validation.endpoint",
                "Worker endpoint must be an HTTP or HTTPS base URL",
            ));
        }
        let update_digest = request
            .idempotency_key
            .as_ref()
            .map(|_| common::canonical_digest(&request));
        let expansion = capability_expansion(&approval, request.capabilities.as_ref());
        let probe_endpoint = request
            .endpoint
            .as_deref()
            .map(normalize)
            .unwrap_or_else(|| approval.endpoint.clone());
        let verified = if expansion {
            let verified =
                match probe(&self.state.client, &self.state.secret, &probe_endpoint).await {
                    Ok(verified) => verified,
                    Err(error) => {
                        self.state.registry.audit(
                            self.state.registry.now(),
                            "worker.update",
                            id,
                            &caller.principal.id,
                            "denied",
                            "capability expansion requires a successful probe",
                        );
                        return Err(error);
                    }
                };
            if let Some(instance) = &request.instance {
                if instance != &verified.instance {
                    return Err(ApiError::new(
                        "worker.instance_mismatch",
                        "verified instance identity does not match the requested identity",
                    ));
                }
            }
            if let Some(package) = &request.package {
                if package != &verified.package {
                    return Err(ApiError::new(
                        "worker.package_mismatch",
                        "verified package identity does not match the requested package",
                    ));
                }
            }
            if let Some(capabilities) = &request.capabilities {
                for capability in capabilities {
                    if !verified
                        .capabilities
                        .iter()
                        .any(|advertised| &advertised.id == capability)
                    {
                        return Err(ApiError::new(
                            "worker.capability_mismatch",
                            format!(
                                "approved capability '{capability}' is not advertised by the Worker"
                            ),
                        ));
                    }
                }
            }
            Some(verified)
        } else {
            None
        };
        self.state.registry.update(
            id,
            ApprovalPatch {
                revision: request.revision,
                endpoint: request.endpoint,
                instance: request.instance,
                package: request.package,
                capabilities: request.capabilities,
                request_digest: update_digest,
            },
            &caller.principal.id,
            self.state.registry.now(),
        )?;
        if let Some(verified) = verified {
            self.state
                .registry
                .bootstrap(id, &verified, self.state.registry.now())?;
        }
        self.state
            .registry
            .admin_worker(id)
            .ok_or_else(|| worker_not_found(id))
    }

    /// Probe the live endpoint of an approved Worker and reconcile identity.
    pub async fn probe_worker(
        &self,
        caller: &Caller,
        id: &str,
    ) -> Result<AdminWorkerSummary, ApiError> {
        self.require_admin(caller, "worker.probe", id)?;
        let Some(approval) = self.state.registry.approval_record(id) else {
            return Err(worker_not_found(id));
        };
        if approval.decommissioned {
            return Err(ApiError::new(
                "worker.decommissioned",
                "Worker identity was decommissioned by an operator",
            ));
        }
        if !approval.enabled {
            return Err(ApiError::new(
                "worker.disabled",
                "Worker identity is disabled by an operator",
            ));
        }
        let verified = match probe(&self.state.client, &self.state.secret, &approval.endpoint).await
        {
            Ok(verified) => verified,
            Err(error) => {
                let _ = self.state.registry.mark_unreachable(
                    id,
                    self.state.registry.now(),
                    "explicit probe could not verify the live endpoint",
                );
                self.state.registry.audit(
                    self.state.registry.now(),
                    "worker.probe",
                    id,
                    &caller.principal.id,
                    "denied",
                    "explicit probe could not verify the live endpoint",
                );
                return Err(error);
            }
        };
        if let Err(error) = self
            .state
            .registry
            .bootstrap(id, &verified, self.state.registry.now())
        {
            let _ = self.state.registry.mark_unreachable(
                id,
                self.state.registry.now(),
                "explicit probe identity did not match the approved identity",
            );
            self.state.registry.audit(
                self.state.registry.now(),
                "worker.probe",
                id,
                &caller.principal.id,
                "denied",
                "explicit probe identity did not match the approved identity",
            );
            return Err(error);
        }
        self.state.registry.audit(
            self.state.registry.now(),
            "worker.probe",
            id,
            &caller.principal.id,
            "accepted",
            "explicit probe verified the live endpoint and reconciled registration",
        );
        self.state
            .registry
            .admin_worker(id)
            .ok_or_else(|| worker_not_found(id))
    }

    /// Disable an approved Worker without discarding its task history.
    pub fn disable_worker(
        &self,
        caller: &Caller,
        id: &str,
    ) -> Result<AdminWorkerSummary, ApiError> {
        self.require_admin(caller, "worker.disable", id)?;
        self.state
            .registry
            .disable(id, &caller.principal.id, self.state.registry.now())
    }

    /// Re-enable a disabled Worker identity.
    pub fn enable_worker(&self, caller: &Caller, id: &str) -> Result<AdminWorkerSummary, ApiError> {
        self.require_admin(caller, "worker.enable", id)?;
        self.state
            .registry
            .enable(id, &caller.principal.id, self.state.registry.now())
    }
    pub(super) fn require_admin(
        &self,
        caller: &Caller,
        action: &str,
        worker: &str,
    ) -> Result<(), ApiError> {
        caller.principal.require(scopes::ADMIN).inspect_err(|_| {
            self.state.registry.audit(
                self.state.registry.now(),
                action,
                worker,
                &caller.principal.id,
                "denied",
                "caller lacks the admin scope",
            );
        })
    }

    pub(super) fn control_identity(
        &self,
        token: Option<&str>,
        action: &str,
        worker: &str,
    ) -> Result<String, ApiError> {
        let now = self.state.registry.now();
        let Some(token) = token else {
            self.state.registry.audit(
                now,
                action,
                worker,
                "unknown",
                "denied",
                "Worker control request had no bearer token",
            );
            return Err(ApiError::new(
                "auth.unauthenticated",
                "a Worker control token is required",
            ));
        };
        match self.state.registry.control_identity(token) {
            Some(id) => Ok(id),
            None => {
                self.state.registry.audit(
                    now,
                    action,
                    worker,
                    "unknown",
                    "denied",
                    "Worker control token is not bound to an approved identity",
                );
                Err(ApiError::new(
                    "auth.unauthenticated",
                    "Worker control token is not recognized by this Gateway",
                ))
            }
        }
    }
}

fn worker_not_found(id: &str) -> ApiError {
    ApiError::new(
        "worker.not_found",
        format!("Worker '{id}' is not approved by this Gateway"),
    )
}

fn capability_expansion(approval: &ApprovalRecord, requested: Option<&Vec<String>>) -> bool {
    let Some(requested) = requested else {
        return false;
    };
    match &approval.capabilities {
        Some(existing) => requested
            .iter()
            .any(|capability| !existing.iter().any(|id| id == capability)),
        None => false,
    }
}
