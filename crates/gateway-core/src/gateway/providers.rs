//! Gateway-managed provider catalog, write-only credentials, and Worker
//! enrollment.
//!
//! Admin calls are caller-authenticated and require `admin` or
//! `provider:admin`. Worker calls are authenticated by the Worker's control
//! token and may only address the identity bound to that token.

use crate::caller::Caller;
use crate::registry::ApprovalRecord;
use common::{
    adapter_discovers_models, scopes, ApiError, CreatePairCodeRequest, CreateProviderRequest,
    PairCodeResponse, ProviderBundle, ProviderCredentialSummary, ProviderMetadata,
    SetProviderCredentialRequest, UpdateProviderRequest, WorkerEnrollRequest, WorkerEnrollResponse,
    PROVIDER_ENVELOPE_TTL_MS,
};

use super::discovery;

use super::Gateway;

struct WorkerIdentity {
    worker: String,
    instance: String,
}

impl Gateway {
    /// Non-secret provider catalog for an operator.
    pub fn provider_list(&self, caller: &Caller) -> Result<Vec<ProviderMetadata>, ApiError> {
        self.require_provider_admin(caller, "provider.list")?;
        Ok(self.provider_vault()?.list())
    }

    /// One non-secret provider entry.
    pub fn provider_show(&self, caller: &Caller, id: &str) -> Result<ProviderMetadata, ApiError> {
        self.require_provider_admin(caller, "provider.show")?;
        self.provider_vault()?.provider(id)
    }

    /// Create provider metadata and run discovery when the adapter supports it.
    /// No credential is accepted here.
    pub async fn provider_create(
        &self,
        caller: &Caller,
        request: CreateProviderRequest,
    ) -> Result<ProviderMetadata, ApiError> {
        self.require_provider_admin(caller, "provider.create")?;
        let id = request.id.clone();
        self.provider_vault()?.create(request)?;
        let metadata = self.refresh_provider_discovery(&id).await?;
        self.audit_provider(
            "provider.create",
            &id,
            caller,
            "accepted",
            "provider created",
        );
        Ok(metadata)
    }

    /// Update non-secret provider metadata. An endpoint, adapter, or model
    /// filter change reruns discovery before the provider can be ready.
    pub async fn provider_update(
        &self,
        caller: &Caller,
        id: &str,
        request: UpdateProviderRequest,
    ) -> Result<ProviderMetadata, ApiError> {
        self.require_provider_admin(caller, "provider.update")?;
        self.provider_vault()?.update(id, request)?;
        let metadata = self.refresh_provider_discovery(id).await?;
        self.audit_provider(
            "provider.update",
            id,
            caller,
            "accepted",
            "provider metadata updated",
        );
        Ok(metadata)
    }

    /// Write a new credential version. The request is the only transport for
    /// the value; the result and audit record contain only version and
    /// fingerprint.
    pub async fn provider_set_credential(
        &self,
        caller: &Caller,
        id: &str,
        request: &SetProviderCredentialRequest,
    ) -> Result<ProviderCredentialSummary, ApiError> {
        self.require_provider_admin(caller, "provider.credential.set")?;
        let mut summary = self.provider_vault()?.set_credential(id, request)?;
        let metadata = self.refresh_provider_discovery(id).await?;
        summary.revision = metadata.revision;
        summary.ready = metadata.ready;
        summary.models = metadata.models;
        summary.discovered_at = metadata.discovered_at;
        summary.discovery_error = metadata.discovery_error;
        summary.discovery_error_code = metadata.discovery_error_code;
        self.audit_provider(
            "provider.credential.set",
            id,
            caller,
            "accepted",
            "provider credential rotated",
        );
        Ok(summary)
    }

    /// Revoke the active credential. New envelope fetches omit the provider.
    pub fn provider_revoke_credential(
        &self,
        caller: &Caller,
        id: &str,
    ) -> Result<ProviderCredentialSummary, ApiError> {
        self.require_provider_admin(caller, "provider.credential.revoke")?;
        let summary = self.provider_vault()?.revoke_credential(id)?;
        self.audit_provider(
            "provider.credential.revoke",
            id,
            caller,
            "accepted",
            "provider credential revoked",
        );
        Ok(summary)
    }

    /// Mint a one-time pairing code for an approved Worker identity.
    pub fn provider_create_pair_code(
        &self,
        caller: &Caller,
        request: CreatePairCodeRequest,
    ) -> Result<PairCodeResponse, ApiError> {
        self.require_provider_admin(caller, "provider.pair")?;
        let approval = self
            .state
            .registry
            .approval_record(&request.worker_id)
            .ok_or_else(|| {
                ApiError::new(
                    "worker.not_found",
                    format!(
                        "Worker '{}' is not approved by this Gateway",
                        request.worker_id
                    ),
                )
            })?;
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
        if let Some(instance) = &approval.instance {
            if instance != &request.instance {
                return Err(ApiError::new(
                    "provider.instance_mismatch",
                    "pair code instance does not match the approved Worker identity",
                ));
            }
        }
        let worker = request.worker_id.clone();
        let response = self.provider_vault()?.create_pair_code(request)?;
        self.audit_provider(
            "provider.pair",
            &worker,
            caller,
            "accepted",
            "one-time Worker pairing code issued",
        );
        Ok(response)
    }

    /// Redeem a one-time code and bind the Worker's X25519 public key.
    pub fn provider_enroll_worker(
        &self,
        token: Option<&str>,
        request: &WorkerEnrollRequest,
    ) -> Result<WorkerEnrollResponse, ApiError> {
        let identity = self.worker_identity(token, "provider.enroll")?;
        if identity.worker != request.worker_id {
            self.audit_provider(
                "provider.enroll",
                &identity.worker,
                &Caller::new(common::Principal::new(
                    "worker-control",
                    "local",
                    &["worker:control"],
                )),
                "denied",
                "enrollment named another Worker identity",
            );
            return Err(ApiError::new(
                "provider.worker_mismatch",
                "enrollment Worker id does not match the control token binding",
            ));
        }
        if identity.instance != request.instance {
            self.audit_provider(
                "provider.enroll",
                &identity.worker,
                &Caller::new(common::Principal::new(
                    "worker-control",
                    "local",
                    &["worker:control"],
                )),
                "denied",
                "enrollment named another Worker instance",
            );
            return Err(ApiError::new(
                "provider.instance_mismatch",
                "enrollment instance does not match the approved Worker identity",
            ));
        }
        let response = self.provider_vault()?.enroll(request)?;
        self.audit_provider(
            "provider.enroll",
            &identity.worker,
            &Caller::new(common::Principal::new(
                "worker-control",
                "local",
                &["worker:control"],
            )),
            "accepted",
            "Worker provider encryption key enrolled",
        );
        Ok(response)
    }

    /// Issue short-lived envelopes for the calling Worker's assigned providers.
    pub fn provider_bundle(&self, token: Option<&str>) -> Result<ProviderBundle, ApiError> {
        let identity = self.worker_identity(token, "provider.bundle")?;
        self.provider_vault()?.bundle(
            &identity.worker,
            &identity.instance,
            PROVIDER_ENVELOPE_TTL_MS,
        )
    }

    /// Manually refresh discovered models. Repeated calls converge on the
    /// current provider response and advance the metadata revision.
    pub async fn provider_discover(
        &self,
        caller: &Caller,
        id: &str,
    ) -> Result<ProviderMetadata, ApiError> {
        self.require_provider_admin(caller, "provider.discover")?;
        self.provider_vault()?.provider(id)?;
        let metadata = self.refresh_provider_discovery(id).await?;
        let (outcome, detail) = if metadata.ready {
            ("accepted", "provider discovery completed")
        } else {
            (
                "denied",
                "provider discovery did not produce a ready provider",
            )
        };
        self.audit_provider("provider.discover", id, caller, outcome, detail);
        Ok(metadata)
    }

    async fn refresh_provider_discovery(&self, id: &str) -> Result<ProviderMetadata, ApiError> {
        let vault = self.provider_vault()?;
        let metadata = vault.provider(id)?;
        if !adapter_discovers_models(&metadata.adapter) {
            return Ok(metadata);
        }
        let plaintext = match vault.credential_plaintext(id) {
            Ok(plaintext) => plaintext,
            Err(error) => {
                return vault.record_discovery(id, Vec::new(), Some((error.code, error.message)));
            }
        };
        let credential = match plaintext.as_ref() {
            Some(plaintext) => match std::str::from_utf8(plaintext.as_slice()) {
                Ok(value) => Some(value),
                Err(_) => {
                    return vault.record_discovery(
                        id,
                        Vec::new(),
                        Some((
                            "provider.credential_invalid".into(),
                            "stored provider credential is not valid UTF-8".into(),
                        )),
                    );
                }
            },
            None => None,
        };
        match discovery::discover(
            &metadata.adapter,
            metadata.endpoint.as_deref(),
            credential,
            metadata.allowed_models.as_deref(),
        )
        .await
        {
            Ok(models) => vault.record_discovery(id, models, None),
            Err(error) => vault.record_discovery(id, Vec::new(), Some((error.code, error.message))),
        }
    }

    fn provider_vault(&self) -> Result<&crate::vault::Vault, ApiError> {
        self.state.vault.as_deref().ok_or_else(|| {
            ApiError::new(
                "provider.vault_disabled",
                "Gateway provider vault is not enabled in this deployment",
            )
        })
    }

    fn require_provider_admin(&self, caller: &Caller, action: &str) -> Result<(), ApiError> {
        caller
            .principal
            .require_any(&[scopes::ADMIN, scopes::PROVIDER_ADMIN])
            .inspect_err(|_| {
                self.state.registry.audit(
                    self.state.registry.now(),
                    action,
                    "*",
                    &caller.principal.id,
                    "denied",
                    "caller lacks the admin or provider:admin scope",
                );
            })
    }

    fn audit_provider(
        &self,
        action: &str,
        target: &str,
        caller: &Caller,
        outcome: &str,
        detail: &str,
    ) {
        self.state.registry.audit(
            self.state.registry.now(),
            action,
            target,
            &caller.principal.id,
            outcome,
            detail,
        );
    }

    fn worker_identity(
        &self,
        token: Option<&str>,
        action: &str,
    ) -> Result<WorkerIdentity, ApiError> {
        let worker = self.control_identity(token, action, "*")?;
        let approval: ApprovalRecord =
            self.state
                .registry
                .approval_record(&worker)
                .ok_or_else(|| {
                    ApiError::new(
                        "provider.worker_unapproved",
                        "Worker control token is not bound to an approved identity",
                    )
                })?;
        if approval.decommissioned {
            return Err(ApiError::new(
                "provider.worker_decommissioned",
                "Worker identity was decommissioned by an operator",
            ));
        }
        if !approval.enabled {
            return Err(ApiError::new(
                "provider.worker_disabled",
                "Worker identity is disabled by an operator",
            ));
        }
        let registered = self.state.registry.worker(&worker);
        let instance = registered
            .as_ref()
            .map(|summary| summary.instance.clone())
            .or(approval.instance.clone())
            .ok_or_else(|| {
                ApiError::new(
                    "provider.worker_unregistered",
                    "Worker has no verified or configured instance identity",
                )
            })?;
        Ok(WorkerIdentity { worker, instance })
    }
}
