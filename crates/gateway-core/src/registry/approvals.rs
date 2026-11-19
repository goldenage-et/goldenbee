//! Runtime approvals: add, update, rollback, status changes, and config seeding.

use common::{AdminWorkerSummary, ApiError, RegistryAuditEntry};

use super::audit::push_audit;
use super::capabilities::{admin_summary, locality, normalize_endpoint};
use super::model::{
    Approval, ApprovalPatch, ApprovalRecord, ApprovalSource, Document, NewApproval,
};
use super::persistence::hash_token;
use super::Registry;

impl Registry {
    /// Add a runtime approval. Returns the summary and whether it was created.
    /// A repeated id with the same digest replays the existing approval.
    pub fn add(
        &self,
        input: NewApproval,
        principal: &str,
        now: i64,
    ) -> Result<(AdminWorkerSummary, bool), ApiError> {
        let hash = input
            .control_token
            .as_deref()
            .map(hash_token)
            .unwrap_or_default();
        self.mutate(|document| {
            if let Some(existing) = document.approvals.get(&input.id) {
                if existing.decommissioned {
                    return Err(ApiError::new(
                        "worker.decommissioned",
                        "Worker identity was decommissioned by an operator; use a new id",
                    ));
                }
                if existing.request_digest.as_deref() == Some(input.request_digest.as_str()) {
                    let summary = admin_summary(existing, document.workers.get(&input.id), now);
                    return Ok((summary, false));
                }
                return Err(ApiError::new(
                    "worker.idempotency_conflict",
                    "this Worker id was already added with a different request",
                )
                .with("worker", input.id.clone()));
            }
            if !hash.is_empty() {
                if let Some(owner) = document.approvals.values().find(|approval| {
                    approval.id != input.id
                        && !approval.control_token_hash.is_empty()
                        && approval.control_token_hash == hash
                }) {
                    return Err(ApiError::new(
                        "worker.control_conflict",
                        format!("control token is already bound to Worker '{}'", owner.id),
                    ));
                }
            }
            let record = ApprovalRecord {
                id: input.id.clone(),
                endpoint: normalize_endpoint(&input.endpoint),
                instance: input.instance,
                package: input.package,
                capabilities: input.capabilities,
                control_token_hash: hash,
                enabled: true,
                revision: 1,
                source: ApprovalSource::Runtime,
                request_digest: Some(input.request_digest),
                update_digest: None,
                decommissioned: false,
                created_at: now,
                updated_at: now,
            };
            document.approvals.insert(input.id.clone(), record.clone());
            push_audit(
                document,
                now,
                "worker.add",
                &input.id,
                principal,
                "accepted",
                "operator added an approved Worker identity",
            );
            Ok((
                admin_summary(&record, document.workers.get(&input.id), now),
                true,
            ))
        })
    }

    /// Update a runtime approval. The revision is an optimistic concurrency
    /// check. A live endpoint owner cannot be moved by an update.
    pub fn update(
        &self,
        id: &str,
        patch: ApprovalPatch,
        principal: &str,
        now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        self.mutate(|document| {
            let record = document.workers.get(id);
            let approval = document.approvals.get_mut(id).ok_or_else(|| {
                ApiError::new(
                    "worker.not_found",
                    format!("Worker '{id}' is not approved by this Gateway"),
                )
            })?;
            if approval.decommissioned {
                return Err(ApiError::new(
                    "worker.decommissioned",
                    "Worker identity was decommissioned by an operator",
                ));
            }
            if patch.request_digest.is_some()
                && approval.update_digest.as_deref() == patch.request_digest.as_deref()
                && approval.revision == patch.revision + 1
            {
                let summary = admin_summary(approval, document.workers.get(id), now);
                return Ok(summary);
            }
            if approval.revision != patch.revision {
                return Err(ApiError::new(
                    "worker.revision_conflict",
                    "Worker approval was changed by another decision",
                )
                .with("expected", approval.revision.to_string())
                .with("observed", patch.revision.to_string()));
            }
            let endpoint = patch
                .endpoint
                .as_deref()
                .map(normalize_endpoint)
                .unwrap_or_else(|| approval.endpoint.clone());
            let endpoint_changed = endpoint != approval.endpoint;
            if endpoint_changed {
                if let Some(record) = record {
                    if record.reachable(now) {
                        return Err(ApiError::new(
                            "worker.endpoint_in_use",
                            "a live Worker incarnation still owns the current endpoint",
                        ));
                    }
                }
            }
            if let Some(instance) = patch.instance {
                approval.instance = Some(instance);
            }
            if let Some(package) = patch.package {
                approval.package = Some(package);
            }
            if let Some(capabilities) = patch.capabilities {
                approval.capabilities = Some(capabilities);
            }
            approval.endpoint = endpoint.clone();
            approval.source = ApprovalSource::Runtime;
            approval.revision += 1;
            approval.updated_at = now;
            approval.update_digest = patch.request_digest;
            if endpoint_changed {
                if let Some(record) = document.workers.get_mut(id) {
                    record.endpoint = endpoint.clone();
                    record.locality = locality(&endpoint);
                    record.lease_expires_at = 0;
                    record.renewed_at = 0;
                    if !matches!(record.status.as_str(), "disabled" | "decommissioned") {
                        record.status = "unreachable".into();
                    }
                }
            }
            if let Some(capabilities) = &approval.capabilities {
                if let Some(record) = document.workers.get_mut(id) {
                    record.approved_capabilities = capabilities.clone();
                    if !matches!(record.status.as_str(), "disabled" | "decommissioned") {
                        record.capabilities.retain(|capability| {
                            capabilities.iter().any(|id| id == &capability.id)
                        });
                    }
                }
            }
            let summary = admin_summary(approval, document.workers.get(id), now);
            push_audit(
                document,
                now,
                "worker.update",
                id,
                principal,
                "accepted",
                "operator updated an approved Worker identity",
            );
            Ok(summary)
        })
    }

    /// Remove a runtime approval that failed its add preflight. Config-owned
    /// approvals are never removed by this rollback path.
    pub fn rollback_add(&self, id: &str, principal: &str, now: i64) -> Result<(), ApiError> {
        self.mutate(|document| {
            let runtime = document
                .approvals
                .get(id)
                .is_some_and(|approval| approval.source == ApprovalSource::Runtime);
            if runtime {
                document.approvals.remove(id);
                document.workers.remove(id);
                push_audit(
                    document,
                    now,
                    "worker.add",
                    id,
                    principal,
                    "denied",
                    "preflight failed and the runtime approval was rolled back",
                );
            }
            Ok(())
        })
    }

    /// Disable an identity. The lease is cleared immediately. Existing task
    /// projections remain readable and are not rewritten.
    pub fn disable(
        &self,
        id: &str,
        principal: &str,
        now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        self.mutate(|document| {
            let approval = document.approvals.get_mut(id).ok_or_else(|| {
                ApiError::new(
                    "worker.not_found",
                    format!("Worker '{id}' is not approved by this Gateway"),
                )
            })?;
            if approval.decommissioned {
                return Err(ApiError::new(
                    "worker.decommissioned",
                    "Worker identity was decommissioned by an operator",
                ));
            }
            let already_disabled = !approval.enabled;
            if !already_disabled {
                approval.enabled = false;
                approval.revision += 1;
                approval.updated_at = now;
            }
            if let Some(record) = document.workers.get_mut(id) {
                record.status = "disabled".into();
                record.lease_expires_at = 0;
            }
            let summary = admin_summary(approval, document.workers.get(id), now);
            push_audit(
                document,
                now,
                "worker.disable",
                id,
                principal,
                "accepted",
                if already_disabled {
                    "Worker was already disabled"
                } else {
                    "operator disabled registration, renewal, discovery, and new routing"
                },
            );
            Ok(summary)
        })
    }

    /// Enable registration again. The previous lease is not restored.
    pub fn enable(
        &self,
        id: &str,
        principal: &str,
        now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        self.mutate(|document| {
            let approval = document.approvals.get_mut(id).ok_or_else(|| {
                ApiError::new(
                    "worker.not_found",
                    format!("Worker '{id}' is not approved by this Gateway"),
                )
            })?;
            if approval.decommissioned {
                return Err(ApiError::new(
                    "worker.decommissioned",
                    "Worker identity was decommissioned by an operator",
                ));
            }
            if !approval.enabled {
                approval.enabled = true;
                approval.revision += 1;
                approval.updated_at = now;
            }
            if let Some(record) = document.workers.get_mut(id) {
                record.status = "unreachable".into();
                record.lease_expires_at = 0;
                record.renewed_at = 0;
            }
            let summary = admin_summary(approval, document.workers.get(id), now);
            push_audit(
                document,
                now,
                "worker.enable",
                id,
                principal,
                "accepted",
                "operator enabled registration; the previous lease was not restored",
            );
            Ok(summary)
        })
    }

    /// Decommission an identity. Task history is retained; the identity is
    /// terminal and a replacement must use a new id.
    pub fn decommission(
        &self,
        id: &str,
        principal: &str,
        now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        self.mutate(|document| {
            let approval = document.approvals.get_mut(id).ok_or_else(|| {
                ApiError::new(
                    "worker.not_found",
                    format!("Worker '{id}' is not approved by this Gateway"),
                )
            })?;
            if approval.decommissioned {
                let summary = admin_summary(approval, document.workers.get(id), now);
                return Ok(summary);
            }
            approval.decommissioned = true;
            approval.enabled = false;
            approval.revision += 1;
            approval.updated_at = now;
            if let Some(record) = document.workers.get_mut(id) {
                record.status = "decommissioned".into();
                record.lease_expires_at = 0;
            }
            let summary = admin_summary(approval, document.workers.get(id), now);
            push_audit(
                document,
                now,
                "worker.decommission",
                id,
                principal,
                "accepted",
                "operator decommissioned the Worker identity; task history was retained",
            );
            Ok(summary)
        })
    }
}

pub(super) fn seed_approvals(
    document: &mut Document,
    approvals: Vec<Approval>,
    now: i64,
) -> Result<(), ApiError> {
    for seed in approvals {
        let endpoint = normalize_endpoint(&seed.endpoint);
        let hash = seed
            .control_token
            .as_deref()
            .map(hash_token)
            .unwrap_or_default();
        let legacy_decommissioned = document
            .workers
            .get(&seed.id)
            .is_some_and(|record| record.status == "decommissioned");
        match document.approvals.get_mut(&seed.id) {
            Some(existing) => {
                if existing.decommissioned {
                    continue;
                }
                if existing.source != ApprovalSource::Config {
                    // Runtime decisions are authoritative after bootstrap.
                    continue;
                }
                let changed = existing.endpoint != endpoint
                    || existing.instance != seed.instance
                    || existing.package != seed.package
                    || existing.capabilities != seed.capabilities
                    || existing.control_token_hash != hash;
                if !changed {
                    continue;
                }
                existing.endpoint = endpoint.clone();
                existing.instance = seed.instance;
                existing.package = seed.package;
                existing.capabilities = seed.capabilities;
                existing.control_token_hash = hash;
                existing.revision += 1;
                existing.updated_at = now;
                if legacy_decommissioned {
                    existing.decommissioned = true;
                    existing.enabled = false;
                }
                if let Some(record) = document.workers.get_mut(&seed.id) {
                    if record.endpoint != endpoint {
                        record.endpoint = endpoint.clone();
                        record.locality = locality(&endpoint);
                        record.lease_expires_at = 0;
                        record.renewed_at = 0;
                        if !matches!(record.status.as_str(), "disabled" | "decommissioned") {
                            record.status = "unreachable".into();
                        }
                    }
                }
                let policy_revision = document.version as u64;
                document.audit.push(RegistryAuditEntry {
                    at: now,
                    action: "worker.config_seed_updated".into(),
                    worker: seed.id,
                    principal: "gateway".into(),
                    outcome: "accepted".into(),
                    detail: "config-owned approval was updated from the operator file".into(),
                    source: "gateway".into(),
                    policy_revision,
                    correlation: String::new(),
                });
            }
            None => {
                document.approvals.insert(
                    seed.id.clone(),
                    ApprovalRecord {
                        id: seed.id,
                        endpoint,
                        instance: seed.instance,
                        package: seed.package,
                        capabilities: seed.capabilities,
                        control_token_hash: hash,
                        revision: 1,
                        source: ApprovalSource::Config,
                        request_digest: None,
                        update_digest: None,
                        decommissioned: legacy_decommissioned,
                        enabled: !legacy_decommissioned,
                        created_at: now,
                        updated_at: now,
                    },
                );
            }
        }
    }
    Ok(())
}
