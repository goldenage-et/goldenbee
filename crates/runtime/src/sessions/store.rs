//! Durable native ADK session persistence.
//!
//! The canonical session history is the sequence of native ADK [`Event`] values
//! stored here. The harness does not keep a second role/text transcript. This
//! module implements the upstream [`SessionService`] extension point and writes
//! atomically to one file below the Worker Instance's data directory.
//!
//! Identity is the ADK `(app_name, user_id, session_id)` triple. The runtime
//! maps `app_name` to the package identity and `user_id` to the authenticated
//! `(namespace, principal)` pair, so a colliding session id under another
//! caller addresses a different record and cannot read or modify this one.

use adk_rust::session::{
    extract_state_deltas, merge_states, AppendEventRequest, CreateRequest, DeleteRequest, Event,
    Events, GetRequest, ListRequest, Session, SessionService, State,
};
use adk_rust::{AdkError, AppName, ErrorCategory, ErrorComponent, SessionId, UserId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;
use uuid::Uuid;

/// File name used for the canonical native session store.
pub const SESSION_STORE_FILE: &str = "native-sessions.json";

/// Persisted native session schema. Older schemas are read as-is; a newer one
/// is refused rather than silently rewritten.
pub const SESSION_STORE_SCHEMA: u32 = 1;

#[derive(Clone, Serialize, Deserialize)]
struct StoredState {
    #[serde(default = "default_schema")]
    schema: u32,
    #[serde(default)]
    sessions: BTreeMap<String, StoredSession>,
    #[serde(default)]
    app_states: BTreeMap<String, HashMap<String, Value>>,
    #[serde(default)]
    user_states: BTreeMap<String, BTreeMap<String, HashMap<String, Value>>>,
}

fn default_schema() -> u32 {
    SESSION_STORE_SCHEMA
}

impl Default for StoredState {
    fn default() -> Self {
        Self {
            schema: SESSION_STORE_SCHEMA,
            sessions: BTreeMap::new(),
            app_states: BTreeMap::new(),
            user_states: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredSession {
    app_name: String,
    user_id: String,
    session_id: String,
    /// Session-tier state, without `app:` or `user:` prefixes.
    state: HashMap<String, Value>,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

/// Durable [`SessionService`] backed by one atomically replaced file.
pub struct FileSessionService {
    path: PathBuf,
    state: Mutex<StoredState>,
}

impl FileSessionService {
    /// Open or create the canonical session store.
    ///
    /// A corrupt or newer store is reported instead of being replaced. The
    /// initial file is written immediately, so a caller never receives a
    /// durable session promise from storage that cannot persist.
    pub fn open(path: impl Into<PathBuf>) -> crate::Result<Self> {
        let path = path.into();
        let state = if path.exists() {
            let bytes = std::fs::read(&path).map_err(|error| {
                crate::Error::new(
                    "session.storage_unavailable",
                    format!("cannot read native session store: {error}"),
                )
            })?;
            let state: StoredState = serde_json::from_slice(&bytes).map_err(|error| {
                crate::Error::new(
                    "session.storage_corrupt",
                    format!("cannot parse native session store: {error}"),
                )
            })?;
            if state.schema > SESSION_STORE_SCHEMA {
                return Err(crate::Error::new(
                    "session.storage_unsupported",
                    format!(
                        "native session schema {} is newer than supported {}",
                        state.schema, SESSION_STORE_SCHEMA
                    ),
                ));
            }
            state
        } else {
            StoredState::default()
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                crate::Error::new(
                    "session.storage_unavailable",
                    format!("cannot create native session directory: {error}"),
                )
            })?;
        }
        // Prove the store is writable before any caller is told a session is
        // durable.
        let bytes = serde_json::to_vec(&state).map_err(|error| {
            crate::Error::new(
                "session.storage_unavailable",
                format!("cannot serialize native sessions: {error}"),
            )
        })?;
        atomic_write(&path, &bytes).map_err(|error| {
            crate::Error::new(
                "session.storage_unavailable",
                format!("cannot persist native sessions: {error}"),
            )
        })?;
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    /// Path of the canonical session store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove sessions whose last update is older than `cutoff_ms`.
    ///
    /// Session identities in `preserve` are retained because an unfinished task
    /// still needs its native history for recovery. Returns the removed count.
    pub async fn retention(
        &self,
        cutoff_ms: i64,
        preserve: &std::collections::BTreeSet<String>,
    ) -> crate::Result<usize> {
        self.update(|state| {
            let before = state.sessions.len();
            state.sessions.retain(|_, session| {
                preserve.contains(&session.session_id)
                    || session.updated_at.timestamp_millis() >= cutoff_ms
            });
            let removed = before - state.sessions.len();
            prune_orphan_user_states(state);
            Ok(removed)
        })
        .await
        .map_err(|error| crate::Error::new("session.retention_failed", error.to_string()))
    }

    /// Hard-delete every session and user-scoped state for one native user id.
    ///
    /// The caller supplies the unambiguous native identity produced by the
    /// runtime identity adapter, so a colliding session id under another caller
    /// cannot be erased by mistake.
    pub async fn erase_user(&self, native_user_id: &str) -> crate::Result<usize> {
        self.update(|state| {
            let before = state.sessions.len();
            state
                .sessions
                .retain(|_, session| session.user_id != native_user_id);
            for users in state.user_states.values_mut() {
                users.remove(native_user_id);
            }
            prune_orphan_user_states(state);
            Ok(before - state.sessions.len())
        })
        .await
        .map_err(|error| crate::Error::new("session.erasure_failed", error.to_string()))
    }

    async fn update<R>(
        &self,
        change: impl FnOnce(&mut StoredState) -> adk_rust::Result<R>,
    ) -> adk_rust::Result<R> {
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();
        let result = change(&mut next)?;
        let bytes = serde_json::to_vec(&next)
            .map_err(|error| storage_error(format!("cannot serialize native sessions: {error}")))?;
        atomic_write(&self.path, &bytes)
            .map_err(|error| storage_error(format!("cannot persist native sessions: {error}")))?;
        *guard = next;
        Ok(result)
    }

    async fn read<R>(&self, read: impl FnOnce(&StoredState) -> R) -> R {
        let state = self.state.lock().await;
        read(&state)
    }
}

#[async_trait::async_trait]
impl SessionService for FileSessionService {
    async fn create(&self, req: CreateRequest) -> adk_rust::Result<Box<dyn Session>> {
        let session_id = req
            .session_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let app_name = req.app_name.clone();
        let user_id = req.user_id.clone();
        validate_identity(&app_name, &user_id, &session_id)?;
        let key = session_key(&app_name, &user_id, &session_id);
        let (app_delta, user_delta, session_delta) = extract_state_deltas(&req.state);
        let timestamp = Utc::now();
        self.update(move |state| {
            state
                .app_states
                .entry(app_name.clone())
                .or_default()
                .extend(app_delta);
            state
                .user_states
                .entry(app_name.clone())
                .or_default()
                .entry(user_id.clone())
                .or_default()
                .extend(user_delta);
            let app = state.app_states.get(&app_name).cloned().unwrap_or_default();
            let user = state
                .user_states
                .get(&app_name)
                .and_then(|users| users.get(&user_id))
                .cloned()
                .unwrap_or_default();
            let merged = merge_states(&app, &user, &session_delta);
            state.sessions.insert(
                key,
                StoredSession {
                    app_name: app_name.clone(),
                    user_id: user_id.clone(),
                    session_id: session_id.clone(),
                    state: session_delta,
                    events: Vec::new(),
                    updated_at: timestamp,
                },
            );
            Ok(Box::new(FileSession {
                app_name,
                user_id,
                session_id,
                state: merged,
                events: Vec::new(),
                updated_at: timestamp,
            }) as Box<dyn Session>)
        })
        .await
    }

    async fn get(&self, req: GetRequest) -> adk_rust::Result<Box<dyn Session>> {
        validate_identity(&req.app_name, &req.user_id, &req.session_id)?;
        let key = session_key(&req.app_name, &req.user_id, &req.session_id);
        let found = self
            .read(|state| {
                let stored = state.sessions.get(&key)?;
                let app = state
                    .app_states
                    .get(&req.app_name)
                    .cloned()
                    .unwrap_or_default();
                let user = state
                    .user_states
                    .get(&req.app_name)
                    .and_then(|users| users.get(&req.user_id))
                    .cloned()
                    .unwrap_or_default();
                let mut session = snapshot(stored, &app, &user);
                if let Some(count) = req.num_recent_events {
                    let start = session.events.len().saturating_sub(count);
                    session.events = session.events[start..].to_vec();
                }
                if let Some(after) = req.after {
                    session.events.retain(|event| event.timestamp >= after);
                }
                Some(session)
            })
            .await;
        found
            .map(|session| Box::new(session) as Box<dyn Session>)
            .ok_or_else(|| not_found(&req.app_name, &req.user_id, &req.session_id))
    }

    async fn list(&self, req: ListRequest) -> adk_rust::Result<Vec<Box<dyn Session>>> {
        let app_name = req.app_name.clone();
        let user_id = req.user_id.clone();
        let offset = req.offset.unwrap_or(0);
        let limit = req.limit.unwrap_or(usize::MAX);
        Ok(self
            .read(|state| {
                let app = state.app_states.get(&app_name).cloned().unwrap_or_default();
                let user = state
                    .user_states
                    .get(&app_name)
                    .and_then(|users| users.get(&user_id))
                    .cloned()
                    .unwrap_or_default();
                let mut sessions: Vec<FileSession> = state
                    .sessions
                    .values()
                    .filter(|stored| stored.app_name == app_name && stored.user_id == user_id)
                    .map(|stored| snapshot(stored, &app, &user))
                    .collect();
                sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
                sessions
                    .into_iter()
                    .skip(offset)
                    .take(limit)
                    .map(|session| Box::new(session) as Box<dyn Session>)
                    .collect()
            })
            .await)
    }

    async fn delete(&self, req: DeleteRequest) -> adk_rust::Result<()> {
        validate_identity(&req.app_name, &req.user_id, &req.session_id)?;
        let key = session_key(&req.app_name, &req.user_id, &req.session_id);
        self.update(|state| {
            state.sessions.remove(&key);
            Ok(())
        })
        .await
    }

    async fn append_event(&self, session_id: &str, event: Event) -> adk_rust::Result<()> {
        let key = self
            .read(|state| {
                let matches: Vec<String> = state
                    .sessions
                    .iter()
                    .filter(|(_, stored)| stored.session_id == session_id)
                    .map(|(key, _)| key.clone())
                    .collect();
                (matches.len(), matches.into_iter().next())
            })
            .await;
        match key {
            (0, _) => Err(not_found("", "", session_id)),
            (1, Some(key)) => self.append_by_key(&key, event).await,
            _ => Err(AdkError::new(
                ErrorComponent::Session,
                ErrorCategory::InvalidInput,
                "session.ambiguous_identity",
                format!(
                    "session id '{session_id}' exists under more than one caller; \
                     append with the full identity instead"
                ),
            )),
        }
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> adk_rust::Result<()> {
        let app_name = req.identity.app_name.as_ref().to_string();
        let user_id = req.identity.user_id.as_ref().to_string();
        let session_id = req.identity.session_id.as_ref().to_string();
        validate_identity(&app_name, &user_id, &session_id)?;
        let key = session_key(&app_name, &user_id, &session_id);
        self.append_by_key(&key, req.event).await
    }

    async fn health_check(&self) -> adk_rust::Result<()> {
        if self.path.exists() {
            Ok(())
        } else {
            Err(storage_error("native session store is missing"))
        }
    }
}

impl FileSessionService {
    async fn append_by_key(&self, key: &str, mut event: Event) -> adk_rust::Result<()> {
        event
            .actions
            .state_delta
            .retain(|name, _| !name.starts_with("temp:"));
        let key = key.to_string();
        self.update(move |state| {
            let stored = state
                .sessions
                .get_mut(&key)
                .ok_or_else(|| not_found("", "", &key))?;
            let app_name = stored.app_name.clone();
            let user_id = stored.user_id.clone();
            let (app_delta, user_delta, session_delta) =
                extract_state_deltas(&event.actions.state_delta);
            state
                .app_states
                .entry(app_name.clone())
                .or_default()
                .extend(app_delta);
            state
                .user_states
                .entry(app_name)
                .or_default()
                .entry(user_id)
                .or_default()
                .extend(user_delta);
            stored.state.extend(session_delta);
            stored.updated_at = event.timestamp;
            stored.events.push(event);
            Ok(())
        })
        .await
    }
}

/// Remove per-user state that no longer has a session in this store. App-level
/// state is namespace configuration, not caller content, and is left untouched.
fn prune_orphan_user_states(state: &mut StoredState) {
    let live: std::collections::BTreeSet<(String, String)> = state
        .sessions
        .values()
        .map(|session| (session.app_name.clone(), session.user_id.clone()))
        .collect();
    for (app_name, users) in state.user_states.iter_mut() {
        users.retain(|user_id, _| live.contains(&(app_name.clone(), user_id.clone())));
    }
    state.user_states.retain(|_, users| !users.is_empty());
}

fn snapshot(
    stored: &StoredSession,
    app: &HashMap<String, Value>,
    user: &HashMap<String, Value>,
) -> FileSession {
    FileSession {
        app_name: stored.app_name.clone(),
        user_id: stored.user_id.clone(),
        session_id: stored.session_id.clone(),
        state: merge_states(app, user, &stored.state),
        events: stored.events.clone(),
        updated_at: stored.updated_at,
    }
}

struct FileSession {
    app_name: String,
    user_id: String,
    session_id: String,
    state: HashMap<String, Value>,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for FileSession {
    fn id(&self) -> &str {
        &self.session_id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn state(&self) -> &dyn State {
        self
    }

    fn events(&self) -> &dyn Events {
        self
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

impl State for FileSession {
    fn get(&self, key: &str) -> Option<Value> {
        self.state.get(key).cloned()
    }

    fn set(&mut self, key: String, value: Value) {
        if adk_rust::validate_state_key(&key).is_ok() {
            self.state.insert(key, value);
        }
    }

    fn all(&self) -> HashMap<String, Value> {
        self.state.clone()
    }
}

impl Events for FileSession {
    fn all(&self) -> Vec<Event> {
        self.events.clone()
    }

    fn len(&self) -> usize {
        self.events.len()
    }

    fn at(&self, index: usize) -> Option<&Event> {
        self.events.get(index)
    }
}

fn validate_identity(app_name: &str, user_id: &str, session_id: &str) -> adk_rust::Result<()> {
    let invalid = |kind: &str, error: String| {
        AdkError::new(
            ErrorComponent::Session,
            ErrorCategory::InvalidInput,
            "session.invalid_identity",
            format!("{kind} is not a valid ADK identifier: {error}"),
        )
    };
    AppName::try_from(app_name).map_err(|error| invalid("app_name", error.to_string()))?;
    UserId::try_from(user_id).map_err(|error| invalid("user_id", error.to_string()))?;
    SessionId::try_from(session_id).map_err(|error| invalid("session_id", error.to_string()))?;
    Ok(())
}

fn session_key(app_name: &str, user_id: &str, session_id: &str) -> String {
    serde_json::to_string(&(app_name, user_id, session_id))
        .expect("identity tuple always serializes")
}

fn not_found(app_name: &str, user_id: &str, session_id: &str) -> AdkError {
    AdkError::not_found(
        ErrorComponent::Session,
        "session.not_found",
        format!("session '{session_id}' was not found for app '{app_name}' and user '{user_id}'"),
    )
}

fn storage_error(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unavailable,
        "session.storage_unavailable",
        message,
    )
}

/// Replace the session file through a synced temporary file and directory.
///
/// The file is flushed and synced before the rename; on Unix the parent
/// directory is synced afterwards so the committed entry survives a process
/// restart. Power-loss behavior is tested separately, not by this unit test.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
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
