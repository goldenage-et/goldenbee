//! Authorized fleet lifecycle wire contracts between Gateway, SDK, CLI, and
//! deployment host composers.
//!
//! The Gateway never receives host filesystem access through these messages.
//! A host composer receives a verified package digest and an opaque host-local
//! configuration selection; it resolves both inside its own permitted roots.
//! Host credentials and Worker control tokens are never part of a caller
//! response.

use crate::error::ApiError;
use crate::worker::AdminWorkerSummary;
use serde::{Deserialize, Serialize};

/// Host composer management protocol version.
pub const HOST_PROTOCOL_VERSION: &str = "1.0";

/// Supported lifecycle operation: provision one verified instance.
pub const HOST_OPERATION_PROVISION: &str = "provision";
/// Supported lifecycle operation: inspect one instance.
pub const HOST_OPERATION_INSPECT: &str = "inspect";
/// Supported lifecycle operation: upgrade one instance.
pub const HOST_OPERATION_UPGRADE: &str = "upgrade";
/// Supported lifecycle operation: roll back a recoverable activation.
pub const HOST_OPERATION_ROLLBACK: &str = "rollback";
/// Supported lifecycle operation: start a stopped instance.
pub const HOST_OPERATION_START: &str = "start";
/// Supported lifecycle operation: stop a running instance.
pub const HOST_OPERATION_STOP: &str = "stop";
/// Supported lifecycle operation: restart a running or stopped instance.
pub const HOST_OPERATION_RESTART: &str = "restart";

/// Every operation a conforming adapter may advertise.
pub const HOST_OPERATIONS: [&str; 7] = [
    HOST_OPERATION_PROVISION,
    HOST_OPERATION_INSPECT,
    HOST_OPERATION_UPGRADE,
    HOST_OPERATION_ROLLBACK,
    HOST_OPERATION_START,
    HOST_OPERATION_STOP,
    HOST_OPERATION_RESTART,
];

/// Resource ceilings selected for one instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLimits {
    /// Active execution ceiling.
    pub max_active: usize,
    /// Pending queue ceiling.
    pub max_queue: usize,
    /// Per-principal admitted work ceiling.
    pub per_principal: usize,
}

impl Default for HostLimits {
    fn default() -> Self {
        Self {
            max_active: 4,
            max_queue: 32,
            per_principal: 8,
        }
    }
}

/// Authorized profile selection applied by the host composer after provisioning.
///
/// The profile remains user-owned configuration. Creating it does not change
/// the installed package or grant a capability the package does not provide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProfileSelection {
    /// Profile name.
    pub name: String,
    /// Create a revision before activation. When false an existing profile is
    /// activated by name.
    #[serde(default)]
    pub create: bool,
    /// Instruction for a created revision.
    #[serde(default)]
    pub instruction: Option<String>,
    /// Native agent members.
    #[serde(default)]
    pub members: Vec<String>,
    /// Package capabilities selected by the revision.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Profile grants, intersected with the instance and host policy.
    #[serde(default)]
    pub grants: Vec<String>,
}

/// Operator request to provision one instance from a verified package digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProvisionRequest {
    /// Caller-supplied idempotency identity. A repeated operation returns the
    /// original result instead of creating another instance.
    pub operation_id: String,
    /// Stable host-local instance identity.
    pub instance: String,
    /// Gateway Worker identity bound to the provisioned instance.
    pub worker: String,
    /// Verified package digest, for example `sha256:<64 hex>`.
    pub release_digest: String,
    /// Host-local operator configuration selection. The adapter validates it
    /// against its own permitted roots; the Gateway never reads it.
    #[serde(default)]
    pub config: Option<String>,
    /// Host-local authorized workspace selection.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Whether the workspace is read-only.
    #[serde(default)]
    pub read_only: bool,
    /// Instance grants. The adapter intersects them with its host policy.
    #[serde(default)]
    pub grants: Vec<String>,
    /// Resource ceilings.
    #[serde(default)]
    pub limits: HostLimits,
    /// Credential references. Never secret values.
    #[serde(default)]
    pub secret_refs: Vec<String>,
    /// Profile created or activated after provisioning.
    #[serde(default)]
    pub profile: Option<HostProfileSelection>,
    /// Requested Worker listener address. The adapter validates locality.
    #[serde(default)]
    pub addr: Option<String>,
    /// Narrow Worker control token to bind to this instance. The Gateway
    /// generates one when omitted. Never a host credential.
    #[serde(default)]
    pub control_token: Option<String>,
}

impl HostProvisionRequest {
    /// Digest over the operation identity, excluding the control token.
    pub fn digest(&self) -> String {
        crate::canonical_digest(&serde_json::json!({
            "kind": HOST_OPERATION_PROVISION,
            "operation_id": self.operation_id,
            "instance": self.instance,
            "worker": self.worker,
            "release_digest": self.release_digest,
            "config": self.config,
            "workspace": self.workspace,
            "read_only": self.read_only,
            "grants": self.grants,
            "limits": self.limits,
            "secret_refs": self.secret_refs,
            "profile": self.profile,
            "addr": self.addr,
        }))
    }
}

/// Work settlement policy before executable replacement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostWorkPolicy {
    /// Stop admission and wait for admitted work to finish.
    #[default]
    Drain,
    /// Refuse replacement while any admitted or queued work exists.
    Pin,
    /// Drain admitted work and resume recoverable queued work on the candidate.
    Migrate,
}

impl HostWorkPolicy {
    /// Stable wire label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Drain => "drain",
            Self::Pin => "pin",
            Self::Migrate => "migrate",
        }
    }
}

/// Managed upgrade request for one provisioned instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostUpgradeRequest {
    /// Idempotent lifecycle operation identity.
    pub operation_id: String,
    /// Verified candidate package digest.
    pub release_digest: String,
    /// Added permissions the operator explicitly authorizes.
    #[serde(default)]
    pub authorized_permissions: Vec<String>,
    /// Work settlement policy.
    #[serde(default)]
    pub work_policy: HostWorkPolicy,
    /// Input used for the post-activation capability check.
    #[serde(default)]
    pub verify_input: Option<String>,
    /// Candidate listener address. Defaults to the previous address.
    #[serde(default)]
    pub addr: Option<String>,
}

impl HostUpgradeRequest {
    /// Digest over this upgrade request.
    pub fn digest(&self) -> String {
        crate::canonical_digest(&serde_json::json!({
            "kind": HOST_OPERATION_UPGRADE,
            "operation_id": self.operation_id,
            "release_digest": self.release_digest,
            "authorized_permissions": self.authorized_permissions,
            "work_policy": self.work_policy,
            "verify_input": self.verify_input,
            "addr": self.addr,
        }))
    }
}

/// Managed rollback request for one instance with a recoverable activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRollbackRequest {
    /// Idempotent lifecycle operation identity.
    pub operation_id: String,
}

impl HostRollbackRequest {
    /// Digest over this rollback request.
    pub fn digest(&self) -> String {
        crate::canonical_digest(&serde_json::json!({
            "kind": HOST_OPERATION_ROLLBACK,
            "operation_id": self.operation_id,
        }))
    }
}

/// Target, protocol, runtime, and ADK compatibility of one installed instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCompatibility {
    /// Host target triple.
    pub target: String,
    /// Package protocol declaration.
    pub protocol: String,
    /// Shared Worker runtime declaration.
    pub runtime: String,
    /// Pinned ADK declaration.
    pub adk: String,
    /// Whether the installed release is compatible with this host.
    pub compatible: bool,
    /// Redacted incompatibilities.
    #[serde(default)]
    pub issues: Vec<String>,
}

/// Last explicit trust decision applied on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostTrustView {
    /// `continue`, `restrict`, or `quarantine`.
    pub decision: String,
    /// Redacted reason.
    pub reason: String,
}

/// Host-owned inspection of one instance. It deliberately omits release,
/// configuration, data, and workspace filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInstanceView {
    /// Host identity.
    pub host: String,
    /// Stable instance identity.
    pub instance: String,
    /// Package name.
    pub package: String,
    /// Package version.
    pub version: String,
    /// Immutable release digest.
    pub artifact_digest: String,
    /// Executable digest.
    pub binary_digest: String,
    /// Trusted signer.
    pub signer: String,
    /// Package target.
    pub target: String,
    /// Package protocol.
    pub protocol: String,
    /// `configuring`, `running`, `upgrading`, `stopped`, `faulted`, or
    /// `quarantined`.
    pub lifecycle: String,
    /// `running`, `exited`, or `unknown`.
    pub process_health: String,
    /// `idle`, `executing`, or `unknown`.
    pub task_activity: String,
    /// Worker runtime phase from its status route.
    #[serde(default)]
    pub runtime_phase: Option<String>,
    /// Current process incarnation.
    #[serde(default)]
    pub incarnation: Option<String>,
    /// Active profile name.
    #[serde(default)]
    pub active_profile: Option<String>,
    /// Worker listener endpoint.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Instance grants.
    #[serde(default)]
    pub grants: Vec<String>,
    /// Resource ceilings.
    pub limits: HostLimits,
    /// Workspace write authorization.
    pub workspace_write: bool,
    /// Credential references. Never secret values.
    #[serde(default)]
    pub secret_refs: Vec<String>,
    /// Whether the operator supplied the configuration file.
    pub operator_config: bool,
    /// Last explicit trust decision.
    #[serde(default)]
    pub trust: Option<HostTrustView>,
    /// Package and host compatibility.
    pub compatibility: HostCompatibility,
}

/// Typed non-blocking finding from an upgrade plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFinding {
    /// Stable code.
    pub code: String,
    /// Redacted actionable message.
    pub message: String,
}

/// Upgrade evidence carried back through the Gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostUpgradeSummary {
    /// Active digest before activation.
    pub current_digest: String,
    /// Candidate digest.
    pub candidate_digest: String,
    /// Permissions the candidate adds.
    #[serde(default)]
    pub permissions_added: Vec<String>,
    /// Permissions the candidate removes.
    #[serde(default)]
    pub permissions_removed: Vec<String>,
    /// Blocking findings from the plan.
    #[serde(default)]
    pub blocked: Vec<HostFinding>,
    /// Explicit permission authorization was satisfied.
    pub authorization_satisfied: bool,
    /// Operator configuration and external asset paths preflighted.
    pub config_preserved: bool,
    /// Persisted schema compatibility.
    pub schema_status: String,
    /// Work settlement action.
    pub work_action: String,
    /// Native launcher modes verified by the candidate probe.
    #[serde(default)]
    pub launcher_modes: Vec<String>,
}

/// Rollback evidence. Binary and data restoration are reported separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRecoverySummary {
    /// Previous binary record restored.
    pub binary_restored: bool,
    /// Persisted data restored from the snapshot.
    pub data_restored: bool,
    /// Failed data could not be restored and was quarantined.
    pub quarantine: bool,
    /// Journal status after recovery.
    pub journal_status: String,
}

/// Result of one managed lifecycle operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostOperationOutcome {
    /// Idempotent operation identity.
    pub operation_id: String,
    /// `provision`, `upgrade`, or `rollback`.
    pub kind: String,
    /// Host identity.
    pub host: String,
    /// Instance identity.
    pub instance: String,
    /// `succeeded`, `recovered`, or `quarantined`.
    pub status: String,
    /// Resulting host inspection.
    #[serde(default)]
    pub view: Option<HostInstanceView>,
    /// Gateway registration projection after reconciliation.
    #[serde(default)]
    pub worker: Option<AdminWorkerSummary>,
    /// Upgrade evidence.
    #[serde(default)]
    pub upgrade: Option<HostUpgradeSummary>,
    /// Rollback evidence.
    #[serde(default)]
    pub recovery: Option<HostRecoverySummary>,
    /// The adapter or Gateway replayed an already recorded operation.
    #[serde(default)]
    pub replayed: bool,
}

/// One registered host composer and its supported operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDescriptor {
    /// Host identity.
    pub host: String,
    /// Adapter protocol version.
    pub protocol: String,
    /// Adapter package version.
    pub adapter_version: String,
    /// Supported operation names.
    #[serde(default)]
    pub supported: Vec<String>,
    /// Whether the adapter answered this Gateway.
    #[serde(default)]
    pub reachable: bool,
    /// Redacted unreachability reason.
    #[serde(default)]
    pub error: Option<String>,
}

/// Gateway inspection combining host evidence and the Worker registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInspection {
    /// Host identity.
    pub host: String,
    /// Host-local inspection.
    pub view: HostInstanceView,
    /// Gateway registration projection, when a Worker identity is bound.
    #[serde(default)]
    pub worker: Option<AdminWorkerSummary>,
}

/// Error returned by a host composer for one refused operation.
pub type HostError = ApiError;

/// One package source the Gateway may install on its own host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageSource {
    /// A release directory already readable by the Gateway host.
    Folder {
        /// Host-local path. The Gateway validates it against its own roots.
        path: String,
    },
    /// A signed release archive downloaded by the Gateway host.
    Url {
        /// HTTPS base URL or archive URL.
        url: String,
    },
}

/// Caller request to install a verified package into the local cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInstallRequest {
    /// Folder or URL source.
    pub source: PackageSource,
    /// Package name. Required when the source contains more than one package.
    #[serde(default)]
    pub package: Option<String>,
    /// Expected immutable manifest digest.
    #[serde(default)]
    pub expected_digest: Option<String>,
}

/// One verified package in the local cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageSummary {
    /// Package name.
    pub package: String,
    /// Package version.
    pub version: String,
    /// Immutable manifest digest.
    pub artifact_digest: String,
    /// Trusted signer identity.
    pub signer: String,
    /// Target triple.
    pub target: String,
}

/// Result of one package installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInstallOutcome {
    /// Installed package.
    pub package: PackageSummary,
    /// Whether this call changed the cache.
    pub installed_now: bool,
}

/// Verified package cache listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageList {
    /// Verified packages.
    #[serde(default)]
    pub packages: Vec<PackageSummary>,
}

/// Bounded local instance log tail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceLogs {
    /// Instance identity.
    pub instance: String,
    /// Log lines, oldest first.
    #[serde(default)]
    pub lines: Vec<String>,
    /// Whether older lines were omitted.
    #[serde(default)]
    pub truncated: bool,
}
