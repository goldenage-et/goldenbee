//! Worker registration, lease renewal, bootstrap, and expiry.

use common::{
    ApiError, Capacity, WorkerIncarnation, WorkerLease, WorkerLeaseRequest,
    WorkerRegistrationRequest,
};

use super::audit::push_audit;
use super::capabilities::{approved_capabilities, locality, normalize_endpoint};
use super::model::{Document, Record, Verified};
use super::Registry;

impl Registry {
    /// Record a verified registration. Duplicate, spoofed, forbidden endpoint
    /// changes, disabled identities, and decommissioned identities are rejected
    /// before any state is written.
    pub fn register(
        &self,
        id: &str,
        request: &WorkerRegistrationRequest,
        verified: &Verified,
        now: i64,
    ) -> Result<WorkerLease, ApiError> {
        let approval = self.approval_or_not_found(id)?.clone();
        if approval.decommissioned {
            self.audit(
                now,
                "worker.register",
                id,
                &format!("worker:{id}"),
                "denied",
                "Worker identity was decommissioned by an operator",
            );
            return Err(ApiError::new(
                "worker.decommissioned",
                "Worker identity was decommissioned by an operator",
            ));
        }
        if !approval.enabled {
            self.audit(
                now,
                "worker.register",
                id,
                &format!("worker:{id}"),
                "denied",
                "Worker registration is disabled",
            );
            return Err(ApiError::new(
                "worker.disabled",
                "Worker identity is disabled by an operator",
            ));
        }
        let endpoint = normalize_endpoint(&request.endpoint);
        if endpoint != normalize_endpoint(&approval.endpoint) {
            self.audit(
                now,
                "worker.register",
                id,
                &format!("worker:{id}"),
                "denied",
                "registration endpoint does not match the approved endpoint",
            );
            return Err(ApiError::new(
                "worker.endpoint_forbidden",
                "registration endpoint is not the approved endpoint",
            ));
        }
        if request.incarnation != verified.incarnation {
            self.audit(
                now,
                "worker.register",
                id,
                &format!("worker:{id}"),
                "denied",
                "caller-supplied incarnation does not match the live endpoint",
            );
            return Err(ApiError::new(
                "worker.incarnation_mismatch",
                "incarnation claim does not match the verified endpoint identity",
            ));
        }
        let (capabilities, approved) = approved_capabilities(&approval, verified)?;
        self.mutate(|document| {
            if let Some(existing) = document.workers.values().find(|record| {
                record.id != id
                    && record.instance == verified.instance
                    && !matches!(record.status.as_str(), "decommissioned" | "disabled")
            }) {
                return Err(ApiError::new(
                    "worker.duplicate_identity",
                    format!(
                        "instance '{}' is already registered as '{}'",
                        verified.instance, existing.id
                    ),
                ));
            }
            if let Some(existing) = document.workers.get(id) {
                if existing.status == "decommissioned" {
                    return Err(ApiError::new(
                        "worker.decommissioned",
                        "Worker identity was decommissioned by an operator",
                    ));
                }
                if existing.status == "disabled" {
                    return Err(ApiError::new(
                        "worker.disabled",
                        "Worker identity is disabled by an operator",
                    ));
                }
                if existing.endpoint != endpoint && existing.reachable(now) {
                    return Err(ApiError::new(
                        "worker.endpoint_forbidden",
                        "a live Worker identity already owns another endpoint",
                    ));
                }
            }
            let record = document
                .workers
                .entry(id.to_string())
                .or_insert_with(|| Record {
                    id: id.to_string(),
                    instance: verified.instance.clone(),
                    endpoint: endpoint.clone(),
                    package: verified.package.clone(),
                    version: verified.version.clone(),
                    protocol: verified.protocol.clone(),
                    capabilities: capabilities.clone(),
                    declared_capabilities: verified.capabilities.clone(),
                    approved_capabilities: approved.clone(),
                    locality: locality(&endpoint),
                    incarnation: verified.incarnation.clone(),
                    incarnations: vec![WorkerIncarnation {
                        incarnation: verified.incarnation.clone(),
                        first_seen_at: now,
                        last_seen_at: now,
                        superseded_at: None,
                    }],
                    capacity: request.capacity.clone(),
                    lease_expires_at: now + self.lease_ttl_ms,
                    renewed_at: now,
                    status: "registered".into(),
                });
            if !record.instance.is_empty() && record.instance != verified.instance {
                return Err(ApiError::new(
                    "worker.instance_mismatch",
                    "registration instance does not match the approved identity",
                ));
            }
            let reconciled = record.incarnation != verified.incarnation;
            record.instance = verified.instance.clone();
            record.endpoint = endpoint.clone();
            record.locality = locality(&endpoint);
            record.observe(&verified.incarnation, now);
            record.package = verified.package.clone();
            record.version = verified.version.clone();
            record.protocol = verified.protocol.clone();
            record.capabilities = capabilities;
            record.declared_capabilities = verified.capabilities.clone();
            record.approved_capabilities = approved;
            record.capacity = request.capacity.clone();
            record.lease_expires_at = now + self.lease_ttl_ms;
            record.renewed_at = now;
            record.status = "registered".into();
            let lease = record.lease();
            push_audit(
                document,
                now,
                if reconciled {
                    "worker.incarnation_reconciled"
                } else {
                    "worker.register"
                },
                id,
                &format!("worker:{id}"),
                "accepted",
                if reconciled {
                    "new incarnation verified and previous references retained"
                } else {
                    "registration verified against the live endpoint"
                },
            );
            Ok(lease)
        })
    }

    /// Renew a lease using Gateway-side expiry.
    pub fn renew(
        &self,
        id: &str,
        request: &WorkerLeaseRequest,
        verified: &Verified,
        now: i64,
    ) -> Result<WorkerLease, ApiError> {
        let approval = self.approval_or_not_found(id)?.clone();
        if approval.decommissioned {
            return Err(ApiError::new(
                "worker.decommissioned",
                "Worker identity was decommissioned by an operator",
            ));
        }
        if !approval.enabled {
            self.audit(
                now,
                "worker.lease_renew",
                id,
                &format!("worker:{id}"),
                "denied",
                "Worker renewal is disabled",
            );
            return Err(ApiError::new(
                "worker.disabled",
                "Worker identity is disabled by an operator",
            ));
        }
        if request.incarnation != verified.incarnation {
            self.audit(
                now,
                "worker.lease_renew",
                id,
                &format!("worker:{id}"),
                "denied",
                "renewal incarnation does not match the live endpoint",
            );
            return Err(ApiError::new(
                "worker.incarnation_mismatch",
                "renewal incarnation does not match the verified endpoint identity",
            ));
        }
        let (capabilities, approved) = approved_capabilities(&approval, verified)?;
        self.mutate(|document| {
            let record = document
                .workers
                .get(id)
                .ok_or_else(|| ApiError::new("worker.not_found", "Worker is not registered"))?;
            if record.status == "decommissioned" {
                return Err(ApiError::new(
                    "worker.decommissioned",
                    "Worker identity was decommissioned by an operator",
                ));
            }
            if record.status == "disabled" {
                return Err(ApiError::new(
                    "worker.disabled",
                    "Worker identity is disabled by an operator",
                ));
            }
            if record.instance != verified.instance {
                push_audit(
                    document,
                    now,
                    "worker.lease_renew",
                    id,
                    &format!("worker:{id}"),
                    "denied",
                    "endpoint identity does not match the registered instance",
                );
                return Err(ApiError::new(
                    "worker.instance_mismatch",
                    "endpoint identity does not match the registered instance",
                ));
            }
            let reconciled = record.incarnation != verified.incarnation;
            let record = document
                .workers
                .get_mut(id)
                .expect("record was checked above");
            record.observe(&verified.incarnation, now);
            record.capabilities = capabilities;
            record.declared_capabilities = verified.capabilities.clone();
            record.approved_capabilities = approved;
            record.capacity = request.capacity.clone();
            record.lease_expires_at = now + self.lease_ttl_ms;
            record.renewed_at = now;
            record.status = "registered".into();
            let lease = record.lease();
            if reconciled {
                push_audit(
                    document,
                    now,
                    "worker.incarnation_reconciled",
                    id,
                    &format!("worker:{id}"),
                    "accepted",
                    "reconnection with a new incarnation kept historical references",
                );
            }
            Ok(lease)
        })
    }

    /// Install startup preflight or explicit probe state for an approved Worker.
    pub fn bootstrap(
        &self,
        id: &str,
        verified: &Verified,
        now: i64,
    ) -> Result<WorkerLease, ApiError> {
        let approval = self.approval_or_not_found(id)?.clone();
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
        let (capabilities, approved) = approved_capabilities(&approval, verified)?;
        self.mutate(|document| {
            if let Some(existing) = document.workers.get(id) {
                if matches!(existing.status.as_str(), "decommissioned" | "disabled") {
                    return Err(ApiError::new(
                        if existing.status == "decommissioned" {
                            "worker.decommissioned"
                        } else {
                            "worker.disabled"
                        },
                        "Worker identity is not available for registration",
                    ));
                }
            }
            let record = document
                .workers
                .entry(id.to_string())
                .or_insert_with(|| Record {
                    id: id.to_string(),
                    instance: verified.instance.clone(),
                    endpoint: normalize_endpoint(&approval.endpoint),
                    package: verified.package.clone(),
                    version: verified.version.clone(),
                    protocol: verified.protocol.clone(),
                    capabilities: capabilities.clone(),
                    declared_capabilities: verified.capabilities.clone(),
                    approved_capabilities: approved.clone(),
                    locality: locality(&approval.endpoint),
                    incarnation: verified.incarnation.clone(),
                    incarnations: vec![WorkerIncarnation {
                        incarnation: verified.incarnation.clone(),
                        first_seen_at: now,
                        last_seen_at: now,
                        superseded_at: None,
                    }],
                    capacity: Capacity::default(),
                    lease_expires_at: now + self.lease_ttl_ms,
                    renewed_at: now,
                    status: "registered".into(),
                });
            record.instance = verified.instance.clone();
            record.observe(&verified.incarnation, now);
            record.endpoint = normalize_endpoint(&approval.endpoint);
            record.locality = locality(&approval.endpoint);
            record.package = verified.package.clone();
            record.version = verified.version.clone();
            record.protocol = verified.protocol.clone();
            record.capabilities = capabilities;
            record.declared_capabilities = verified.capabilities.clone();
            record.approved_capabilities = approved;
            record.lease_expires_at = now + self.lease_ttl_ms;
            record.renewed_at = now;
            record.status = "registered".into();
            let lease = record.lease();
            push_audit(
                document,
                now,
                "worker.preflight",
                id,
                "gateway",
                "accepted",
                "endpoint identity verification succeeded",
            );
            Ok(lease)
        })
    }

    /// Record an approved Worker whose endpoint could not be verified.
    pub fn mark_unreachable(&self, id: &str, now: i64, detail: &str) -> Result<(), ApiError> {
        let approval = self.approval_or_not_found(id)?.clone();
        self.mutate(|document| {
            let record = document
                .workers
                .entry(id.to_string())
                .or_insert_with(|| Record {
                    id: id.to_string(),
                    instance: approval.instance.clone().unwrap_or_default(),
                    endpoint: normalize_endpoint(&approval.endpoint),
                    package: approval.package.clone().unwrap_or_default(),
                    version: String::new(),
                    protocol: String::new(),
                    capabilities: Vec::new(),
                    declared_capabilities: Vec::new(),
                    approved_capabilities: approval.capabilities.clone().unwrap_or_default(),
                    locality: locality(&approval.endpoint),
                    incarnation: String::new(),
                    incarnations: Vec::new(),
                    capacity: Capacity::default(),
                    lease_expires_at: 0,
                    renewed_at: 0,
                    status: "unreachable".into(),
                });
            if !matches!(record.status.as_str(), "disabled" | "decommissioned") {
                record.status = "unreachable".into();
                record.lease_expires_at = 0;
            }
            push_audit(
                document,
                now,
                "worker.preflight",
                id,
                "gateway",
                "denied",
                detail,
            );
            Ok(())
        })
    }

    /// Mark expired leases unreachable. Task outcomes are not touched.
    pub fn expire(&self, now: i64) -> usize {
        let changed = {
            let mut document = self.lock();
            let changed = expire_document(&mut document.document, now);
            if changed > 0 {
                let _ = self.write(&document.document);
            }
            changed
        };
        changed
    }
}

pub(super) fn expire_document(document: &mut Document, now: i64) -> usize {
    let mut changed = 0;
    let mut expired = Vec::new();
    for record in document.workers.values_mut() {
        if record.status == "registered" && record.lease_expires_at <= now {
            record.status = "unreachable".into();
            changed += 1;
            expired.push(record.id.clone());
        }
    }
    for worker in expired {
        push_audit(
            document,
            now,
            "worker.lease_expired",
            &worker,
            "gateway",
            "accepted",
            "lease expired; Worker is unreachable, task outcomes unchanged",
        );
    }
    changed
}
