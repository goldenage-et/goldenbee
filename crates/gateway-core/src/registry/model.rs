//! Persisted approvals, Worker records, registry documents, and their leases.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use common::{
    CapabilityDescriptor, Capacity, RegistryAuditEntry, Team, WorkerIncarnation, WorkerLease,
    WorkerSummary,
};

/// Current persisted registry schema.
pub const REGISTRY_SCHEMA: u32 = 2;

/// Where an approval came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalSource {
    /// Seeded from the operator configuration file.
    Config,
    /// Created or changed through the Gateway admin API.
    Runtime,
}

impl ApprovalSource {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Runtime => "runtime",
        }
    }
}

/// Operator approval input used to seed the registry at startup.
#[derive(Debug, Clone)]
pub struct Approval {
    /// Gateway-assigned identity.
    pub id: String,
    /// Expected Worker base URL.
    pub endpoint: String,
    /// Expected Worker instance identity.
    pub instance: Option<String>,
    /// Expected package name.
    pub package: Option<String>,
    /// Approved capability identifiers. Omitted approves the verified card.
    pub capabilities: Option<Vec<String>>,
    /// Bearer token the Worker uses for authenticated control traffic. It is
    /// hashed before it is stored.
    pub control_token: Option<String>,
}

/// Persisted approval for one Worker identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    /// Gateway-assigned identity.
    pub id: String,
    /// Approved Worker base URL.
    pub endpoint: String,
    /// Expected instance identity.
    #[serde(default)]
    pub instance: Option<String>,
    /// Expected package name.
    #[serde(default)]
    pub package: Option<String>,
    /// Approved capability identifiers. Omitted approves the verified card.
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    /// SHA-256 of the Worker control token, or empty when no token is bound.
    #[serde(default)]
    pub control_token_hash: String,
    /// Whether registration, renewal, discovery, and routing are enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Monotonic approval revision.
    #[serde(default = "default_revision")]
    pub revision: u64,
    /// Config ownership or runtime decision.
    #[serde(default = "default_source")]
    pub source: ApprovalSource,
    /// Digest of the add request, used for idempotent retries.
    #[serde(default)]
    pub request_digest: Option<String>,
    /// Digest of the last successful update, used for idempotent retries.
    #[serde(default)]
    pub update_digest: Option<String>,
    /// Terminal decommission marker. A replacement uses a new identity.
    #[serde(default)]
    pub decommissioned: bool,
    /// Creation time.
    #[serde(default)]
    pub created_at: i64,
    /// Last update time.
    #[serde(default)]
    pub updated_at: i64,
}

fn default_enabled() -> bool {
    true
}

fn default_revision() -> u64 {
    1
}

fn default_source() -> ApprovalSource {
    ApprovalSource::Config
}

/// Input for a runtime approval add. The registry owns hashing.
#[derive(Debug, Clone)]
pub struct NewApproval {
    /// Identity.
    pub id: String,
    /// Endpoint.
    pub endpoint: String,
    /// Expected instance.
    pub instance: Option<String>,
    /// Expected package.
    pub package: Option<String>,
    /// Approved capabilities.
    pub capabilities: Option<Vec<String>>,
    /// Plain control token. Hashed before persistence.
    pub control_token: Option<String>,
    /// Request digest for idempotent retries.
    pub request_digest: String,
}

/// Runtime approval patch. Only fields the operator explicitly changed are
/// present.
#[derive(Debug, Clone, Default)]
pub struct ApprovalPatch {
    /// Revision the caller observed.
    pub revision: u64,
    /// New endpoint.
    pub endpoint: Option<String>,
    /// New expected instance.
    pub instance: Option<String>,
    /// New expected package.
    pub package: Option<String>,
    /// New approved capabilities.
    pub capabilities: Option<Vec<String>>,
    /// Digest of the update request, when an idempotency key was supplied.
    pub request_digest: Option<String>,
}

/// Descriptor verified from the Worker's live management routes.
#[derive(Debug, Clone)]
pub struct Verified {
    /// Instance identity reported by `/v1/health`.
    pub instance: String,
    /// Process incarnation reported by `/v1/health`.
    pub incarnation: String,
    /// Package name reported by `/v1/capabilities`.
    pub package: String,
    /// Package version reported by `/v1/capabilities`.
    pub version: String,
    /// Protocol version reported by `/v1/capabilities`.
    pub protocol: String,
    /// Capability descriptors reported by `/v1/capabilities`.
    pub capabilities: Vec<CapabilityDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Record {
    pub(super) id: String,
    pub(super) instance: String,
    pub(super) endpoint: String,
    pub(super) package: String,
    pub(super) version: String,
    pub(super) protocol: String,
    pub(super) capabilities: Vec<CapabilityDescriptor>,
    pub(super) declared_capabilities: Vec<CapabilityDescriptor>,
    pub(super) approved_capabilities: Vec<String>,
    pub(super) locality: String,
    pub(super) incarnation: String,
    pub(super) incarnations: Vec<WorkerIncarnation>,
    pub(super) capacity: Capacity,
    pub(super) lease_expires_at: i64,
    pub(super) renewed_at: i64,
    pub(super) status: String,
}

impl Record {
    pub(super) fn reachable(&self, now: i64) -> bool {
        self.status == "registered" && self.lease_expires_at > now
    }

    pub(super) fn lease(&self) -> WorkerLease {
        WorkerLease {
            worker: self.id.clone(),
            instance: self.instance.clone(),
            endpoint: self.endpoint.clone(),
            incarnation: self.incarnation.clone(),
            package: self.package.clone(),
            version: self.version.clone(),
            protocol: self.protocol.clone(),
            capabilities: self.capabilities.clone(),
            declared_capabilities: self.declared_capabilities.clone(),
            approved_capabilities: self.approved_capabilities.clone(),
            capacity: self.capacity.clone(),
            lease_expires_at: self.lease_expires_at,
            renewed_at: self.renewed_at,
            status: self.status.clone(),
            incarnations: self.incarnations.clone(),
        }
    }

    pub(super) fn summary(&self, now: i64) -> WorkerSummary {
        WorkerSummary {
            id: self.id.clone(),
            endpoint: self.endpoint.clone(),
            instance: self.instance.clone(),
            package: self.package.clone(),
            version: self.version.clone(),
            capabilities: self.capabilities.clone(),
            declared_capabilities: self.declared_capabilities.clone(),
            approved_capabilities: self.approved_capabilities.clone(),
            protocol: self.protocol.clone(),
            incarnation: self.incarnation.clone(),
            incarnations: self.incarnations.clone(),
            locality: self.locality.clone(),
            capacity: self.capacity.clone(),
            lease_expires_at: self.lease_expires_at,
            renewed_at: self.renewed_at,
            status: self.status.clone(),
            reachable: self.reachable(now),
        }
    }

    pub(super) fn observe(&mut self, incarnation: &str, now: i64) {
        if let Some(current) = self
            .incarnations
            .iter_mut()
            .find(|entry| entry.incarnation == self.incarnation)
        {
            current.last_seen_at = now;
        }
        if self.incarnation != incarnation {
            if let Some(current) = self
                .incarnations
                .iter_mut()
                .find(|entry| entry.incarnation == self.incarnation)
            {
                current.superseded_at = Some(now);
            }
            self.incarnations.push(WorkerIncarnation {
                incarnation: incarnation.to_string(),
                first_seen_at: now,
                last_seen_at: now,
                superseded_at: None,
            });
            self.incarnation = incarnation.to_string();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Document {
    #[serde(default = "default_schema")]
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) workers: BTreeMap<String, Record>,
    #[serde(default)]
    pub(crate) approvals: BTreeMap<String, ApprovalRecord>,
    /// Versioned team rosters keyed by namespace and name.
    #[serde(default)]
    pub(crate) teams: BTreeMap<String, Team>,
    #[serde(default)]
    pub(crate) audit: Vec<RegistryAuditEntry>,
}

fn default_schema() -> u32 {
    1
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: REGISTRY_SCHEMA,
            workers: BTreeMap::new(),
            approvals: BTreeMap::new(),
            teams: BTreeMap::new(),
            audit: Vec::new(),
        }
    }
}

pub(super) struct Inner {
    pub(super) document: Document,
    /// SHA-256 token hash -> Worker identity. Plain tokens are never retained.
    pub(super) control: BTreeMap<String, String>,
}
