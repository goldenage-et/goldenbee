//! Durable single-node state for harness-owned metadata.

use common::{
    ApiError, Approval, Artifact, ArtifactGrant, Instance, Profile, Release, StreamEvent, Task,
    Team, TraceSample, WorkerDescriptor,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

/// Current persisted-data schema. Bump only for migrations with an explicit
/// compatibility decision.
///
/// Schema 2 removed the unused harness role/text session store. Native ADK
/// session events are the canonical history and live in the Worker Instance's
/// native session store.
/// Schema 3 stores native artifact scope and provenance instead of a harness
/// filesystem path, and removes the unused harness memory table. Artifact bytes
/// remain owned by the pinned native `ArtifactService`; long-term memory is the
/// pinned native SQLite store below the Worker Instance's data directory.
/// Schema 4 removes the harness knowledge-observation map: the pinned native
/// graph store with the runtime provenance adapter is the only writable graph
/// owner. Observations persisted under schema 3 are quarantined next to the
/// state file, never silently discarded.
///
/// Schema 5 adds the file-checkpoint manifest for turn-boundary snapshots.
/// Snapshot bytes remain content-addressed files below the data directory; the
/// manifest records only digests, sizes, workspace-relative paths, and restore
/// evidence. The field is `#[serde(default)]`, so a schema-4 store opens with
/// an empty manifest and no canonical data is migrated or dropped.
pub const DATA_SCHEMA: u32 = 5;

/// Audit record. Contains no secret values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Monotonic audit sequence.
    pub seq: u64,
    /// Time.
    pub at: i64,
    /// Principal that performed the action.
    pub principal: String,
    /// Action name.
    pub action: String,
    /// Target resource identity.
    pub target: String,
    /// Outcome.
    pub outcome: String,
    /// Redacted detail.
    pub detail: String,
    /// Owning process instance.
    #[serde(default)]
    pub source: String,
    /// Effective policy/configuration revision.
    #[serde(default)]
    pub policy_revision: u64,
    /// Trace correlation, retained independently of sampled telemetry.
    #[serde(default)]
    pub correlation: String,
}

/// Durable progress for one owner-coordinated retention or erasure run.
///
/// A cleanup job is written before the first destructive step and updated after
/// each owning store reports its outcome. A restart resumes the unfinished
/// stages instead of treating a partial cleanup as complete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupJob {
    /// Stable job identity derived from the operation and target scope.
    pub id: String,
    /// `retention` or `erasure`.
    pub kind: String,
    /// Owning namespace.
    pub namespace: String,
    /// Erased principal for `erasure`; empty for instance retention.
    pub target: String,
    /// Creation time.
    pub started_at: i64,
    /// Last progress time.
    pub updated_at: i64,
    /// Stages that completed durably.
    pub completed: Vec<String>,
    /// Stages that reported an owning-store limitation instead of success.
    pub blocked: Vec<String>,
    /// Removed record counts by owning data class.
    pub removed: BTreeMap<String, usize>,
    /// Obligations retained because a live reference or recovery need exists.
    pub retained: Vec<RetainedObligation>,
}

/// One record intentionally preserved by retention or erasure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetainedObligation {
    /// Data class, such as `task`, `checkpoint`, `artifact`, or `audit`.
    pub class: String,
    /// Stable identity of the retained record.
    pub id: String,
    /// Machine-readable reason, such as `unfinished-task`.
    pub reason: String,
}

/// Ingested knowledge document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentRecord {
    /// Document identity.
    pub id: String,
    /// Namespace.
    pub namespace: String,
    /// Principal that ingested the document.
    #[serde(default)]
    pub owner: String,
    /// Title.
    pub title: String,
    /// Real source descriptor.
    pub source: String,
    /// Extracted text.
    pub content: String,
    /// Content digest.
    pub digest: String,
    /// Allowed principals. Empty means the namespace.
    pub allowed: Vec<String>,
    /// Creation time.
    pub created_at: i64,
}

/// Persisted side-effect operation.
///
/// The record is owned by the Worker Instance that executes the effect. It is
/// evidence for reconciliation; it is not a retry token that a Client can
/// present, and no shared wire type can write it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRecord {
    /// Operation identity.
    pub id: String,
    /// Task identity.
    pub task_id: String,
    /// Exact operation digest. It excludes the injected idempotency key.
    pub digest: String,
    /// `reserved`, `committed`, `absent`, `failed`, or `unknown`.
    pub status: String,
    /// External reference, when confirmed. Redacted before persistence.
    pub external_ref: Option<String>,
    /// Redacted receipt or reconciliation detail.
    pub receipt: Option<String>,
    /// Update time.
    pub updated_at: i64,
    /// Dispatch attempts recorded for this operation identity.
    #[serde(default)]
    pub attempt: u32,
    /// Native invocation that dispatched the effect, when known.
    #[serde(default)]
    pub invocation_id: Option<String>,
    /// Registered native tool that owns the effect.
    #[serde(default)]
    pub tool: String,
    /// Stable external idempotency key. It never changes across retries.
    #[serde(default)]
    pub idempotency_key: String,
    /// Documented authorization and retry policy used for this attempt.
    #[serde(default)]
    pub policy: String,
}

/// One content-addressed snapshot blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    /// `sha256:<hex>` content digest. Empty when the file was absent.
    pub digest: String,
    /// Byte length.
    pub bytes: u64,
    /// Whether the file existed at capture time.
    pub present: bool,
}

/// One file captured by a turn-boundary checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFileRecord {
    /// Workspace-relative path. Never an absolute host path.
    pub path: String,
    /// Content before the turn.
    pub before: BlobRef,
    /// Content after the turn; absent while the turn is open or when the
    /// post-image could not be captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<BlobRef>,
}

/// One applied restore recorded as idempotency and audit evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRestore {
    /// Deterministic restore identity for retry convergence.
    pub id: String,
    /// `rewind`, `undo`, or `restore`.
    pub mode: String,
    /// Selected workspace-relative paths.
    pub files: Vec<String>,
    /// Restore time.
    pub at: i64,
}

/// Durable manifest entry for one task turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRecord {
    /// Stable checkpoint identity derived from task and turn.
    pub id: String,
    /// Owning task.
    pub task_id: String,
    /// Owning namespace.
    pub namespace: String,
    /// Submitting principal.
    pub caller: String,
    /// Native session identity.
    pub session_id: String,
    /// Native invocation that bounds the turn.
    pub turn: String,
    /// Task attempt that produced the checkpoint.
    pub attempt: u32,
    /// `open`, `closed`, or `partial`.
    pub status: String,
    /// Creation time.
    pub created_at: i64,
    /// Last update time.
    pub updated_at: i64,
    /// Captured files.
    pub files: Vec<CheckpointFileRecord>,
    /// Applied restores, keyed by deterministic restore id.
    #[serde(default)]
    pub restores: Vec<CheckpointRestore>,
}

/// Native scope used to address one immutable artifact version. These values
/// are internal identifiers, not client-supplied paths or URLs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactScope {
    /// Package application identity used by the native artifact service.
    pub app_name: String,
    /// Authenticated native user identity.
    pub user_id: String,
    /// Native session identity.
    pub session_id: String,
    /// Native artifact file name.
    pub file_name: String,
}

/// Persisted artifact metadata plus its native scope. Content bytes remain in
/// the pinned native [`adk_artifact::ArtifactService`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    /// Public metadata.
    pub artifact: Artifact,
    /// Native address of the immutable version.
    pub scope: ArtifactScope,
}

/// Persisted release archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseRecord {
    /// Metadata.
    pub release: Release,
    /// Base64 manifest bytes.
    pub manifest: String,
    /// Base64 signature over the manifest digest.
    pub signature: String,
    /// Publication time.
    pub published_at: i64,
}

/// Everything the harness owns in its single-node store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    /// Persisted-data schema.
    pub schema: u32,
    /// Token bindings.
    pub tokens: BTreeMap<String, common::Principal>,
    /// Revocation generation.
    pub revocation_generation: u64,
    /// Task projections.
    pub tasks: BTreeMap<String, Task>,
    /// Idempotency index: namespace|caller|key -> task.
    pub task_index: BTreeMap<String, String>,
    /// Durable event streams.
    pub events: BTreeMap<String, Vec<StreamEvent>>,
    /// Profiles.
    pub profiles: BTreeMap<String, Profile>,
    /// Approvals.
    pub approvals: BTreeMap<String, Approval>,
    /// Artifacts.
    pub artifacts: BTreeMap<String, ArtifactRecord>,
    /// Artifact grants.
    pub grants: BTreeMap<String, ArtifactGrant>,
    /// Registered Workers.
    pub workers: BTreeMap<String, WorkerDescriptor>,
    /// Teams.
    pub teams: BTreeMap<String, Team>,
    /// Ingested documents.
    pub documents: BTreeMap<String, DocumentRecord>,
    /// Side-effect operations.
    pub operations: BTreeMap<String, OperationRecord>,
    /// Installed and published releases.
    pub releases: BTreeMap<String, ReleaseRecord>,
    /// Revoked release digests.
    pub revocations: std::collections::BTreeSet<String>,
    /// Worker instances.
    pub instances: BTreeMap<String, Instance>,
    /// Last verified static peer identities and effective capabilities.
    #[serde(default)]
    pub peer_records: BTreeMap<String, crate::peer::PeerRecord>,
    /// Durable parent-to-child native A2A delegation bindings.
    #[serde(default)]
    pub peer_delegations: BTreeMap<String, crate::peer::PeerDelegation>,
    /// Durable Master execution plans and their observed evidence.
    #[serde(default)]
    pub plans: BTreeMap<String, crate::master::MasterPlan>,
    /// Per-task plans recorded by the planning tool, keyed by task id.
    #[serde(default)]
    pub task_plans: BTreeMap<String, common::TaskPlan>,
    /// Turn-boundary file checkpoint manifests. Snapshot bytes live in
    /// content-addressed files below the data directory.
    #[serde(default)]
    pub checkpoints: BTreeMap<String, CheckpointRecord>,
    /// Correlated trace samples.
    pub traces: Vec<TraceSample>,
    /// Security audit.
    pub audit: Vec<AuditRecord>,
    /// Durable progress for interrupted retention or erasure.
    #[serde(default)]
    pub cleanup: BTreeMap<String, CleanupJob>,
    /// Monotonic counters.
    pub counters: Counters,
}

/// Monotonic identifiers derived from persisted state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Counters {
    /// Audit sequence.
    pub audit: u64,
    /// Stream sequence fallback.
    pub stream: u64,
    /// Native/invocation sequence.
    pub invocation: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema: DATA_SCHEMA,
            tokens: BTreeMap::new(),
            revocation_generation: 0,
            tasks: BTreeMap::new(),
            task_index: BTreeMap::new(),
            events: BTreeMap::new(),
            profiles: BTreeMap::new(),
            approvals: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            grants: BTreeMap::new(),
            workers: BTreeMap::new(),
            teams: BTreeMap::new(),
            documents: BTreeMap::new(),
            operations: BTreeMap::new(),
            releases: BTreeMap::new(),
            revocations: std::collections::BTreeSet::new(),
            instances: BTreeMap::new(),
            peer_records: BTreeMap::new(),
            peer_delegations: BTreeMap::new(),
            plans: BTreeMap::new(),
            task_plans: BTreeMap::new(),
            checkpoints: BTreeMap::new(),
            traces: Vec::new(),
            audit: Vec::new(),
            cleanup: BTreeMap::new(),
            counters: Counters::default(),
        }
    }
}

/// File-backed state with a single writer lock.
pub struct Store {
    path: PathBuf,
    state: Mutex<State>,
}

impl Store {
    /// Open the store, creating the parent directory. A corrupt or newer store
    /// is reported, never silently replaced.
    pub async fn open(path: impl Into<PathBuf>, initial: State) -> Result<Self, ApiError> {
        let path = path.into();
        let mut quarantine = None;
        let state = if path.exists() {
            let bytes = std::fs::read(&path).map_err(|error| {
                ApiError::new("storage.unavailable", format!("cannot read state: {error}"))
            })?;
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
                ApiError::new("storage.corrupt", format!("cannot parse state: {error}"))
            })?;
            let schema = value
                .get("schema")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if schema < u64::from(DATA_SCHEMA) {
                if let Some(observations) = value
                    .get("observations")
                    .and_then(serde_json::Value::as_object)
                    .filter(|observations| !observations.is_empty())
                {
                    quarantine = Some(serde_json::json!({
                        "schema": schema,
                        "reason": "harness knowledge observations moved to the native graph store",
                        "observations": observations,
                    }));
                }
            }
            let state: State = serde_json::from_value(value).map_err(|error| {
                ApiError::new("storage.corrupt", format!("cannot parse state: {error}"))
            })?;
            if state.schema > DATA_SCHEMA {
                return Err(ApiError::new(
                    "storage.schema_unsupported",
                    format!(
                        "persisted schema {} is newer than supported {}",
                        state.schema, DATA_SCHEMA
                    ),
                ));
            }
            state
        } else {
            initial
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                ApiError::new(
                    "storage.unavailable",
                    format!("cannot create data dir: {error}"),
                )
            })?;
        }
        if let Some(observations) = quarantine {
            let quarantine_path = path.with_file_name("state.observations-quarantine.json");
            let bytes = serde_json::to_vec_pretty(&observations).map_err(|error| {
                ApiError::new(
                    "storage.unavailable",
                    format!("cannot serialize quarantined observations: {error}"),
                )
            })?;
            atomic_write(&quarantine_path, &bytes)?;
        }
        let store = Self {
            path,
            state: Mutex::new(state),
        };
        store.persist_current().await?;
        Ok(store)
    }

    /// Path of the backing file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read a value under the store lock.
    pub async fn read<R>(&self, f: impl FnOnce(&State) -> R) -> R {
        let state = self.state.lock().await;
        f(&state)
    }

    /// Atomically mutate and persist. The in-memory state changes only after a
    /// successful write, so persistence failures cannot produce false
    /// acceptance.
    pub async fn write<R>(
        &self,
        f: impl FnOnce(&mut State) -> Result<R, ApiError>,
    ) -> Result<R, ApiError> {
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();
        let result = f(&mut next)?;
        if let Some(error) = crate::core::fault::persistence_failure() {
            return Err(error);
        }
        let bytes = serde_json::to_vec(&next).map_err(|error| {
            ApiError::new(
                "storage.unavailable",
                format!("cannot serialize state: {error}"),
            )
        })?;
        atomic_write(&self.path, &bytes)?;
        *guard = next;
        Ok(result)
    }

    /// Replace the whole state, used by restore.
    pub async fn replace(&self, state: State) -> Result<(), ApiError> {
        if state.schema > DATA_SCHEMA {
            return Err(ApiError::new(
                "storage.schema_unsupported",
                "backup was written by a newer schema",
            ));
        }
        let bytes = serde_json::to_vec(&state).map_err(|error| {
            ApiError::new(
                "storage.unavailable",
                format!("cannot serialize state: {error}"),
            )
        })?;
        atomic_write(&self.path, &bytes)?;
        *self.state.lock().await = state;
        Ok(())
    }

    /// Export a backup without changing live state.
    pub async fn export(&self) -> State {
        self.state.lock().await.clone()
    }

    async fn persist_current(&self) -> Result<(), ApiError> {
        let state = self.state.lock().await;
        let bytes = serde_json::to_vec(&*state).map_err(|error| {
            ApiError::new(
                "storage.unavailable",
                format!("cannot serialize state: {error}"),
            )
        })?;
        atomic_write(&self.path, &bytes)
    }
}

/// Replace one file through a synced temporary file.
///
/// The temporary file is flushed and synced before the rename, then the parent
/// directory is synced so the new directory entry is durable on Unix. This
/// closes the process-restart durability gap for committed writes; it is not a
/// power-loss test by itself. The temporary file is removed on any failure
/// before the rename.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ApiError> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(|error| {
        ApiError::retryable(
            "storage.unavailable",
            format!("cannot persist state: {error}"),
        )
    })
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_audit(seq: u64) -> State {
        State {
            counters: Counters {
                audit: seq,
                ..Counters::default()
            },
            ..State::default()
        }
    }

    #[tokio::test]
    async fn committed_write_survives_reopen() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let path = directory.path().join("state.json");
        {
            let store = Store::open(&path, State::default())
                .await
                .expect("store opens");
            store
                .write(|state| {
                    state.counters.audit = 7;
                    Ok(())
                })
                .await
                .expect("write commits");
        }
        let reopened = Store::open(&path, State::default())
            .await
            .expect("store reopens");
        assert_eq!(reopened.read(|state| state.counters.audit).await, 7);
    }

    #[tokio::test]
    async fn failed_mutation_keeps_previous_state() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let path = directory.path().join("state.json");
        let store = Store::open(&path, State::default())
            .await
            .expect("store opens");
        store
            .write(|state| {
                state.counters.audit = 3;
                Ok(())
            })
            .await
            .expect("first write commits");

        let error = store
            .write(|state| {
                state.counters.audit = 99;
                Err::<(), ApiError>(ApiError::new("test.rejected", "mutation rejected"))
            })
            .await
            .expect_err("mutation fails");
        assert_eq!(error.code, "test.rejected");
        assert_eq!(store.read(|state| state.counters.audit).await, 3);

        let reopened = Store::open(&path, State::default())
            .await
            .expect("store reopens");
        assert_eq!(reopened.read(|state| state.counters.audit).await, 3);
    }

    #[tokio::test]
    async fn successful_writes_leave_no_temporary_files() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let path = directory.path().join("state.json");
        let store = Store::open(&path, State::default())
            .await
            .expect("store opens");
        store
            .write(|state| {
                state.counters.audit = 1;
                Ok(())
            })
            .await
            .expect("write commits");

        let names: Vec<_> = std::fs::read_dir(directory.path())
            .expect("data directory")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("state.json")]);
    }

    #[tokio::test]
    async fn legacy_observations_are_quarantined_not_silently_discarded() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let path = directory.path().join("state.json");
        let mut value = serde_json::to_value(State::default()).expect("state JSON");
        value["schema"] = serde_json::json!(3);
        value["observations"] = serde_json::json!({
            "obs-1": {
                "id": "obs-1",
                "namespace": "local",
                "subject": "Ada",
                "predicate": "works_at",
                "object": "Acme",
                "source": "doc://1",
                "document_id": "document-1",
                "valid_from": 1,
                "valid_to": null,
                "deleted": false
            }
        });
        std::fs::write(&path, serde_json::to_vec(&value).expect("legacy state"))
            .expect("legacy state file");

        let store = Store::open(&path, State::default())
            .await
            .expect("legacy state opens");
        let persisted = std::fs::read(&path).expect("persisted state");
        let persisted: serde_json::Value =
            serde_json::from_slice(&persisted).expect("persisted JSON");
        assert!(
            persisted.get("observations").is_none(),
            "the harness observation map is gone"
        );
        assert_eq!(store.read(|state| state.schema).await, 3);

        let quarantine = std::fs::read(directory.path().join("state.observations-quarantine.json"))
            .expect("quarantine file exists");
        let quarantine: serde_json::Value =
            serde_json::from_slice(&quarantine).expect("quarantine JSON");
        assert_eq!(quarantine["schema"], 3);
        assert_eq!(quarantine["observations"]["obs-1"]["source"], "doc://1");
    }

    #[tokio::test]
    async fn persisted_schema_and_newer_schema_behavior_are_unchanged() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let path = directory.path().join("state.json");
        {
            let store = Store::open(&path, state_with_audit(1))
                .await
                .expect("store opens");
            store
                .read(|state| assert_eq!(state.schema, DATA_SCHEMA))
                .await;
        }
        let bytes = std::fs::read(&path).expect("persisted state");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("state JSON");
        assert_eq!(value["schema"].as_u64(), Some(u64::from(DATA_SCHEMA)));

        let mut newer = state_with_audit(1);
        newer.schema = DATA_SCHEMA + 1;
        std::fs::write(&path, serde_json::to_vec(&newer).expect("newer state JSON"))
            .expect("newer state file");
        let error = match Store::open(&path, State::default()).await {
            Ok(_) => panic!("newer schema is refused"),
            Err(error) => error,
        };
        assert_eq!(error.code, "storage.schema_unsupported");
    }
}
