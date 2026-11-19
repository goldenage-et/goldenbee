//! Worker registration, leases, discovery, administration, and audit.

use crate::limits::Capacity;
use serde::{Deserialize, Serialize};

/// One approved Worker registration visible to authorized callers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSummary {
    /// Gateway-assigned Worker identity.
    pub id: String,
    /// Worker endpoint.
    pub endpoint: String,
    /// Worker process identity.
    pub instance: String,
    /// Installed package name.
    pub package: String,
    /// Installed package version.
    pub version: String,
    /// Effective capabilities after verification and operator approval.
    pub capabilities: Vec<CapabilityDescriptor>,
    /// Static capabilities the installed package card declares.
    #[serde(default)]
    pub declared_capabilities: Vec<CapabilityDescriptor>,
    /// Operator-approved capability identifiers bound at registration.
    #[serde(default)]
    pub approved_capabilities: Vec<String>,
    /// Supported management protocol version.
    #[serde(default)]
    pub protocol: String,
    /// Current process incarnation.
    #[serde(default)]
    pub incarnation: String,
    /// Historical incarnations retained across reconnection.
    #[serde(default)]
    pub incarnations: Vec<WorkerIncarnation>,
    /// Endpoint locality as derived by the Gateway.
    #[serde(default)]
    pub locality: String,
    /// Live capacity reported by the current incarnation. This is distinct
    /// from the static capability declarations above.
    #[serde(default)]
    pub capacity: Capacity,
    /// Gateway-measured lease expiry.
    #[serde(default)]
    pub lease_expires_at: i64,
    /// Last accepted renewal.
    #[serde(default)]
    pub renewed_at: i64,
    /// `registered`, `unreachable`, or `decommissioned`.
    #[serde(default)]
    pub status: String,
    /// Lease-derived reachability.
    pub reachable: bool,
}

/// Locality value for a loopback Worker endpoint.
pub const LOCALITY_LOCAL: &str = "local";
/// Locality value for a non-loopback Worker endpoint.
pub const LOCALITY_REMOTE: &str = "remote";

/// Authenticated registration body sent by an approved Worker. Identity comes
/// from the Gateway's control-token binding; package, protocol, instance, and
/// capabilities are verified against the Worker's live management routes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRegistrationRequest {
    /// Endpoint the Worker expects to be registered under.
    pub endpoint: String,
    /// Current process incarnation.
    pub incarnation: String,
    /// Live capacity.
    #[serde(default)]
    pub capacity: Capacity,
}

/// Lease renewal authenticated as the registered Worker incarnation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerLeaseRequest {
    /// Current process incarnation. A different verified incarnation triggers
    /// reconciliation instead of silently replacing history.
    pub incarnation: String,
    /// Live capacity.
    #[serde(default)]
    pub capacity: Capacity,
}

/// One process incarnation retained for historical references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerIncarnation {
    /// Process incarnation identity.
    pub incarnation: String,
    /// First accepted contact.
    pub first_seen_at: i64,
    /// Last accepted contact.
    pub last_seen_at: i64,
    /// When a newer incarnation superseded this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<i64>,
}

/// Current registration and lease state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerLease {
    /// Gateway-assigned Worker identity.
    pub worker: String,
    /// Verified Worker instance identity.
    pub instance: String,
    /// Verified endpoint.
    pub endpoint: String,
    /// Current process incarnation.
    pub incarnation: String,
    /// Verified package name.
    pub package: String,
    /// Verified package version.
    pub version: String,
    /// Verified management protocol version.
    pub protocol: String,
    /// Effective capabilities after verification and operator approval.
    pub capabilities: Vec<CapabilityDescriptor>,
    /// Static capabilities the installed package card declares.
    pub declared_capabilities: Vec<CapabilityDescriptor>,
    /// Operator-approved capability identifiers.
    pub approved_capabilities: Vec<String>,
    /// Live capacity for the current incarnation.
    pub capacity: Capacity,
    /// Gateway-measured expiry.
    pub lease_expires_at: i64,
    /// Last accepted renewal.
    pub renewed_at: i64,
    /// Registration status.
    pub status: String,
    /// Retained incarnation history.
    pub incarnations: Vec<WorkerIncarnation>,
}

/// Gateway admin inventory projection for one approved Worker identity.
///
/// This shape is read-only wire data. Mutable Gateway registry state and
/// control-token hashes stay in the Gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminWorkerSummary {
    /// Gateway-assigned identity.
    pub id: String,
    /// Approved endpoint.
    pub endpoint: String,
    /// Configured expected instance, or the last verified instance.
    #[serde(default)]
    pub instance: String,
    /// Configured expected package, or the last verified package.
    #[serde(default)]
    pub package: String,
    /// Operator-approved capability subset. Omitted means every verified
    /// capability is approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    /// Effective approved capabilities after the last verification.
    #[serde(default)]
    pub approved_capabilities: Vec<String>,
    /// Capabilities verified from the live package card.
    #[serde(default)]
    pub declared_capabilities: Vec<CapabilityDescriptor>,
    /// Whether registration, renewal, discovery, and routing are enabled.
    pub enabled: bool,
    /// Optimistic concurrency revision.
    pub revision: u64,
    /// `config` or `runtime`.
    pub source: String,
    /// `approved`, `registered`, `unreachable`, `disabled`, or
    /// `decommissioned`.
    pub status: String,
    /// Lease-derived reachability.
    #[serde(default)]
    pub reachable: bool,
    /// Whether a control token is bound. The token value is never returned.
    #[serde(default)]
    pub control_token_set: bool,
    /// Current process incarnation, when registered.
    #[serde(default)]
    pub incarnation: String,
    /// Historical incarnations retained across reconnection.
    #[serde(default)]
    pub incarnations: Vec<WorkerIncarnation>,
    /// Endpoint locality.
    #[serde(default)]
    pub locality: String,
    /// Gateway-measured lease expiry.
    #[serde(default)]
    pub lease_expires_at: i64,
    /// Last accepted renewal.
    #[serde(default)]
    pub renewed_at: i64,
    /// Approval creation time.
    pub created_at: i64,
    /// Last approval update time.
    pub updated_at: i64,
}

/// Admin request to add one approved Worker identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddWorkerRequest {
    /// Gateway-assigned identity.
    pub id: String,
    /// Worker base URL.
    pub endpoint: String,
    /// Expected instance identity, when pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Expected package name, when pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Approved capability subset. Omitted approves the verified card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    /// Optional control token input. When omitted the Gateway generates one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_token: Option<String>,
    /// Verify the endpoint before returning success. Defaults to true.
    #[serde(default = "default_preflight")]
    pub preflight: bool,
    /// Caller-scoped idempotency key for this add operation.
    pub idempotency_key: String,
}

fn default_preflight() -> bool {
    true
}

/// Admin add result. A generated control token is shown exactly once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddWorkerResponse {
    /// Created or replayed approval.
    pub worker: AdminWorkerSummary,
    /// Generated control token. Present only in the response that created it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_token: Option<String>,
}

/// Admin request to update one approved Worker. The revision prevents a stale
/// writer from overwriting a newer decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateWorkerRequest {
    /// Revision the caller observed.
    pub revision: u64,
    /// New endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// New expected instance identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// New expected package name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// New approved capability subset. Expanding it requires a successful
    /// probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    /// Optional idempotency key. Repeating the same update after a lost
    /// acknowledgment converges on the already committed revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// Filter accepted by authorized discovery.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkerDiscoveryFilter {
    /// Require this capability identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    /// Require this management protocol version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// Require `local` or `remote` locality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locality: Option<String>,
    /// Require `healthy`, `unreachable`, or `any` health.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
}

/// One auditable Gateway registry decision. Never contains control tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryAuditEntry {
    /// Decision time as measured by the Gateway.
    pub at: i64,
    /// Stable action, for example `worker.register`.
    pub action: String,
    /// Worker identity the decision applied to.
    pub worker: String,
    /// Authenticated control principal or caller.
    pub principal: String,
    /// `accepted` or `denied`.
    pub outcome: String,
    /// Redacted detail.
    pub detail: String,
    /// Decision source.
    #[serde(default)]
    pub source: String,
    /// Effective policy revision.
    #[serde(default)]
    pub policy_revision: u64,
    /// Correlation retained independently of sampled telemetry.
    #[serde(default)]
    pub correlation: String,
}

/// Registered Worker descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerDescriptor {
    /// Stable Worker identity.
    pub id: String,
    /// Endpoint base URL.
    pub endpoint: String,
    /// Process incarnation.
    pub incarnation: String,
    /// Package name and version.
    pub package: String,
    /// Supported wire protocol range.
    pub protocol: String,
    /// Capability descriptors.
    pub capabilities: Vec<CapabilityDescriptor>,
    /// Approval policy identifier.
    pub policy: String,
    /// Lease expiry as measured by the Gateway.
    pub lease_expires_at: i64,
    /// Last renewal.
    pub renewed_at: i64,
    /// Live capacity.
    pub capacity: Capacity,
    /// Registration state.
    pub status: String,
}

/// Capability descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    /// Capability identity.
    pub id: String,
    /// Kind, for example `agent` or `tool`.
    pub kind: String,
    /// Human description.
    pub description: String,
    /// Required permissions.
    #[serde(default)]
    pub permissions: Vec<String>,
}
