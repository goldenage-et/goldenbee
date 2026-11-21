//! Shared Goldenbee wire types and helpers.
//!
//! These types are framework-neutral: they never expose native ADK types,
//! storage schemas, or transport implementation details. Management routes are
//! versioned independently from the pinned ADK release.
//!
//! Each module owns one wire concept. This root is a facade that re-exports
//! every public contract type and helper at its established path.

pub mod artifact;
pub mod backup;
pub mod checkpoint;
pub mod command;
pub mod control;
pub mod delegation;
pub mod diagnostics;
pub mod digest;
pub mod error;
pub mod gateway;
pub mod health;
pub mod host;
pub mod identity;
pub mod limits;
pub mod memory;
pub mod package;
pub mod plan;
pub mod profile;
pub mod providers;
pub mod redaction;
pub mod session;
pub mod socket;
pub mod stream;
pub mod task;
pub mod team;
pub mod telemetry;
pub mod worker;

#[cfg(test)]
mod tests;

pub use artifact::{Artifact, ArtifactGrant};
pub use backup::{
    BackupApproval, BackupArtifact, BackupExecution, BackupFile, BackupManifest, BackupProfile,
    BackupRelease, BackupTask, GatewayBackup, WorkerBackup, BACKUP_FORMAT, BACKUP_SCHEMA,
};
pub use checkpoint::{
    CheckpointDiff, CheckpointFileView, CheckpointRestoreOutcome, CheckpointRestoreRequest,
    CheckpointView, DiffHunk, DiffLine, FileDiff,
};
pub use command::{
    kinds as command_kinds, CommandArgument, CommandDescriptor, CommandRequest, CommandResponse,
    SkillDescriptor,
};
pub use control::{
    ControlAuditEntry, ControlCommand, ControlCommandRequest, ControlCommandResponse,
    ControlHandBackRequest, ControlLease, ControlLeaseRequest, ControlOwner, ControlPage,
    ControlStatus, CONTROL_SCHEMA_VERSION,
};
pub use diagnostics::{
    DiagnosticApproval, DiagnosticBundle, DiagnosticCorrelation, DiagnosticDelegation,
    DiagnosticFinding, DiagnosticStorage, DiagnosticTask,
};
pub use digest::{bytes_digest, canonical_digest};
pub use error::{ApiError, ContractResult};
pub use gateway::{
    GatewayApproval, GatewayConnectivity, GatewayHealth, GatewaySubmitRequest,
    GatewaySubmitResponse, GatewayTask,
};
pub use health::{Health, LifecyclePhase, TraceSample};
pub use host::{
    HostCompatibility, HostDescriptor, HostFinding, HostInspection, HostInstanceView, HostLimits,
    HostOperationOutcome, HostProfileSelection, HostProvisionRequest, HostRecoverySummary,
    HostRollbackRequest, HostTrustView, HostUpgradeRequest, HostUpgradeSummary, HostWorkPolicy,
    InstanceLogs, PackageInstallOutcome, PackageInstallRequest, PackageList, PackageSource,
    PackageSummary, HOST_OPERATIONS, HOST_OPERATION_INSPECT, HOST_OPERATION_PROVISION,
    HOST_OPERATION_RESTART, HOST_OPERATION_ROLLBACK, HOST_OPERATION_START, HOST_OPERATION_STOP,
    HOST_OPERATION_UPGRADE, HOST_PROTOCOL_VERSION,
};
pub use identity::{scopes, Principal};
pub use limits::{Capacity, Limits, Usage};
pub use memory::{KnowledgeFact, MemoryItem};
pub use package::{
    protocol_in_range, BuildEvidence, Instance, PackageManifest, Release, ReleaseMetadata,
    SeedAsset, RELEASE_METADATA_SCHEMA,
};
pub use plan::{
    CreatePlanRequest, CriterionEvidence, EscalatePlanRequest, PlanChildRequest,
    PlanControlRequest, PlanCriterion,
};
pub use profile::{
    Approval, ApprovalAction, ApprovalDecision, ContextCompaction, CreateProfileRequest,
    DeletedProfile, Profile, ProfileRevision, ProfileVersionRequest, ReviseProfileRequest,
};
pub use providers::{
    adapter_discovers_models, CreatePairCodeRequest, CreateProviderRequest, PairCodeResponse,
    ProviderAssignment, ProviderBundle, ProviderCredentialSummary, ProviderEnvelope,
    ProviderMetadata, SetProviderCredentialRequest, UpdateProviderRequest, WorkerEnrollRequest,
    WorkerEnrollResponse, MAX_DISCOVERED_MODELS, PAIR_CODE_TTL_MS, PROVIDER_ADAPTERS,
    PROVIDER_ENVELOPE_INFO, PROVIDER_ENVELOPE_TTL_MS,
};
pub use redaction::redact;
pub use session::{
    SessionCompaction, SessionForkRequest, SessionRenameRequest, SessionTranscript, SessionTurn,
    SessionView,
};
pub use socket::{
    SocketCommand, SocketCommandFrame, SocketOutcome, SocketScope, SocketServerFrame, SocketTicket,
    SOCKET_HEARTBEAT_MS, SOCKET_IDLE_TIMEOUT_MS, SOCKET_MAX_FRAME_BYTES, SOCKET_MAX_SUBSCRIPTIONS,
    SOCKET_MAX_TICKETS, SOCKET_PROTOCOL_VERSION, SOCKET_SEND_TIMEOUT_MS, SOCKET_SUBSCRIBER_QUEUE,
    SOCKET_TICKET_TTL_MS,
};
pub use stream::{
    parse_cursor, parse_limit, StreamEvent, StreamPage, EVENT_SCHEMA_VERSION, MAX_EVENT_PAGE,
};
pub use task::{ResolvedModel, SubmitTaskRequest, SubmitTaskResponse, Task, TaskState};
pub use team::{
    CreateTeamRequest, Team, TeamMember, TeamMemberGrant, TeamRoster, TeamSpec, TeamVersionRequest,
    UpdateTeamRequest,
};
pub use telemetry::{
    is_bounded_label, trace_id, traceparent_id, MetricQuality, MetricSample, BOUNDED_LABEL_KEYS,
};
pub use worker::{
    AddWorkerRequest, AddWorkerResponse, AdminWorkerSummary, CapabilityDescriptor,
    RegistryAuditEntry, UpdateWorkerRequest, WorkerDescriptor, WorkerDiscoveryFilter,
    WorkerIncarnation, WorkerLease, WorkerLeaseRequest, WorkerRegistrationRequest, WorkerSummary,
    LOCALITY_LOCAL, LOCALITY_REMOTE,
};

/// Wire protocol version implemented by this crate.
pub const PROTOCOL_VERSION: &str = "1.0";

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
