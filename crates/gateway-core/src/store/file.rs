//! Standalone JSON dispatch store with a single-writer guard.
//!
//! One process owns the directory. Claims, revisions, and idempotency are
//! enforced under the process write lock and persisted before they are visible.
//! A second process fails with `storage.single_writer` instead of silently
//! clobbering the first.

use async_trait::async_trait;
use common::{ApiError, Task, TaskState};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

use super::{
    atomic_write, index_key, AcceptedMapping, AttemptOutcome, Begin, Claim, ClaimToken,
    Connectivity, DispatchState, DispatchStore, NewIntent, ReplicaId, SingleWriter, StreamMapStore,
    StreamMapping, Submission, DISPATCH_SCHEMA,
};

/// Persisted Gateway dispatch state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchData {
    /// Persisted schema.
    pub schema: u32,
    /// External execution id -> submission.
    pub submissions: BTreeMap<String, Submission>,
    /// namespace|caller|key -> external execution id.
    pub keys: BTreeMap<String, String>,
}

impl Default for DispatchData {
    fn default() -> Self {
        Self {
            schema: DISPATCH_SCHEMA,
            submissions: BTreeMap::new(),
            keys: BTreeMap::new(),
        }
    }
}

/// Standalone durable dispatch store with one writer.
#[derive(Debug)]
pub struct FileDispatchStore {
    path: PathBuf,
    state: Mutex<DispatchData>,
    _lock: SingleWriter,
}

impl FileDispatchStore {
    /// Open or create the dispatch file. A newer schema and a second writer fail
    /// closed.
    pub async fn open(directory: impl AsRef<Path>) -> Result<Self, ApiError> {
        let directory = directory.as_ref();
        std::fs::create_dir_all(directory).map_err(|error| {
            ApiError::new(
                "storage.unavailable",
                format!("cannot create Gateway data directory: {error}"),
            )
        })?;
        let lock = SingleWriter::acquire(&directory.join("gateway.lock"))?;
        let path = directory.join("dispatch.json");
        let state = if path.exists() {
            let bytes = std::fs::read(&path).map_err(|error| {
                ApiError::new(
                    "storage.unavailable",
                    format!("cannot read Gateway dispatch state: {error}"),
                )
            })?;
            let state: DispatchData = serde_json::from_slice(&bytes).map_err(|error| {
                ApiError::new(
                    "storage.corrupt",
                    format!("cannot parse Gateway dispatch state: {error}"),
                )
            })?;
            if state.schema > DISPATCH_SCHEMA {
                return Err(ApiError::new(
                    "storage.schema_unsupported",
                    format!(
                        "Gateway dispatch schema {} is newer than supported {}",
                        state.schema, DISPATCH_SCHEMA
                    ),
                ));
            }
            state
        } else {
            DispatchData::default()
        };
        atomic_write(
            &path,
            &serde_json::to_vec(&state).map_err(|error| {
                ApiError::new(
                    "storage.unavailable",
                    format!("cannot serialize Gateway dispatch state: {error}"),
                )
            })?,
        )?;
        Ok(Self {
            path,
            state: Mutex::new(state),
            _lock: lock,
        })
    }

    /// Read a value under the store lock.
    pub async fn read<R>(&self, f: impl FnOnce(&DispatchData) -> R) -> R {
        f(&*self.state.lock().await)
    }

    async fn write<R>(
        &self,
        f: impl FnOnce(&mut DispatchData) -> Result<R, ApiError>,
    ) -> Result<R, ApiError> {
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();
        let result = f(&mut next)?;
        if let Some(error) = crate::fault::persistence_failure() {
            return Err(error);
        }
        atomic_write(
            &self.path,
            &serde_json::to_vec(&next).map_err(serialize_error)?,
        )?;
        *guard = next;
        Ok(result)
    }
}

fn serialize_error(error: serde_json::Error) -> ApiError {
    ApiError::new(
        "storage.unavailable",
        format!("cannot serialize Gateway dispatch state: {error}"),
    )
}

fn classify(submission: Submission, digest: &str) -> Begin {
    if submission.request_digest == digest {
        Begin::Existing(submission)
    } else {
        Begin::Conflict(submission)
    }
}

fn lost_mapping(execution_id: &str) -> ApiError {
    ApiError::new(
        "storage.corrupt",
        "the dispatch index points to a missing execution",
    )
    .with("execution_id", execution_id.to_string())
}

#[async_trait]
impl DispatchStore for FileDispatchStore {
    async fn begin(&self, intent: NewIntent) -> Result<Begin, ApiError> {
        let key = index_key(&intent.namespace, &intent.caller, &intent.idempotency_key);
        self.write(|data| {
            if let Some(existing_id) = data.keys.get(&key) {
                let submission = data
                    .submissions
                    .get(existing_id)
                    .cloned()
                    .ok_or_else(|| lost_mapping(existing_id))?;
                return Ok(classify(submission, &intent.request_digest));
            }
            let now = common::now_ms();
            let mut submission = Submission {
                execution_id: intent.execution_id.clone(),
                namespace: intent.namespace.clone(),
                caller: intent.caller.clone(),
                idempotency_key: intent.idempotency_key.clone(),
                request_digest: intent.request_digest.clone(),
                worker: intent.worker.clone(),
                worker_instance: String::new(),
                worker_incarnation: String::new(),
                agent: intent.agent.clone(),
                dispatch: DispatchState::Intent,
                worker_task_id: None,
                accepted: None,
                projection: None,
                connectivity: Connectivity::Unknown,
                last_error: None,
                revision: 1,
                claim_id: None,
                claimed_by: None,
                claim_expires_at: None,
                created_at: now,
                updated_at: now,
                transitions: Vec::new(),
            };
            submission.transition(
                "intent_recorded",
                "caller-scoped identity and dispatch intent are durable",
            );
            data.keys.insert(key, submission.execution_id.clone());
            data.submissions
                .insert(submission.execution_id.clone(), submission.clone());
            Ok(Begin::Created(submission))
        })
        .await
    }

    async fn claim(
        &self,
        execution_id: &str,
        replica: &ReplicaId,
        claim_ttl_ms: i64,
    ) -> Result<Claim, ApiError> {
        let now = common::now_ms();
        self.write(|data| {
            let submission = data
                .submissions
                .get_mut(execution_id)
                .ok_or_else(|| lost_mapping(execution_id))?;
            if submission.dispatch == DispatchState::Accepted {
                return Ok(Claim::Accepted(submission.clone()));
            }
            if submission.dispatch == DispatchState::Dispatching {
                if let Some(until) = submission.claim_expires_at {
                    if until > now {
                        return Ok(Claim::Busy { until_ms: until });
                    }
                }
            }
            let token = ClaimToken {
                execution_id: execution_id.to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                replica: replica.0.clone(),
            };
            submission.dispatch = DispatchState::Dispatching;
            submission.connectivity = Connectivity::Unknown;
            submission.claim_id = Some(token.claim_id.clone());
            submission.claimed_by = Some(token.replica.clone());
            submission.claim_expires_at = Some(now.saturating_add(claim_ttl_ms.max(1)));
            submission.revision = submission.revision.saturating_add(1);
            submission.transition(
                "dispatch_attempted",
                "remote submission may have reached the Worker",
            );
            Ok(Claim::Claimed(token, submission.clone()))
        })
        .await
    }

    async fn commit(
        &self,
        claim: &ClaimToken,
        mapping: AcceptedMapping,
    ) -> Result<Submission, ApiError> {
        self.write(|data| {
            let submission = data
                .submissions
                .get_mut(&claim.execution_id)
                .ok_or_else(|| lost_mapping(&claim.execution_id))?;
            if submission.dispatch == DispatchState::Accepted {
                return if submission.worker_task_id.as_deref() == Some(&mapping.worker_task_id) {
                    Ok(submission.clone())
                } else {
                    Err(ApiError::new(
                        "task.dispatch_conflict",
                        "the Worker acceptance was already committed with a different task handle",
                    )
                    .with("execution_id", claim.execution_id.clone()))
                };
            }
            if submission.claim_id.as_deref() != Some(&claim.claim_id)
                || submission.dispatch != DispatchState::Dispatching
            {
                return Err(ApiError::retryable(
                    "task.claim_lost",
                    "the dispatch claim was lost before the mapping could commit",
                )
                .with("execution_id", claim.execution_id.clone()));
            }
            submission.dispatch = DispatchState::Accepted;
            submission.worker_task_id = Some(mapping.worker_task_id);
            submission.worker_instance = mapping.worker_instance;
            submission.worker_incarnation = mapping.worker_incarnation;
            submission.accepted = Some(mapping.accepted);
            submission.connectivity = Connectivity::Online;
            submission.last_error = None;
            submission.claim_id = None;
            submission.claim_expires_at = None;
            submission.revision = submission.revision.saturating_add(1);
            submission.transition(
                "accepted",
                "Worker returned a durable task handle for the original key",
            );
            Ok(submission.clone())
        })
        .await
    }

    async fn release(&self, claim: &ClaimToken, outcome: AttemptOutcome) -> Result<(), ApiError> {
        self.write(|data| {
            let submission = data
                .submissions
                .get_mut(&claim.execution_id)
                .ok_or_else(|| lost_mapping(&claim.execution_id))?;
            if submission.claim_id.as_deref() != Some(&claim.claim_id) {
                return Err(ApiError::new(
                    "task.claim_lost",
                    "the dispatch claim was already released",
                )
                .with("execution_id", claim.execution_id.clone()));
            }
            submission.dispatch = outcome.state;
            submission.connectivity = outcome.connectivity;
            submission.last_error = outcome.last_error;
            submission.claim_id = None;
            submission.claim_expires_at = None;
            submission.revision = submission.revision.saturating_add(1);
            submission.transition(&outcome.outcome, &outcome.detail);
            Ok(())
        })
        .await
    }

    async fn record_projection(
        &self,
        execution_id: &str,
        projection: Option<Task>,
        state: Option<TaskState>,
        connectivity: Connectivity,
    ) -> Result<(), ApiError> {
        self.write(|data| {
            if let Some(submission) = data.submissions.get_mut(execution_id) {
                submission.projection = projection;
                if let (Some(accepted), Some(state)) = (submission.accepted.as_mut(), state) {
                    accepted.state = state;
                }
                submission.connectivity = connectivity;
                submission.revision = submission.revision.saturating_add(1);
                submission.transition("projection_refreshed", "Worker projection observed");
            }
            Ok(())
        })
        .await
    }

    async fn mark_connectivity(
        &self,
        execution_id: &str,
        connectivity: Connectivity,
        outcome: &str,
        detail: &str,
    ) -> Result<(), ApiError> {
        self.write(|data| {
            if let Some(submission) = data.submissions.get_mut(execution_id) {
                submission.connectivity = connectivity;
                submission.revision = submission.revision.saturating_add(1);
                submission.transition(outcome, detail);
            }
            Ok(())
        })
        .await
    }

    async fn clear_projection(
        &self,
        execution_id: &str,
        outcome: &str,
        detail: &str,
    ) -> Result<(), ApiError> {
        self.write(|data| {
            if let Some(submission) = data.submissions.get_mut(execution_id) {
                submission.projection = None;
                submission.last_error = Some(outcome.to_string());
                submission.revision = submission.revision.saturating_add(1);
                submission.transition(outcome, detail);
            }
            Ok(())
        })
        .await
    }

    async fn transition(
        &self,
        execution_id: &str,
        outcome: &str,
        detail: &str,
    ) -> Result<(), ApiError> {
        self.write(|data| {
            if let Some(submission) = data.submissions.get_mut(execution_id) {
                submission.revision = submission.revision.saturating_add(1);
                submission.transition(outcome, detail);
            }
            Ok(())
        })
        .await
    }

    async fn submission(&self, execution_id: &str) -> Result<Option<Submission>, ApiError> {
        Ok(self
            .read(|data| data.submissions.get(execution_id).cloned())
            .await)
    }

    async fn submission_by_key(
        &self,
        namespace: &str,
        caller: &str,
        key: &str,
    ) -> Result<Option<Submission>, ApiError> {
        let key = index_key(namespace, caller, key);
        Ok(self
            .read(|data| {
                data.keys
                    .get(&key)
                    .and_then(|id| data.submissions.get(id))
                    .cloned()
            })
            .await)
    }

    fn description(&self) -> String {
        format!("file:{}", self.path.display())
    }

    fn path(&self) -> Option<PathBuf> {
        Some(self.path.clone())
    }
}

#[async_trait]
impl StreamMapStore for FileDispatchStore {
    async fn stream_mapping(&self, execution_id: &str) -> Result<Option<StreamMapping>, ApiError> {
        Ok(self
            .read(|data| {
                data.submissions
                    .get(execution_id)
                    .and_then(Submission::stream_mapping)
            })
            .await)
    }
}
