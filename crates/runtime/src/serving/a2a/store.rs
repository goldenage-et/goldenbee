//! Durable adapter for the pinned A2A v1 [`TaskStore`] extension point.
//!
//! The pinned release provides the trait and an in-memory implementation only.
//! This adapter is the approved narrower extension: it persists the native
//! entry fields, the owner binding, and the durable message-id idempotency
//! metadata without introducing a second task state machine.

use a2a_protocol_types::artifact::Artifact;
use a2a_protocol_types::task::{TaskState, TaskStatus};
use a2a_protocol_types::Message;
use adk_rust::server::a2a::v1::error::A2aError;
use adk_rust::server::a2a::v1::task_store::{TaskStore, TaskStoreEntry, OWNER_METADATA_KEY};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// File name of the per-instance A2A task store below the instance data
/// directory. Several instances from one package have separate data
/// directories, so this file is never shared implicitly.
pub const A2A_STORE_FILE: &str = "a2a-tasks.sqlite3";

/// Result of claiming a delegation token for one objective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationClaim {
    /// First use of this token.
    New,
    /// Same token and same message id; safe idempotent replay.
    Same,
    /// Same token used for a different message id.
    Replayed,
}

/// One durable A2A task store owned by a Worker Instance.
pub struct DurableTaskStore {
    pool: SqlitePool,
    path: PathBuf,
}

impl std::fmt::Debug for DurableTaskStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurableTaskStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl DurableTaskStore {
    /// Open, migrate, and verify the task store below `data_dir`.
    ///
    /// A corrupt or unavailable store is a startup failure: the caller must not
    /// serve A2A requests while claiming durable storage.
    pub async fn open(data_dir: impl AsRef<Path>) -> crate::Result<Arc<Self>> {
        let data_dir = data_dir.as_ref();
        std::fs::create_dir_all(data_dir).map_err(|error| {
            crate::ApiError::new(
                "a2a.storage_unavailable",
                format!("cannot create A2A data directory: {error}"),
            )
        })?;
        let path = data_dir.join(A2A_STORE_FILE);
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .map_err(|error| storage_error(&path, error))?;
        let store = Arc::new(Self { pool, path });
        store.migrate().await?;
        store.health().await?;
        Ok(store)
    }

    /// Path of the per-instance task store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn migrate(&self) -> crate::Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS a2a_tasks (
                id TEXT PRIMARY KEY NOT NULL,
                context_id TEXT NOT NULL,
                owner TEXT,
                status_json TEXT NOT NULL,
                artifacts_json TEXT NOT NULL,
                history_json TEXT NOT NULL,
                metadata_json TEXT NOT NULL,
                push_configs_json TEXT NOT NULL,
                message_id TEXT,
                message_digest TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error(&self.path, error))?;
        // One caller's message id maps to exactly one task, durably. The
        // deterministic task id also encodes this, but the index closes the
        // concurrent-submission race at the storage boundary.
        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS a2a_tasks_message
             ON a2a_tasks(owner, message_id)
             WHERE message_id IS NOT NULL",
        )
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error(&self.path, error))?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS a2a_tasks_context
             ON a2a_tasks(context_id, updated_at)",
        )
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error(&self.path, error))?;
        // A signed delegation token authorizes one objective. Reusing the same
        // token for a different message id is a replay, while repeating the
        // identical message id remains the native idempotent path.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS a2a_delegation_tokens (
                token_id TEXT PRIMARY KEY NOT NULL,
                message_id TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error(&self.path, error))?;
        Ok(())
    }

    async fn health(&self) -> crate::Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(|error| storage_error(&self.path, error))?;
        Ok(())
    }

    /// Atomically bind one delegation token to one objective.
    pub async fn claim_delegation(
        &self,
        token_id: &str,
        message_id: &str,
    ) -> Result<DelegationClaim, A2aError> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO a2a_delegation_tokens (token_id, message_id, created_at)
             VALUES (?, ?, ?)",
        )
        .bind(token_id)
        .bind(message_id)
        .bind(timestamp(Utc::now()))
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        if result.rows_affected() == 1 {
            return Ok(DelegationClaim::New);
        }
        let existing: Option<String> =
            sqlx::query("SELECT message_id FROM a2a_delegation_tokens WHERE token_id = ?")
                .bind(token_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(store_error)?
                .map(|row| row.get("message_id"));
        Ok(if existing.as_deref() == Some(message_id) {
            DelegationClaim::Same
        } else {
            DelegationClaim::Replayed
        })
    }

    /// Durable message-id lookup scoped to one owner.
    pub async fn find_idempotent(
        &self,
        owner: &str,
        message_id: &str,
    ) -> Result<Option<(String, String)>, A2aError> {
        let row = sqlx::query(
            "SELECT id, message_digest FROM a2a_tasks
             WHERE owner = ? AND message_id = ?",
        )
        .bind(owner)
        .bind(message_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(row.map(|row| {
            (
                row.get::<String, _>("id"),
                row.get::<Option<String>, _>("message_digest")
                    .unwrap_or_default(),
            )
        }))
    }

    /// Insert a task together with durable idempotency metadata.
    ///
    /// Returns `false` when another request already claimed the same
    /// `(owner, message_id)` pair. The caller then reads the existing task
    /// instead of running the objective a second time.
    pub async fn insert_new(
        &self,
        entry: &TaskStoreEntry,
        owner: &str,
        message_id: &str,
        digest: &str,
    ) -> Result<bool, A2aError> {
        let mut metadata = entry.metadata.clone();
        metadata.insert(
            OWNER_METADATA_KEY.to_string(),
            serde_json::Value::String(owner.to_string()),
        );
        let result = sqlx::query(
            "INSERT OR IGNORE INTO a2a_tasks (
                id, context_id, owner, status_json, artifacts_json, history_json,
                metadata_json, push_configs_json, message_id, message_digest,
                created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&entry.id)
        .bind(&entry.context_id)
        .bind(owner)
        .bind(encode(&entry.status)?)
        .bind(encode(&entry.artifacts)?)
        .bind(encode(&entry.history)?)
        .bind(encode(&metadata)?)
        .bind(encode(&entry.push_configs)?)
        .bind(message_id)
        .bind(digest)
        .bind(timestamp(entry.created_at))
        .bind(timestamp(entry.updated_at))
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// Mark every non-terminal task as interrupted without inventing a state.
    ///
    /// A2A has no `UNKNOWN` or `RECOVERING` state. The truthful projection is
    /// the retained native state plus recoverable artifacts/history and an
    /// explicit adapter marker.
    pub async fn mark_interrupted(&self) -> crate::Result<u64> {
        let rows = sqlx::query("SELECT id, metadata_json FROM a2a_tasks")
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error(&self.path, error))?;
        let mut marked = 0;
        for row in rows {
            let id: String = row.get("id");
            let metadata_json: String = row.get("metadata_json");
            let metadata: HashMap<String, serde_json::Value> = decode(&metadata_json)
                .map_err(|error| crate::ApiError::new("a2a.storage_corrupt", error.to_string()))?;
            let state = sqlx::query("SELECT status_json FROM a2a_tasks WHERE id = ?")
                .bind(&id)
                .fetch_one(&self.pool)
                .await
                .map_err(|error| storage_error(&self.path, error))
                .and_then(|row| {
                    let status: String = row.get("status_json");
                    decode::<TaskStatus>(&status).map_err(|error| {
                        crate::ApiError::new("a2a.storage_corrupt", error.to_string())
                    })
                })?;
            if is_terminal(state.state) {
                continue;
            }
            let mut metadata = metadata;
            metadata.insert(
                "goldenbee.recovery".into(),
                serde_json::Value::String("interrupted".into()),
            );
            let encoded = encode(&metadata)
                .map_err(|error| crate::ApiError::new("a2a.storage_corrupt", error.to_string()))?;
            sqlx::query("UPDATE a2a_tasks SET metadata_json = ?, updated_at = ? WHERE id = ?")
                .bind(encoded)
                .bind(timestamp(Utc::now()))
                .bind(&id)
                .execute(&self.pool)
                .await
                .map_err(|error| storage_error(&self.path, error))?;
            marked += 1;
        }
        Ok(marked)
    }

    async fn load(&self, task_id: &str) -> Result<TaskStoreEntry, A2aError> {
        let row = sqlx::query("SELECT * FROM a2a_tasks WHERE id = ?")
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(store_error)?
            .ok_or_else(|| A2aError::TaskNotFound {
                task_id: task_id.to_string(),
            })?;
        entry_from_row(&row)
    }

    async fn all(&self) -> Result<Vec<TaskStoreEntry>, A2aError> {
        let rows = sqlx::query("SELECT * FROM a2a_tasks")
            .fetch_all(&self.pool)
            .await
            .map_err(store_error)?;
        rows.iter().map(entry_from_row).collect()
    }
}

#[async_trait]
impl TaskStore for DurableTaskStore {
    async fn create_task(&self, entry: TaskStoreEntry) -> Result<(), A2aError> {
        // Upsert keeps the durable idempotency columns that the native
        // `TaskStore` trait cannot carry.
        sqlx::query(
            "INSERT INTO a2a_tasks (
                id, context_id, owner, status_json, artifacts_json, history_json,
                metadata_json, push_configs_json, created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                context_id = excluded.context_id,
                owner = excluded.owner,
                status_json = excluded.status_json,
                artifacts_json = excluded.artifacts_json,
                history_json = excluded.history_json,
                metadata_json = excluded.metadata_json,
                push_configs_json = excluded.push_configs_json,
                updated_at = excluded.updated_at",
        )
        .bind(&entry.id)
        .bind(&entry.context_id)
        .bind(entry.owner())
        .bind(encode(&entry.status)?)
        .bind(encode(&entry.artifacts)?)
        .bind(encode(&entry.history)?)
        .bind(encode(&entry.metadata)?)
        .bind(encode(&entry.push_configs)?)
        .bind(timestamp(entry.created_at))
        .bind(timestamp(entry.updated_at))
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn get_task(&self, task_id: &str) -> Result<TaskStoreEntry, A2aError> {
        self.load(task_id).await
    }

    async fn update_status(&self, task_id: &str, status: TaskStatus) -> Result<(), A2aError> {
        let result =
            sqlx::query("UPDATE a2a_tasks SET status_json = ?, updated_at = ? WHERE id = ?")
                .bind(encode(&status)?)
                .bind(timestamp(Utc::now()))
                .bind(task_id)
                .execute(&self.pool)
                .await
                .map_err(store_error)?;
        if result.rows_affected() == 0 {
            return Err(A2aError::TaskNotFound {
                task_id: task_id.to_string(),
            });
        }
        Ok(())
    }

    async fn add_artifact(&self, task_id: &str, artifact: Artifact) -> Result<(), A2aError> {
        let mut entry = self.load(task_id).await?;
        entry.artifacts.push(artifact);
        self.update_artifacts(task_id, &entry.artifacts).await
    }

    async fn add_history_message(&self, task_id: &str, message: Message) -> Result<(), A2aError> {
        let mut entry = self.load(task_id).await?;
        entry.history.push(message);
        sqlx::query("UPDATE a2a_tasks SET history_json = ?, updated_at = ? WHERE id = ?")
            .bind(encode(&entry.history)?)
            .bind(timestamp(Utc::now()))
            .bind(task_id)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn find_task_by_context(
        &self,
        context_id: &str,
    ) -> Result<Option<TaskStoreEntry>, A2aError> {
        let mut matches: Vec<TaskStoreEntry> = self
            .all()
            .await?
            .into_iter()
            .filter(|entry| entry.context_id == context_id && !is_terminal(entry.status.state))
            .collect();
        matches.sort_by_key(|entry| entry.updated_at);
        Ok(matches.pop())
    }

    async fn list_tasks(
        &self,
        params: adk_rust::server::a2a::v1::task_store::ListTasksParams,
    ) -> Result<Vec<TaskStoreEntry>, A2aError> {
        let mut entries: Vec<TaskStoreEntry> = self
            .all()
            .await?
            .into_iter()
            .filter(|entry| {
                params
                    .context_id
                    .as_ref()
                    .is_none_or(|context| &entry.context_id == context)
                    && params
                        .state
                        .as_ref()
                        .is_none_or(|state| &entry.status.state == state)
            })
            .collect();
        entries.sort_by_key(|entry| entry.created_at);
        if let Some(page_size) = params.page_size {
            entries.truncate(page_size as usize);
        }
        Ok(entries)
    }

    async fn delete_task(&self, task_id: &str) -> Result<(), A2aError> {
        let result = sqlx::query("DELETE FROM a2a_tasks WHERE id = ?")
            .bind(task_id)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        if result.rows_affected() == 0 {
            return Err(A2aError::TaskNotFound {
                task_id: task_id.to_string(),
            });
        }
        Ok(())
    }
}

impl DurableTaskStore {
    async fn update_artifacts(
        &self,
        task_id: &str,
        artifacts: &[Artifact],
    ) -> Result<(), A2aError> {
        sqlx::query("UPDATE a2a_tasks SET artifacts_json = ?, updated_at = ? WHERE id = ?")
            .bind(encode(&artifacts)?)
            .bind(timestamp(Utc::now()))
            .bind(task_id)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(())
    }
}

fn entry_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<TaskStoreEntry, A2aError> {
    let owner: Option<String> = row.get("owner");
    let mut metadata: HashMap<String, serde_json::Value> =
        decode(&row.get::<String, _>("metadata_json"))?;
    if let Some(owner) = owner {
        metadata.insert(
            OWNER_METADATA_KEY.to_string(),
            serde_json::Value::String(owner),
        );
    }
    Ok(TaskStoreEntry {
        id: row.get("id"),
        context_id: row.get("context_id"),
        status: decode(&row.get::<String, _>("status_json"))?,
        artifacts: decode(&row.get::<String, _>("artifacts_json"))?,
        history: decode(&row.get::<String, _>("history_json"))?,
        metadata,
        push_configs: decode(&row.get::<String, _>("push_configs_json"))?,
        created_at: parse_timestamp(&row.get::<String, _>("created_at"))?,
        updated_at: parse_timestamp(&row.get::<String, _>("updated_at"))?,
    })
}

fn encode<T: Serialize>(value: &T) -> Result<String, A2aError> {
    serde_json::to_string(value).map_err(|error| A2aError::Internal {
        message: format!("A2A store encoding failed: {error}"),
    })
}

fn decode<T: DeserializeOwned>(value: &str) -> Result<T, A2aError> {
    serde_json::from_str(value).map_err(|error| A2aError::Internal {
        message: format!("A2A store record is invalid: {error}"),
    })
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, A2aError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| A2aError::Internal {
            message: format!("A2A store timestamp is invalid: {error}"),
        })
}

fn store_error(error: sqlx::Error) -> A2aError {
    A2aError::Internal {
        message: format!("A2A task store failed: {error}"),
    }
}

fn storage_error(path: &Path, error: impl std::fmt::Display) -> crate::ApiError {
    crate::ApiError::new(
        "a2a.storage_unavailable",
        format!("cannot open A2A task store '{}': {error}", path.display()),
    )
}

pub(crate) fn is_terminal(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Failed | TaskState::Canceled | TaskState::Rejected
    )
}
