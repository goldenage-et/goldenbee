//! Authorized fleet lifecycle orchestration through host deployment adapters.
//!
//! The Gateway validates the caller scope, the configured host, and the
//! instance allow-list, then forwards the operation to exactly one host
//! adapter with that host's scoped credential. Lease, registration, and
//! inspection projections remain Gateway-owned. The Gateway never resolves a
//! host path, reads Worker assets, or performs privileged installation.

use crate::caller::Caller;
use crate::config::HostConfig;
use crate::lifecycle::Record;
use crate::registry::NewApproval;
use common::{
    scopes, AdminWorkerSummary, ApiError, Capacity, HostDescriptor, HostInspection,
    HostOperationOutcome, HostProvisionRequest, HostRollbackRequest, HostUpgradeRequest,
    WorkerRegistrationRequest, HOST_OPERATION_INSPECT, HOST_OPERATION_PROVISION,
    HOST_OPERATION_ROLLBACK, HOST_OPERATION_UPGRADE,
};

use super::support::normalize;
use super::worker_client::probe;
use super::Gateway;

impl Gateway {
    /// Configured host composers with their live supported operations.
    pub async fn hosts(&self, caller: &Caller) -> Result<Vec<HostDescriptor>, ApiError> {
        caller.principal.require(scopes::INSTANCE_ADMIN)?;
        let mut hosts = Vec::new();
        for host in &self.state.hosts {
            let descriptor = match self.host_descriptor(host).await {
                Ok(descriptor) => descriptor,
                Err(error) => HostDescriptor {
                    host: host.id.clone(),
                    protocol: common::HOST_PROTOCOL_VERSION.into(),
                    adapter_version: String::new(),
                    supported: host.operations.clone().unwrap_or_default(),
                    reachable: false,
                    error: Some(format!("{}: {}", error.code, error.message)),
                },
            };
            hosts.push(descriptor);
        }
        Ok(hosts)
    }

    /// One configured host composer descriptor.
    pub async fn host(&self, caller: &Caller, host: &str) -> Result<HostDescriptor, ApiError> {
        caller.principal.require(scopes::INSTANCE_ADMIN)?;
        let host = self.host_config(host)?;
        self.host_descriptor(&host).await
    }

    /// Provision one verified instance, activate its profile, then register the
    /// resulting Worker through the existing identity checks.
    pub async fn provision_instance(
        &self,
        caller: &Caller,
        host: &str,
        request: HostProvisionRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        self.require_instance_admin(caller, "host.provision", &request.instance)?;
        let host =
            self.authorized_host(caller, host, &request.instance, HOST_OPERATION_PROVISION)?;
        validate_operation(&request.operation_id, &request.worker, &request.instance)?;
        let digest = request.digest();
        if let Some(record) =
            self.lifecycle_replay(&request.operation_id, &digest, &request.instance)?
        {
            return Ok(record);
        }
        self.require_supported(&host, HOST_OPERATION_PROVISION)
            .await?;

        let token = request
            .control_token
            .clone()
            .filter(|token| !token.trim().is_empty())
            .unwrap_or_else(|| self.worker_token(&request.operation_id));
        let mut adapter_request = request.clone();
        adapter_request.control_token = Some(token.clone());
        self.record_in_flight(
            &host,
            &request.operation_id,
            HOST_OPERATION_PROVISION,
            &digest,
            &request.instance,
            &request.worker,
        )?;

        let outcome: HostOperationOutcome = self
            .adapter_post(&host, "/v1/instances", &adapter_request)
            .await
            .map_err(|error| {
                self.operation_failed(
                    &host,
                    &request.operation_id,
                    &digest,
                    &request.instance,
                    &request.worker,
                    error,
                )
            })?;
        let view = outcome.view.clone().ok_or_else(|| {
            ApiError::new(
                "host.invalid_response",
                "the host composer did not return an instance inspection",
            )
        })?;
        let endpoint = view.endpoint.clone().ok_or_else(|| {
            ApiError::new(
                "host.identity_unverified",
                "the provisioned instance did not record a Worker endpoint",
            )
        })?;

        let approval = NewApproval {
            id: request.worker.clone(),
            endpoint: normalize(&endpoint),
            instance: Some(request.instance.clone()),
            package: None,
            capabilities: None,
            control_token: Some(token),
            request_digest: common::canonical_digest(&serde_json::json!({
                "kind": "host.provision",
                "worker": request.worker,
                "instance": request.instance,
                "operation_id": request.operation_id,
            })),
        };
        let now = self.state.registry.now();
        self.state
            .registry
            .add(approval, &caller.principal.id, now)
            .map_err(|error| {
                ApiError::new(error.code, error.message).with("instance", request.instance.clone())
            })?;
        let worker = self
            .reconcile_worker(&request.worker, &request.instance, None)
            .await?;
        let worker_id = worker.id.clone();
        let mut outcome = outcome;
        outcome.worker = Some(worker);
        self.complete(
            &host,
            &request.operation_id,
            &digest,
            &request.instance,
            &worker_id,
            outcome,
        )
    }

    /// Read-only host inspection plus the Gateway registration projection.
    pub async fn inspect_instance(
        &self,
        caller: &Caller,
        host: &str,
        instance: &str,
    ) -> Result<HostInspection, ApiError> {
        self.require_instance_admin(caller, "host.inspect", instance)?;
        let host = self.authorized_host(caller, host, instance, HOST_OPERATION_INSPECT)?;
        let inspection: HostInspection = self
            .adapter_get(&host, &format!("/v1/instances/{instance}"))
            .await?;
        let worker = self.worker_for_instance(instance);
        Ok(HostInspection {
            host: host.id,
            view: inspection.view,
            worker,
        })
    }

    /// Managed upgrade through the same local verification, permission,
    /// drain, backup, readiness, and quarantine semantics.
    pub async fn upgrade_instance(
        &self,
        caller: &Caller,
        host: &str,
        instance: &str,
        request: HostUpgradeRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        self.require_instance_admin(caller, "host.upgrade", instance)?;
        let host = self.authorized_host(caller, host, instance, HOST_OPERATION_UPGRADE)?;
        validate_operation(&request.operation_id, "", instance)?;
        let digest = request.digest();
        if let Some(record) = self.lifecycle_replay(&request.operation_id, &digest, instance)? {
            return Ok(record);
        }
        self.require_supported(&host, HOST_OPERATION_UPGRADE)
            .await?;
        let worker = self.worker_for_instance(instance).ok_or_else(|| {
            ApiError::new(
                "host.worker_unbound",
                "the instance is not bound to an approved Worker identity",
            )
        })?;
        self.record_in_flight(
            &host,
            &request.operation_id,
            HOST_OPERATION_UPGRADE,
            &digest,
            instance,
            &worker.id,
        )?;
        let result = self
            .adapter_post(
                &host,
                &format!("/v1/instances/{instance}/upgrade"),
                &request,
            )
            .await;
        let outcome: HostOperationOutcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = self.reconcile_worker(&worker.id, instance, None).await;
                return Err(self.operation_failed(
                    &host,
                    &request.operation_id,
                    &digest,
                    instance,
                    &worker.id,
                    error,
                ));
            }
        };
        let worker = self.reconcile_worker(&worker.id, instance, None).await?;
        let worker_id = worker.id.clone();
        let mut outcome = outcome;
        outcome.worker = Some(worker);
        self.complete(
            &host,
            &request.operation_id,
            &digest,
            instance,
            &worker_id,
            outcome,
        )
    }

    /// Managed rollback from a recoverable activation journal.
    pub async fn rollback_instance(
        &self,
        caller: &Caller,
        host: &str,
        instance: &str,
        request: HostRollbackRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        self.require_instance_admin(caller, "host.rollback", instance)?;
        let host = self.authorized_host(caller, host, instance, HOST_OPERATION_ROLLBACK)?;
        validate_operation(&request.operation_id, "", instance)?;
        let digest = request.digest();
        if let Some(record) = self.lifecycle_replay(&request.operation_id, &digest, instance)? {
            return Ok(record);
        }
        self.require_supported(&host, HOST_OPERATION_ROLLBACK)
            .await?;
        let worker = self.worker_for_instance(instance).ok_or_else(|| {
            ApiError::new(
                "host.worker_unbound",
                "the instance is not bound to an approved Worker identity",
            )
        })?;
        self.record_in_flight(
            &host,
            &request.operation_id,
            HOST_OPERATION_ROLLBACK,
            &digest,
            instance,
            &worker.id,
        )?;
        let result = self
            .adapter_post(
                &host,
                &format!("/v1/instances/{instance}/rollback"),
                &request,
            )
            .await;
        let outcome: HostOperationOutcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = self.reconcile_worker(&worker.id, instance, None).await;
                return Err(self.operation_failed(
                    &host,
                    &request.operation_id,
                    &digest,
                    instance,
                    &worker.id,
                    error,
                ));
            }
        };
        let worker = self.reconcile_worker(&worker.id, instance, None).await?;
        let worker_id = worker.id.clone();
        let mut outcome = outcome;
        outcome.worker = Some(worker);
        self.complete(
            &host,
            &request.operation_id,
            &digest,
            instance,
            &worker_id,
            outcome,
        )
    }

    fn lifecycle_replay(
        &self,
        operation: &str,
        digest: &str,
        instance: &str,
    ) -> Result<Option<HostOperationOutcome>, ApiError> {
        let Some(record) = self.state.lifecycle.get(operation) else {
            return Ok(None);
        };
        if record.digest != digest {
            return Err(ApiError::new(
                "host.operation_conflict",
                "this lifecycle operation id was already used with a different request",
            )
            .with("instance", instance.to_string()));
        }
        if let Some(mut outcome) = record.outcome {
            outcome.replayed = true;
            return Ok(Some(outcome));
        }
        if record.status == "failed" {
            if let Some(error) = record.error {
                return Err(error);
            }
        }
        Ok(None)
    }

    fn record_in_flight(
        &self,
        host: &HostConfig,
        operation: &str,
        kind: &str,
        digest: &str,
        instance: &str,
        worker: &str,
    ) -> Result<(), ApiError> {
        self.state.lifecycle.put(Record {
            operation_id: operation.into(),
            kind: kind.into(),
            host: host.id.clone(),
            instance: instance.into(),
            worker: worker.into(),
            digest: digest.into(),
            status: "in_flight".into(),
            outcome: None,
            error: None,
            updated_at: self.state.registry.now(),
        })
    }

    fn operation_failed(
        &self,
        host: &HostConfig,
        operation: &str,
        digest: &str,
        instance: &str,
        worker: &str,
        error: ApiError,
    ) -> ApiError {
        let status = if error.retryable { "unknown" } else { "failed" };
        let kind = self
            .state
            .lifecycle
            .get(operation)
            .map(|record| record.kind)
            .unwrap_or_default();
        let _ = self.state.lifecycle.put(Record {
            operation_id: operation.into(),
            kind,
            host: host.id.clone(),
            instance: instance.into(),
            worker: worker.into(),
            digest: digest.into(),
            status: status.into(),
            outcome: None,
            error: Some(error.clone()),
            updated_at: self.state.registry.now(),
        });
        error
    }

    fn complete(
        &self,
        host: &HostConfig,
        operation: &str,
        digest: &str,
        instance: &str,
        worker: &str,
        outcome: HostOperationOutcome,
    ) -> Result<HostOperationOutcome, ApiError> {
        self.state.lifecycle.put(Record {
            operation_id: operation.into(),
            kind: outcome.kind.clone(),
            host: host.id.clone(),
            instance: instance.into(),
            worker: worker.into(),
            digest: digest.into(),
            status: "succeeded".into(),
            outcome: Some(outcome.clone()),
            error: None,
            updated_at: self.state.registry.now(),
        })?;
        Ok(outcome)
    }

    async fn reconcile_worker(
        &self,
        worker: &str,
        instance: &str,
        endpoint: Option<&str>,
    ) -> Result<AdminWorkerSummary, ApiError> {
        let approval = self.state.registry.approval_record(worker).ok_or_else(|| {
            ApiError::new(
                "worker.not_found",
                "the provisioned instance is not bound to an approved Worker",
            )
        })?;
        let endpoint = endpoint
            .map(normalize)
            .unwrap_or_else(|| approval.endpoint.clone());
        let verified = probe(&self.state.client, &self.state.secret, &endpoint).await?;
        if verified.instance != instance {
            let _ = self.state.registry.mark_unreachable(
                worker,
                self.state.registry.now(),
                "host lifecycle endpoint reported a different instance identity",
            );
            return Err(ApiError::new(
                "worker.instance_mismatch",
                "the host lifecycle endpoint reported a different instance identity",
            ));
        }
        self.state.registry.register(
            worker,
            &WorkerRegistrationRequest {
                endpoint,
                incarnation: verified.incarnation.clone(),
                capacity: Capacity::default(),
            },
            &verified,
            self.state.registry.now(),
        )?;
        self.state.registry.admin_worker(worker).ok_or_else(|| {
            ApiError::new(
                "worker.not_found",
                "the reconciled Worker identity is not approved",
            )
        })
    }

    fn worker_for_instance(&self, instance: &str) -> Option<AdminWorkerSummary> {
        self.state
            .registry
            .admin_workers()
            .into_iter()
            .find(|worker| worker.instance == instance)
    }

    fn require_instance_admin(
        &self,
        caller: &Caller,
        action: &str,
        instance: &str,
    ) -> Result<(), ApiError> {
        caller
            .principal
            .require(scopes::INSTANCE_ADMIN)
            .inspect_err(|_| {
                self.state.registry.audit(
                    self.state.registry.now(),
                    action,
                    instance,
                    &caller.principal.id,
                    "denied",
                    "caller lacks the instance administration scope",
                );
            })
    }

    fn host_config(&self, host: &str) -> Result<HostConfig, ApiError> {
        self.state
            .hosts
            .iter()
            .find(|configured| configured.id == host)
            .cloned()
            .ok_or_else(|| {
                ApiError::new(
                    "host.not_found",
                    format!("host '{host}' is not configured in this Gateway"),
                )
            })
    }

    fn authorized_host(
        &self,
        caller: &Caller,
        host: &str,
        instance: &str,
        operation: &str,
    ) -> Result<HostConfig, ApiError> {
        let host = self.host_config(host)?;
        if let Some(instances) = &host.instances {
            if !instances.iter().any(|allowed| allowed == instance) {
                self.state.registry.audit(
                    self.state.registry.now(),
                    &format!("host.{operation}"),
                    instance,
                    &caller.principal.id,
                    "denied",
                    "instance is outside this host's authorized resource scope",
                );
                return Err(ApiError::new(
                    "host.scope_denied",
                    format!(
                        "host '{}' may not administer instance '{instance}'",
                        host.id
                    ),
                ));
            }
        }
        if let Some(operations) = &host.operations {
            if !operations.iter().any(|allowed| allowed == operation) {
                return Err(ApiError::new(
                    "host.unsupported",
                    format!("host '{}' does not support '{operation}'", host.id),
                ));
            }
        }
        Ok(host)
    }

    async fn require_supported(&self, host: &HostConfig, operation: &str) -> Result<(), ApiError> {
        let descriptor = self.host_descriptor(host).await?;
        if descriptor
            .supported
            .iter()
            .any(|supported| supported == operation)
        {
            Ok(())
        } else {
            Err(ApiError::new(
                "host.unsupported",
                format!("host '{}' does not support '{operation}'", host.id),
            ))
        }
    }

    async fn host_descriptor(&self, host: &HostConfig) -> Result<HostDescriptor, ApiError> {
        let mut descriptor: HostDescriptor = self.adapter_get(host, "/v1/host").await?;
        descriptor.reachable = true;
        Ok(descriptor)
    }

    async fn adapter_get<T: serde::de::DeserializeOwned>(
        &self,
        host: &HostConfig,
        path: &str,
    ) -> Result<T, ApiError> {
        let response = self
            .state
            .lifecycle_client
            .get(format!("{}{path}", host.endpoint))
            .bearer_auth(&host.token)
            .send()
            .await
            .map_err(transport)?;
        decode(response).await
    }

    async fn adapter_post<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        host: &HostConfig,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let response = self
            .state
            .lifecycle_client
            .post(format!("{}{path}", host.endpoint))
            .bearer_auth(&host.token)
            .json(body)
            .send()
            .await
            .map_err(transport)?;
        decode(response).await
    }

    fn worker_token(&self, operation: &str) -> String {
        let mut material = self.state.secret.clone();
        material.extend_from_slice(operation.as_bytes());
        let digest = common::bytes_digest(&material);
        format!("gwb_{}", digest.trim_start_matches("sha256:"))
    }
}

fn validate_operation(operation: &str, worker: &str, instance: &str) -> Result<(), ApiError> {
    if operation.trim().is_empty() || operation.len() > 200 {
        return Err(ApiError::new(
            "host.operation_invalid",
            "operation_id must be 1-200 characters",
        ));
    }
    if instance.trim().is_empty() {
        return Err(ApiError::new(
            "host.instance_invalid",
            "instance identity must not be empty",
        ));
    }
    if !worker.is_empty() && worker.trim().is_empty() {
        return Err(ApiError::new(
            "host.worker_invalid",
            "Worker identity must not be blank",
        ));
    }
    Ok(())
}

fn transport(error: reqwest::Error) -> ApiError {
    if error.is_connect() || error.is_timeout() {
        ApiError::retryable(
            "host.unreachable",
            "the configured host composer did not respond",
        )
    } else {
        ApiError::retryable(
            "host.transport_failed",
            "the host composer exchange ended before a complete response",
        )
    }
}

async fn decode<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ApiError> {
    let status = response.status();
    let bytes = response.bytes().await.map_err(|_| {
        ApiError::retryable(
            "host.transport_failed",
            "the host composer response body was lost",
        )
    })?;
    if !status.is_success() {
        if let Ok(error) = serde_json::from_slice::<ApiError>(&bytes) {
            return Err(error);
        }
        return Err(ApiError::new(
            "host.invalid_response",
            format!("the host composer returned {status} without a typed error"),
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::new(
            "host.invalid_response",
            "the host composer response does not match the lifecycle protocol",
        )
    })
}
