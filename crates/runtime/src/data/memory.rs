//! Scoped persistent memory and ingested documents.
//!
//! Long-term memory items are owned by the pinned native ADK SQLite memory
//! service. This module is the runtime's narrow lifecycle surface over that
//! adapter: it checks caller authority, derives document restrictions, and
//! delegates storage, retrieval, and erasure to native memory. Knowledge graph
//! facts are owned by the native graph adapter in [`super::graph`]. `common`
//! only holds the wire representations.

use crate::{new_id, now, DocumentRecord, Result, Runtime};
use adk_memory::{MemoryService, SearchRequest, SearchResponse};
use adk_rust::{AdkError, BeforeModelCallback, Content, Tool, ToolContext};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use common::{scopes, ApiError, MemoryItem, Principal};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;

impl Runtime {
    /// Store a scoped memory item in the canonical native memory backend.
    pub async fn remember(
        &self,
        principal: &Principal,
        namespace: &str,
        source: &str,
        text: &str,
    ) -> Result<MemoryItem> {
        let memory = self.memory().await.ok_or_else(memory_unavailable)?;
        memory.remember(principal, namespace, source, text).await
    }

    async fn remember_from_document(
        &self,
        principal: &Principal,
        namespace: &str,
        source: &str,
        text: &str,
        allowed: Vec<String>,
    ) -> Result<MemoryItem> {
        let memory = self.memory().await.ok_or_else(memory_unavailable)?;
        memory
            .remember_with_allowed(principal, namespace, source, text, allowed)
            .await
    }

    /// Keyword retrieval over the native memory backend, scoped to one caller.
    ///
    /// The pinned SQLite backend uses FTS5 keyword matching. This is not vector
    /// semantic search and the runtime does not advertise it as such.
    pub async fn search_memory(
        &self,
        principal: &Principal,
        namespace: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryItem>> {
        let memory = self.memory().await.ok_or_else(memory_unavailable)?;
        memory.search(principal, namespace, query, limit).await
    }

    /// Erase one memory item from the native store. Retrieval and the adapter
    /// cache observe the deletion immediately; restart observes it durably.
    pub async fn erase_memory(&self, principal: &Principal, memory_id: &str) -> Result<()> {
        let memory = self.memory().await.ok_or_else(memory_unavailable)?;
        memory.erase(principal, memory_id).await?;
        self.audit(&principal.id, "memory.erase", memory_id, "erased", "")
            .await
    }

    /// Erase the caller's memory plus documents and graph facts in its
    /// namespace.
    ///
    /// Native memory identity is `(namespace, principal)`. This method erases
    /// the authenticated caller's memory, the namespace's documents, and every
    /// knowledge-graph scope recorded in that namespace.
    pub async fn erase_namespace_memory(&self, principal: &Principal) -> Result<usize> {
        principal.require(scopes::ADMIN)?;
        let namespace = principal.namespace.clone();
        if let Some(memory) = self.memory().await {
            memory.erase_principal(&namespace, &principal.id).await?;
        }
        if let Some(graph) = self.graph().await {
            graph.erase_namespace(&namespace).await?;
        }
        let count = self
            .store()
            .write(|state| {
                let documents = state
                    .documents
                    .keys()
                    .filter(|id| state.documents[id.as_str()].namespace == namespace)
                    .count();
                state
                    .documents
                    .retain(|_, document| document.namespace != namespace);
                Ok(documents)
            })
            .await?;
        self.audit(
            &principal.id,
            "memory.erase_namespace",
            &namespace,
            "erased",
            "",
        )
        .await?;
        Ok(count)
    }

    /// Ingest a real document into the authorized knowledge store. The source
    /// adapter is supplied by the caller; parsing is intentionally explicit.
    pub async fn ingest_document(
        &self,
        principal: &Principal,
        namespace: &str,
        title: &str,
        source: &str,
        content: &str,
        allowed: Vec<String>,
    ) -> Result<DocumentRecord> {
        principal.require(scopes::ARTIFACT_WRITE)?;
        principal.require_namespace(namespace)?;
        if content.len() > 16 * 1024 * 1024 {
            return Err(ApiError::new(
                "knowledge.document_too_large",
                "document exceeds the 16 MiB ingestion limit",
            ));
        }
        let document = DocumentRecord {
            id: new_id("doc"),
            namespace: namespace.to_string(),
            owner: principal.id.clone(),
            title: title.to_string(),
            source: source.to_string(),
            content: content.to_string(),
            digest: common::bytes_digest(content.as_bytes()),
            allowed,
            created_at: now(),
        };
        self.store()
            .write(|state| {
                state
                    .documents
                    .insert(document.id.clone(), document.clone());
                Ok(document.clone())
            })
            .await?;
        // Derived memories carry the document's allowed-principal snapshot so
        // every later read path, including cached and derived copies, keeps the
        // source restriction. Keyword retrieval is explicit: this is not
        // described as vector search.
        for paragraph in content.split("\n\n").filter(|part| !part.trim().is_empty()) {
            let _ = self
                .remember_from_document(
                    principal,
                    namespace,
                    &format!("{source}#{}", document.id),
                    paragraph.trim(),
                    document.allowed.clone(),
                )
                .await;
        }
        self.append_document_events(&document).await;
        Ok(document)
    }

    /// Search ingested documents after namespace and allowed-list filtering.
    pub async fn search_documents(
        &self,
        principal: &Principal,
        namespace: &str,
        query: &str,
    ) -> Result<Vec<DocumentRecord>> {
        principal.require(scopes::SESSION_READ)?;
        principal.require_namespace(namespace)?;
        let needle = query.to_lowercase();
        Ok(self
            .store()
            .read(|state| {
                state
                    .documents
                    .values()
                    .filter(|document| document.namespace == namespace)
                    .filter(|document| {
                        document.allowed.is_empty()
                            || document.allowed.iter().any(|id| id == &principal.id)
                    })
                    .filter(|document| {
                        needle.is_empty() || document.content.to_lowercase().contains(&needle)
                    })
                    .cloned()
                    .collect()
            })
            .await)
    }

    async fn append_document_events(&self, document: &DocumentRecord) {
        let _ = self
            .audit(
                "knowledge",
                "document.ingest",
                &document.id,
                "accepted",
                &document.source,
            )
            .await;
    }
}

fn memory_unavailable() -> ApiError {
    ApiError::new(
        "memory.unavailable",
        "this Worker Instance has no native memory service configured",
    )
}

use crate::execution::runner::{decode_native_user_id, native_user_id};

/// File name of the canonical native memory store.
pub const MEMORY_STORE_FILE: &str = "native-memory.sqlite3";

/// Provenance persisted in the native `author` column.
///
/// The native entry shape has no source or allowed-principal fields. Storing a
/// small JSON token in `author` keeps provenance and source restrictions in the
/// same canonical row, so they survive restart and every derived copy carries
/// the source restriction that created it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Provenance {
    v: u32,
    source: String,
    #[serde(default)]
    allowed: Vec<String>,
}

/// One parsed native result before scope and source filtering.
#[derive(Debug, Clone)]
struct RawEntry {
    id: String,
    source: String,
    text: String,
    allowed: Vec<String>,
    timestamp: DateTime<Utc>,
}

#[derive(Default)]
struct Cache {
    generation: u64,
    entries: HashMap<String, (u64, Vec<RawEntry>)>,
}

/// Scoped durable memory backed by the pinned native SQLite memory service.
pub struct NativeMemory {
    app_name: String,
    path: PathBuf,
    pool: SqlitePool,
    service: Arc<dyn MemoryService>,
    cache: Mutex<Cache>,
}

impl std::fmt::Debug for NativeMemory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeMemory")
            .field("app_name", &self.app_name)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl NativeMemory {
    /// Open the canonical native memory store below `data_dir`.
    ///
    /// Storage is created, migrated, and health-checked before this returns so
    /// startup never promises durable memory from an unavailable backend.
    pub async fn open(
        app_name: impl Into<String>,
        data_dir: impl AsRef<Path>,
    ) -> Result<Arc<Self>> {
        let app_name = app_name.into();
        let data_dir = data_dir.as_ref();
        std::fs::create_dir_all(data_dir).map_err(|error| {
            ApiError::new(
                "memory.storage_unavailable",
                format!("cannot create memory data directory: {error}"),
            )
        })?;
        let path = data_dir.join(MEMORY_STORE_FILE);
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .map_err(|error| storage_error(&path, AdkError::memory(error.to_string())))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .map_err(|error| storage_error(&path, AdkError::memory(error.to_string())))?;
        let service = adk_memory::SqliteMemoryService::from_pool(pool.clone());
        service
            .migrate()
            .await
            .map_err(|error| storage_error(&path, error))?;
        service
            .health_check()
            .await
            .map_err(|error| storage_error(&path, error))?;
        Ok(Arc::new(Self {
            app_name,
            path,
            pool,
            service: Arc::new(service),
            cache: Mutex::new(Cache::default()),
        }))
    }

    /// Path of the canonical native memory store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Store memory for an authenticated principal. The namespace must match
    /// the principal's namespace.
    pub async fn remember(
        &self,
        principal: &Principal,
        namespace: &str,
        source: &str,
        text: &str,
    ) -> Result<MemoryItem> {
        principal.require(common::scopes::SESSION_WRITE)?;
        principal.require_namespace(namespace)?;
        self.remember_with_allowed(principal, namespace, source, text, Vec::new())
            .await
    }

    /// Store memory derived from a source with an allowed-principal list.
    ///
    /// An empty list means the namespace. The restriction is persisted with the
    /// native row so a derived or cached copy cannot widen it later.
    pub async fn remember_with_allowed(
        &self,
        principal: &Principal,
        namespace: &str,
        source: &str,
        text: &str,
        allowed: Vec<String>,
    ) -> Result<MemoryItem> {
        principal.require(common::scopes::SESSION_WRITE)?;
        principal.require_namespace(namespace)?;
        let user_id = native_user_id(namespace, &principal.id)?;
        self.add_native(namespace, &user_id, source, text, allowed)
            .await
    }

    /// Retrieve memory for an authenticated principal. Namespace, principal,
    /// and any document/source restriction are applied before exposure, even
    /// for cached results.
    pub async fn search(
        &self,
        principal: &Principal,
        namespace: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryItem>> {
        principal.require(common::scopes::SESSION_READ)?;
        principal.require_namespace(namespace)?;
        let user_id = native_user_id(namespace, &principal.id)?;
        let raw = self.search_scoped(&user_id, query, limit).await?;
        Ok(raw
            .into_iter()
            .filter(|entry| {
                entry.allowed.is_empty() || entry.allowed.iter().any(|id| id == &principal.id)
            })
            .map(|entry| MemoryItem {
                id: entry.id,
                namespace: namespace.to_string(),
                source: entry.source,
                text: entry.text,
                valid_from: entry.timestamp.timestamp_millis(),
                valid_to: None,
                deleted: false,
            })
            .collect())
    }

    /// Erase one memory owned by the authenticated principal.
    ///
    /// The native backend is queried by exact memory identity under the caller's
    /// native user scope. A colliding id under another namespace or principal
    /// deletes nothing and reports `memory.not_found`.
    pub async fn erase(&self, principal: &Principal, memory_id: &str) -> Result<()> {
        principal.require(common::scopes::SESSION_WRITE)?;
        let user_id = native_user_id(&principal.namespace, &principal.id)?;
        let deleted = self
            .service
            .delete_entries(&self.app_name, &user_id, memory_id)
            .await
            .map_err(memory_error)?;
        if deleted == 0 {
            return Err(ApiError::new(
                "memory.not_found",
                "memory item is not visible to this caller",
            ));
        }
        self.invalidate().await;
        Ok(())
    }

    /// Erase all memory for one native user identity. Used by principal erasure.
    pub async fn erase_principal(&self, namespace: &str, principal: &str) -> Result<()> {
        let user_id = native_user_id(namespace, principal)?;
        self.service
            .delete_user(&self.app_name, &user_id)
            .await
            .map_err(memory_error)?;
        self.invalidate().await;
        Ok(())
    }

    /// Remove entries older than `cutoff`, returning the deleted row count.
    ///
    /// The pinned native service exposes exact identity deletes but no age
    /// policy. This owner-level query over its canonical table applies the
    /// configured long-term-memory retention; the cache is invalidated after.
    pub async fn prune_before(&self, cutoff: DateTime<Utc>) -> Result<usize> {
        let result = sqlx::query("DELETE FROM memory_entries WHERE app_name=?1 AND timestamp<?2")
            .bind(&self.app_name)
            .bind(cutoff.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(|error| memory_error(AdkError::memory(error.to_string())))?;
        self.invalidate().await;
        Ok(result.rows_affected() as usize)
    }

    /// Remove every derived memory row whose provenance source ends with one
    /// ingested document identity. Document erasure must reach derivatives even
    /// when the document has no reader in the current request.
    pub async fn erase_document(&self, document_id: &str) -> Result<usize> {
        let escaped = document_id
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%#{escaped}\"%");
        let result = sqlx::query(
            "DELETE FROM memory_entries WHERE app_name=?1 AND author LIKE ?2 ESCAPE '\\'",
        )
        .bind(&self.app_name)
        .bind(pattern)
        .execute(&self.pool)
        .await
        .map_err(|error| memory_error(AdkError::memory(error.to_string())))?;
        self.invalidate().await;
        Ok(result.rows_affected() as usize)
    }

    /// Verify backend connectivity without reading memory content.
    pub async fn health_check(&self) -> Result<()> {
        self.service.health_check().await.map_err(memory_error)
    }

    /// The native app identity used by this store.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }

    async fn add_native(
        &self,
        namespace: &str,
        user_id: &str,
        source: &str,
        text: &str,
        allowed: Vec<String>,
    ) -> Result<MemoryItem> {
        if text.trim().is_empty() {
            return Err(ApiError::new("memory.empty", "memory text is required"));
        }
        if source.trim().is_empty() {
            return Err(ApiError::new(
                "memory.source_required",
                "memory source is required",
            ));
        }
        let id = new_id("mem").replace('-', "");
        let provenance = Provenance {
            v: 1,
            source: source.to_string(),
            allowed,
        };
        let author = serde_json::to_string(&provenance)
            .map_err(|error| ApiError::new("memory.serialize", error.to_string()))?;
        // The identity marker is stored next to the content so erase can target
        // one exact native row. It is stripped before any result is exposed.
        let content = format!("{id}\u{1f}{}", common::redact(text));
        let timestamp = Utc::now();
        let entry = adk_memory::MemoryEntry {
            content: Content::new("user").with_text(content),
            author,
            timestamp,
        };
        self.service
            .add_entry(&self.app_name, user_id, entry)
            .await
            .map_err(memory_error)?;
        self.invalidate().await;
        Ok(MemoryItem {
            id,
            namespace: namespace.to_string(),
            source: source.to_string(),
            text: common::redact(text),
            valid_from: timestamp.timestamp_millis(),
            valid_to: None,
            deleted: false,
        })
    }

    async fn search_scoped(
        &self,
        user_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<RawEntry>> {
        let limit = limit.clamp(1, 100);
        let key = serde_json::to_string(&(user_id, query, limit))
            .map_err(|error| ApiError::new("memory.search", error.to_string()))?;

        let generation = {
            let cache = self.cache.lock().await;
            if let Some((cached, entries)) = cache.entries.get(&key) {
                if *cached == cache.generation {
                    // Cached values are filtered again by the caller before
                    // exposure; the cache never grants scope on its own.
                    return Ok(entries.clone());
                }
            }
            cache.generation
        };

        let response = self
            .service
            .search(SearchRequest {
                query: query.to_string(),
                user_id: user_id.to_string(),
                app_name: self.app_name.clone(),
                limit: Some(limit),
                min_score: None,
                project_id: None,
            })
            .await
            .map_err(memory_error)?;
        let entries: Vec<RawEntry> = response.memories.into_iter().map(parse_entry).collect();
        let mut cache = self.cache.lock().await;
        if cache.generation == generation {
            cache.entries.insert(key, (generation, entries.clone()));
        }
        Ok(entries)
    }

    async fn invalidate(&self) {
        let mut cache = self.cache.lock().await;
        cache.generation = cache.generation.wrapping_add(1);
        cache.entries.clear();
    }
}

/// Parse a native memory entry, stripping the internal identity marker.
fn parse_entry(entry: adk_memory::MemoryEntry) -> RawEntry {
    let content = entry
        .content
        .parts
        .iter()
        .filter_map(|part| match part {
            adk_rust::Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let (id, text) = content
        .split_once('\u{1f}')
        .map(|(id, text)| (id.to_string(), text.to_string()))
        .unwrap_or_else(|| (String::new(), content));
    let provenance = serde_json::from_str::<Provenance>(&entry.author).unwrap_or(Provenance {
        v: 0,
        source: entry.author.clone(),
        allowed: Vec::new(),
    });
    RawEntry {
        id,
        source: provenance.source,
        text,
        allowed: provenance.allowed,
        timestamp: entry.timestamp,
    }
}

fn storage_error(path: &Path, error: AdkError) -> ApiError {
    let message = error.to_string();
    let code = if message.contains("not a database") || message.contains("malformed") {
        "memory.storage_corrupt"
    } else {
        "memory.storage_unavailable"
    };
    ApiError::new(
        code,
        format!("native memory store '{}': {message}", path.display()),
    )
}

fn api_message(error: ApiError) -> String {
    format!("{}: {}", error.code, error.message)
}

fn memory_error(error: AdkError) -> ApiError {
    ApiError::new("memory.unavailable", error.to_string())
}

fn native_entry(entry: RawEntry) -> adk_memory::MemoryEntry {
    adk_memory::MemoryEntry {
        content: Content::new("user").with_text(entry.text),
        author: entry.source,
        timestamp: entry.timestamp,
    }
}

/// Native `MemoryService` surface used by ADK memory tools and the Runner.
///
/// Only identities produced by the runtime's unambiguous encoder are accepted.
/// Calls for another app name are refused rather than reading a foreign scope.
#[async_trait]
impl MemoryService for NativeMemory {
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        _session_id: &str,
        entries: Vec<adk_memory::MemoryEntry>,
    ) -> adk_rust::Result<()> {
        let (namespace, _principal) = self.scope(app_name, user_id)?;
        for entry in entries {
            let text = entry
                .content
                .parts
                .iter()
                .filter_map(|part| match part {
                    adk_rust::Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            self.add_native(&namespace, user_id, &entry.author, &text, Vec::new())
                .await
                .map_err(|error| AdkError::memory(api_message(error)))?;
        }
        Ok(())
    }

    async fn search(&self, req: SearchRequest) -> adk_rust::Result<SearchResponse> {
        if req.project_id.is_some() {
            return Err(AdkError::memory(
                "the native SQLite memory adapter does not implement project scoping",
            ));
        }
        let (_, principal) = self.scope(&req.app_name, &req.user_id)?;
        let raw = self
            .search_scoped(&req.user_id, &req.query, req.limit.unwrap_or(10))
            .await
            .map_err(|error| AdkError::memory(api_message(error)))?;
        let memories = raw
            .into_iter()
            .filter(|entry| {
                entry.allowed.is_empty() || entry.allowed.iter().any(|id| id == &principal)
            })
            .map(native_entry)
            .collect();
        Ok(SearchResponse { memories })
    }

    async fn add_entry(
        &self,
        app_name: &str,
        user_id: &str,
        entry: adk_memory::MemoryEntry,
    ) -> adk_rust::Result<()> {
        let (namespace, _principal) = self.scope(app_name, user_id)?;
        let text = entry
            .content
            .parts
            .iter()
            .filter_map(|part| match part {
                adk_rust::Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        self.add_native(&namespace, user_id, &entry.author, &text, Vec::new())
            .await
            .map_err(|error| AdkError::memory(api_message(error)))?;
        Ok(())
    }

    async fn delete_user(&self, app_name: &str, user_id: &str) -> adk_rust::Result<()> {
        let (namespace, principal) = self.scope(app_name, user_id)?;
        self.erase_principal(&namespace, &principal)
            .await
            .map_err(|error| AdkError::memory(api_message(error)))
    }

    async fn delete_entries(
        &self,
        app_name: &str,
        user_id: &str,
        query: &str,
    ) -> adk_rust::Result<u64> {
        let _ = self.scope(app_name, user_id)?;
        let count = self
            .service
            .delete_entries(app_name, user_id, query)
            .await?;
        self.invalidate().await;
        Ok(count)
    }

    async fn list_recent(
        &self,
        app_name: &str,
        user_id: &str,
        limit: usize,
    ) -> adk_rust::Result<Vec<adk_memory::MemoryEntry>> {
        let (_, principal) = self.scope(app_name, user_id)?;
        let entries = self
            .service
            .list_recent(app_name, user_id, limit)
            .await
            .map_err(|error| AdkError::memory(error.to_string()))?;
        Ok(entries
            .into_iter()
            .map(parse_entry)
            .filter(|entry| {
                entry.allowed.is_empty() || entry.allowed.iter().any(|id| id == &principal)
            })
            .map(native_entry)
            .collect())
    }

    async fn health_check(&self) -> adk_rust::Result<()> {
        self.service.health_check().await
    }
}

impl NativeMemory {
    fn scope(&self, app_name: &str, user_id: &str) -> adk_rust::Result<(String, String)> {
        if app_name != self.app_name {
            return Err(AdkError::memory(format!(
                "memory app scope '{app_name}' is not this store's scope '{}'",
                self.app_name
            )));
        }
        if user_id.starts_with("v1|") {
            return decode_native_user_id(user_id)
                .map_err(|error| AdkError::memory(api_message(error)));
        }
        // Native development routes carry an operator-supplied user id rather
        // than the runtime's authenticated encoding. Keep that identity in its
        // own isolated scope instead of failing the invocation or widening it
        // to another caller's memory.
        Ok((app_name.to_string(), user_id.to_string()))
    }
}

/// Native ADK tool that stores a durable fact through the runtime adapter.
///
/// This is a small tool binding, not another memory engine: storage, retrieval,
/// filtering, and erasure remain the pinned native service and its runtime
/// scope policy.
pub struct RememberMemoryTool {
    memory: Arc<NativeMemory>,
}

impl RememberMemoryTool {
    /// Build the tool for one memory store.
    pub fn new(memory: Arc<NativeMemory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for RememberMemoryTool {
    fn name(&self) -> &str {
        "remember_memory"
    }

    fn description(&self) -> &str {
        "Save a durable fact to long-term memory. Use the caller's own scope; \
         the runtime derives namespace and principal from the authenticated \
         invocation."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "The fact to remember."},
                "source": {"type": "string", "description": "Where the fact came from."}
            },
            "required": ["text", "source"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| AdkError::tool("text is required"))?;
        let source = args
            .get("source")
            .and_then(Value::as_str)
            .filter(|source| !source.trim().is_empty())
            .unwrap_or("agent");
        let namespace = decode_native_user_id(ctx.user_id())
            .map(|(namespace, _)| namespace)
            .unwrap_or_else(|_| ctx.app_name().to_string());
        let item = self
            .memory
            .add_native(&namespace, ctx.user_id(), source, text, Vec::new())
            .await
            .map_err(|error| AdkError::tool(api_message(error)))?;
        Ok(json!({"stored": true, "id": item.id, "source": item.source}))
    }
}

/// Build the native `PreloadMemoryTool` callback for one memory store.
///
/// The callback is a native ADK extension point; the runtime only binds the
/// verified service to it.
pub fn preload_callback(memory: Arc<NativeMemory>) -> adk_rust::Result<BeforeModelCallback> {
    let service: Arc<dyn MemoryService> = memory;
    let tool = adk_tool::memory::PreloadMemoryTool::builder()
        .memory_service(service)
        .max_results(5)
        .build()?;
    Ok(tool.into_before_model_callback())
}
