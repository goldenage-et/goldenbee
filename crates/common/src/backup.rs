//! Portable backup manifest shared by independent owner adapters.
//!
//! A manifest describes one owner's archived native state. Worker and Gateway
//! adapters fill the section they own; neither section turns the other owner's
//! store into a shared database. Manifests never carry credential values: they
//! record secret *references* and the digest of every archived file.

use serde::{Deserialize, Serialize};

/// Manifest format marker. It prevents an unrelated JSON file from being
/// mistaken for a Goldenbee backup.
pub const BACKUP_FORMAT: &str = "goldenbee.backup";

/// Current manifest schema understood by this build.
pub const BACKUP_SCHEMA: u32 = 1;

/// One archived file and its content identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupFile {
    /// Archive-relative path. It never starts with `/` and never contains a
    /// parent component.
    pub path: String,
    /// Purpose of the file, such as `instance-metadata`, `native-data`,
    /// `config`, or `external-asset`.
    pub role: String,
    /// SHA-256 digest as `sha256:<hex>`.
    pub digest: String,
    /// Byte length.
    pub size: u64,
}

/// One verified release reference needed to run a restored Worker.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupRelease {
    /// Package name.
    pub package: String,
    /// Package version.
    pub version: String,
    /// Immutable release manifest digest.
    pub artifact_digest: String,
    /// Executable digest.
    pub binary_digest: String,
    /// Trusted signer identity.
    pub signer: String,
    /// Target triple.
    pub target: String,
    /// Shared runtime version.
    pub runtime: String,
    /// Pinned ADK version.
    pub adk: String,
    /// Lowest protocol version.
    pub protocol_min: String,
    /// Highest protocol version.
    pub protocol_max: String,
    /// Source revision bound into signed metadata.
    pub source_revision: String,
    /// Compiled hooks required by the release.
    pub hooks: Vec<String>,
    /// Native Launcher modes composed by the release.
    pub launcher_modes: Vec<String>,
}

/// One immutable profile revision recorded for restore validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupProfile {
    /// Profile name.
    pub name: String,
    /// Active revision, or zero when inactive.
    pub active_revision: u64,
    /// Immutable revision numbers.
    pub revisions: Vec<u64>,
}

/// One task binding needed to reconcile unfinished work after restore.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupTask {
    /// Stable external task identity.
    pub task_id: String,
    /// Management state label.
    pub state: String,
    /// Session identity.
    pub session_id: String,
    /// Frozen profile name.
    pub profile: String,
    /// Frozen profile revision.
    pub profile_revision: u64,
    /// Frozen asset revision.
    pub asset_revision: u64,
    /// Effective scopes inherited from the submitting authority.
    pub scopes: Vec<String>,
    /// Delegation authority expiry, when bounded.
    #[serde(default)]
    pub authority_expires_at: Option<i64>,
    /// Confirmed native invocation identity.
    #[serde(default)]
    pub native_invocation_id: Option<String>,
    /// Native graph thread identity.
    #[serde(default)]
    pub native_thread_id: Option<String>,
    /// Native A2A task identity.
    #[serde(default)]
    pub native_a2a_task_id: Option<String>,
    /// Package digest active when the task was admitted.
    pub package_digest: String,
}

/// One immutable artifact reference owned by the Worker store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupArtifact {
    /// Artifact identity.
    pub id: String,
    /// Content digest.
    pub digest: String,
    /// Native immutable version.
    pub version: i64,
    /// Byte length.
    pub size: u64,
    /// Finalized flag.
    pub finalized: bool,
}

/// One external configuration or asset directory required by the instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupExternalAsset {
    /// `agents`, `prompts`, or `skills`.
    pub kind: String,
    /// Absolute path recorded on the source host.
    pub original_path: String,
    /// Archive-relative directory, when the contents were copied.
    #[serde(default)]
    pub archived_path: Option<String>,
    /// Archive-relative files that belong to this directory.
    #[serde(default)]
    pub files: Vec<String>,
    /// Aggregate digest over the directory tree.
    pub digest: String,
}

/// Worker-native section of a manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerBackup {
    /// Stable Worker Instance identity.
    pub instance: String,
    /// Source instance directory. It is evidence for path restoration, never a
    /// target path authority.
    pub source_instance: String,
    /// Verified release reference.
    pub release: BackupRelease,
    /// Persisted Worker data schema found in the native state.
    pub data_schema: u32,
    /// Operator configuration path on the source host.
    pub config_path: String,
    /// Archived configuration path.
    pub config_archived: String,
    /// Durable data directory on the source host.
    pub data_dir: String,
    /// Archived data root.
    pub data_archived: String,
    /// Authorized workspace root.
    pub workspace_root: String,
    /// Workspace write authorization.
    pub workspace_write: bool,
    /// Instance grants.
    pub grants: Vec<String>,
    /// Resource ceilings.
    pub limits: crate::Limits,
    /// Credential references. Never values.
    pub secret_refs: Vec<String>,
    /// Signed seed paths materialized for the instance.
    pub seed_assets: Vec<String>,
    /// Immutable profile revisions.
    pub profiles: Vec<BackupProfile>,
    /// Active profile recorded in the lifecycle state.
    #[serde(default)]
    pub active_profile: Option<String>,
    /// Task bindings and their frozen definitions.
    pub tasks: Vec<BackupTask>,
    /// Immutable artifact references.
    pub artifacts: Vec<BackupArtifact>,
    /// External configuration asset directories.
    pub external_assets: Vec<BackupExternalAsset>,
    /// Native Launcher modes enabled by this deployment.
    pub launcher_modes: Vec<String>,
    /// Whether the deployment configuration selects a Gateway connection.
    pub gateway_enabled: bool,
}

/// One execution mapping owned by the Gateway projection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupExecution {
    /// External execution identity.
    pub execution_id: String,
    /// Owning namespace.
    pub namespace: String,
    /// Submitting principal.
    pub caller: String,
    /// Authorized Worker identity.
    pub worker: String,
    /// Observed Worker process identity.
    pub worker_instance: String,
    /// Worker process incarnation frozen when the mapping was accepted.
    pub worker_incarnation: String,
    /// Frozen agent capability.
    pub agent: String,
    /// Authority Worker task handle.
    #[serde(default)]
    pub worker_task_id: Option<String>,
    /// Durable dispatch state label.
    pub dispatch: String,
    /// Canonical request digest.
    pub request_digest: String,
}

/// One approval decision projection. Only references and digests are copied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupApproval {
    /// Approval identity.
    pub id: String,
    /// Bound request digest, when recorded.
    #[serde(default)]
    pub request_digest: Option<String>,
    /// Update digest, when recorded.
    #[serde(default)]
    pub update_digest: Option<String>,
    /// Approval revision.
    pub revision: u64,
    /// Whether registration and routing are enabled.
    pub enabled: bool,
    /// Terminal decommission marker.
    pub decommissioned: bool,
}

/// Gateway-owned section of a manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayBackup {
    /// Stable Gateway projection identity derived from restored content.
    pub gateway_id: String,
    /// Store adapter kind, `file` for the standalone single-writer store.
    pub store_kind: String,
    /// Dispatch store schema.
    pub dispatch_schema: u32,
    /// Registry store schema.
    pub registry_schema: u32,
    /// Registered Worker identities.
    pub workers: Vec<String>,
    /// Team identities as `namespace/name@version`.
    pub teams: Vec<String>,
    /// Approval projections.
    pub approvals: Vec<BackupApproval>,
    /// Execution mappings to Worker-native task handles.
    pub executions: Vec<BackupExecution>,
}

/// One owner's portable backup manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    /// Format marker.
    pub format: String,
    /// Manifest schema.
    pub schema: u32,
    /// Stable backup identity generated by the creating adapter.
    pub id: String,
    /// Creation time in milliseconds since the Unix epoch.
    pub created_at: i64,
    /// Source host name. It is evidence, not authorization.
    pub source_host: String,
    /// Archived files with content digests.
    pub files: Vec<BackupFile>,
    /// Worker owner section.
    #[serde(default)]
    pub worker: Option<WorkerBackup>,
    /// Gateway owner section.
    #[serde(default)]
    pub gateway: Option<GatewayBackup>,
}

impl BackupManifest {
    /// Construct an empty manifest for one owner.
    pub fn new(id: impl Into<String>, source_host: impl Into<String>) -> Self {
        Self {
            format: BACKUP_FORMAT.to_string(),
            schema: BACKUP_SCHEMA,
            id: id.into(),
            created_at: crate::now_ms(),
            source_host: source_host.into(),
            files: Vec::new(),
            worker: None,
            gateway: None,
        }
    }

    /// Whether this build understands the manifest schema.
    pub fn schema_supported(&self) -> bool {
        self.format == BACKUP_FORMAT && self.schema <= BACKUP_SCHEMA
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips_and_defaults_new_sections() {
        let manifest = BackupManifest::new("backup-1", "host-a");
        let bytes = serde_json::to_vec(&manifest).expect("serialize");
        let decoded: BackupManifest = serde_json::from_slice(&bytes).expect("parse");
        assert_eq!(decoded, manifest);
        assert!(decoded.schema_supported());
    }

    #[test]
    fn newer_manifest_schema_is_not_supported() {
        let mut manifest = BackupManifest::new("backup-1", "host-a");
        manifest.schema = BACKUP_SCHEMA + 1;
        assert!(!manifest.schema_supported());
    }
}
