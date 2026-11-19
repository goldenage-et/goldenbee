//! Ingestion lifecycle, native RAG index, ACL filtering, and citations.
//!
//! The retrieval engine is the pinned native `adk-rag` pipeline. This module
//! owns only package concerns: the authorized source adapter, durable job
//! provenance, namespace collections, ACL checks before exposure, and citation
//! mapping back to a stored source revision.

use crate::embedding::HashingEmbeddingProvider;
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::jobs::{JobRecord, JobStatus, JobStore};
use crate::source::{FileSource, DEFAULT_MAX_DOCUMENT_BYTES};
use adk_rust::rag::{
    Chunk, Chunker, Document, FixedSizeChunker, InMemoryVectorStore, RagConfig, RagPipeline,
};
use runtime::native::{decode_native_user_id, NativeMemory};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, MutexGuard};

/// Package settings resolved from the operator `[settings]` table.
#[derive(Debug, Clone)]
pub struct KnowledgeConfig {
    /// Operator-configured namespace used by the native development launcher,
    /// which binds its console session to the upstream `user` identity.
    pub namespace: String,
    /// Canonical authorized source roots.
    pub source_roots: Vec<PathBuf>,
    /// Maximum accepted document bytes.
    pub max_document_bytes: usize,
    /// Native chunk size in UTF-8 bytes.
    pub chunk_size: usize,
    /// Maximum chunk count for one document.
    pub max_chunks: usize,
    /// Default result count.
    pub top_k: usize,
    /// Hard result count ceiling.
    pub max_top_k: usize,
    /// Optional package policy that denies matching source URIs before a tool
    /// runs. This is a hook/callback boundary, not a second retrieval path.
    pub deny_source_contains: Option<String>,
}

impl KnowledgeConfig {
    /// Resolve settings against the configuration file's directory.
    pub fn from_settings(
        config_dir: &Path,
        namespace: &str,
        settings: &BTreeMap<String, String>,
    ) -> KnowledgeResult<Self> {
        let roots = settings
            .get("source_roots")
            .ok_or_else(|| {
                KnowledgeError::new(
                    "knowledge.source_unavailable",
                    "settings.source_roots is required and must name at least one readable directory",
                )
            })?
            .split([',', '\n'])
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                let path = PathBuf::from(value);
                if path.is_absolute() {
                    path
                } else {
                    config_dir.join(path)
                }
            })
            .map(|path| {
                std::fs::canonicalize(&path).map_err(|_| {
                    KnowledgeError::new(
                        "knowledge.source_unavailable",
                        format!("source root '{}' is not readable", path.display()),
                    )
                })
            })
            .collect::<KnowledgeResult<Vec<_>>>()?;
        if roots.is_empty() {
            return Err(KnowledgeError::new(
                "knowledge.source_unavailable",
                "settings.source_roots resolves no directories",
            ));
        }
        for root in &roots {
            if !root.is_dir() {
                return Err(KnowledgeError::new(
                    "knowledge.source_unavailable",
                    format!("source root '{}' is not a directory", root.display()),
                ));
            }
        }
        let number = |key: &str, default: usize| -> KnowledgeResult<usize> {
            match settings.get(key) {
                Some(value) => value.parse::<usize>().map_err(|_| {
                    KnowledgeError::new(
                        "knowledge.config_invalid",
                        format!("settings.{key} must be a positive integer"),
                    )
                }),
                None => Ok(default),
            }
        };
        let max_document_bytes = number("max_document_bytes", DEFAULT_MAX_DOCUMENT_BYTES)?;
        let chunk_size = number("chunk_size", 512)?;
        let max_chunks = number("max_chunks", 10_000)?;
        let top_k = number("top_k", 5)?;
        let max_top_k = number("max_top_k", 20)?;
        if max_document_bytes == 0 || chunk_size == 0 || max_chunks == 0 || top_k == 0 {
            return Err(KnowledgeError::new(
                "knowledge.config_invalid",
                "document, chunk, and result limits must be greater than zero",
            ));
        }
        Ok(Self {
            namespace: namespace.to_string(),
            source_roots: roots,
            max_document_bytes,
            chunk_size,
            max_chunks,
            top_k: top_k.min(max_top_k.max(1)),
            max_top_k: max_top_k.max(1),
            deny_source_contains: settings
                .get("deny_source_contains")
                .cloned()
                .filter(|value| !value.is_empty()),
        })
    }
}

/// Shared startup-owned service handle used by tools, hooks, and callbacks.
#[derive(Clone)]
pub struct KnowledgeCell {
    inner: Arc<Mutex<Option<KnowledgeService>>>,
}

impl Default for KnowledgeCell {
    fn default() -> Self {
        Self::new()
    }
}

impl KnowledgeCell {
    /// Create an uninitialized cell. Startup installs the service.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    /// Install the service after configuration, roots, and storage resolve.
    pub async fn install(&self, service: KnowledgeService) {
        *self.inner.lock().await = Some(service);
    }

    /// Borrow the service for one serialized package operation.
    pub async fn lock(&self) -> MutexGuard<'_, Option<KnowledgeService>> {
        self.inner.lock().await
    }

    /// Drop the service at shutdown.
    pub async fn clear(&self) {
        *self.inner.lock().await = None;
    }

    /// Return an actionable denial when package policy or source containment
    /// rejects a URI. Used by the native before-tool callback so a denied
    /// source is denied before the toolset executes.
    pub async fn denial(&self, uri: &str) -> Option<KnowledgeError> {
        let guard = self.inner.lock().await;
        let service = guard.as_ref()?;
        service.denial(uri)
    }

    /// Return a package-policy denial for an authorized removal.
    pub async fn removal_denial(&self, uri: &str) -> Option<KnowledgeError> {
        let guard = self.inner.lock().await;
        let service = guard.as_ref()?;
        service.removal_denial(uri)
    }
}

/// One servable citation resolved against current job and source state.
#[derive(Debug, Clone, Serialize)]
pub struct SourceCitation {
    /// Deterministic citation identity.
    pub citation_id: String,
    /// Ingested document revision identity.
    pub document_id: String,
    /// Canonical source path.
    pub source_uri: String,
    /// Digest of the exact source revision.
    pub source_digest: String,
    /// Source revision at this layer.
    pub source_revision: String,
    /// Native chunk id.
    pub chunk_id: String,
    /// Retrieved chunk text.
    pub text: String,
    /// Native similarity score.
    pub score: f32,
    /// UTF-8 byte span start in the source.
    pub span_start: usize,
    /// UTF-8 byte span end in the source.
    pub span_end: usize,
}

impl SourceCitation {
    /// Stable answer citation token.
    pub fn token(&self) -> String {
        format!(
            "{}@{}#{}",
            self.source_uri, self.source_digest, self.chunk_id
        )
    }
}

/// Successful ingestion result.
#[derive(Debug, Clone)]
pub struct IngestOutcome {
    /// Durable job record.
    pub job: JobRecord,
    /// True when an identical source revision already existed.
    pub converged: bool,
    /// Source bytes covered by chunks.
    pub covered_bytes: usize,
    /// Total source bytes.
    pub source_bytes: usize,
    /// Source bytes that could not be chunked (always zero on success).
    pub uncovered_bytes: usize,
}

/// Query result including stale observations and denied candidates.
#[derive(Debug, Clone)]
pub struct QueryOutcome {
    /// Authorized citations after ACL, status, and digest checks.
    pub citations: Vec<SourceCitation>,
    /// Candidates dropped because their source changed or was removed.
    pub stale: Vec<serde_json::Value>,
}

/// Package lifecycle owner for one Worker Instance.
pub struct KnowledgeService {
    /// Resolved settings.
    pub config: KnowledgeConfig,
    /// Authorized source adapter.
    pub source: FileSource,
    /// Durable job and source store.
    pub jobs: JobStore,
    pipeline: RagPipeline,
    chunker: Arc<FixedSizeChunker>,
    memory: Option<Arc<NativeMemory>>,
    loaded: BTreeSet<String>,
}

impl KnowledgeService {
    /// Open the service from resolved configuration and the instance runtime.
    pub fn open(
        config_dir: &Path,
        data_dir: &Path,
        namespace: &str,
        settings: &BTreeMap<String, String>,
        memory: Option<Arc<NativeMemory>>,
    ) -> KnowledgeResult<Self> {
        let config = KnowledgeConfig::from_settings(config_dir, namespace, settings)?;
        let jobs = JobStore::open(&data_dir.join("knowledge"))?;
        let chunker = Arc::new(
            FixedSizeChunker::try_new(config.chunk_size, 0).map_err(|_| {
                KnowledgeError::new(
                    "knowledge.config_invalid",
                    "settings.chunk_size must be greater than zero",
                )
            })?,
        );
        let rag_config = RagConfig::builder()
            .chunk_size(config.chunk_size)
            .chunk_overlap(0)
            .top_k(config.max_top_k.max(config.top_k).max(20))
            .similarity_threshold(0.0)
            .build()
            .map_err(|_| {
                KnowledgeError::new(
                    "knowledge.config_invalid",
                    "native RAG configuration is invalid",
                )
            })?;
        let pipeline = RagPipeline::builder()
            .config(rag_config)
            .embedding_provider(Arc::new(HashingEmbeddingProvider::new()))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(chunker.clone())
            .build()
            .map_err(|_| {
                KnowledgeError::new(
                    "knowledge.index_unavailable",
                    "native RAG pipeline could not be composed",
                )
            })?;
        let source = FileSource {
            roots: config.source_roots.clone(),
            max_document_bytes: config.max_document_bytes,
        };
        Ok(Self {
            config,
            source,
            jobs,
            pipeline,
            chunker,
            memory,
            loaded: BTreeSet::new(),
        })
    }

    /// Resolve the namespace and principal bound at admission.
    ///
    /// The upstream native development launcher creates its console session
    /// with the fixed `user` identity; that path uses the operator-configured
    /// namespace and a `user` principal. Server and task execution always use
    /// the encoded native identity.
    pub fn identity(&self, user_id: &str) -> KnowledgeResult<(String, String)> {
        match decode_native_user_id(user_id) {
            Ok(identity) => Ok(identity),
            Err(_) if user_id == "user" => Ok((self.config.namespace.clone(), "user".into())),
            Err(error) => Err(KnowledgeError::new(
                error.code,
                "native session identity could not be resolved",
            )),
        }
    }

    /// Callback-boundary denial: package policy plus path containment.
    pub fn denial(&self, uri: &str) -> Option<KnowledgeError> {
        if let Some(denial) = self.policy_denial(uri) {
            return Some(denial);
        }
        self.source.resolve(uri).err()
    }

    /// Denial for an authorized removal.
    ///
    /// A removal tombstone must remain possible after the source file itself is
    /// gone, so only the package deny policy applies here. Job lookup still
    /// limits removal to the caller's namespace and authorized revisions.
    pub fn removal_denial(&self, uri: &str) -> Option<KnowledgeError> {
        self.policy_denial(uri)
    }

    fn policy_denial(&self, uri: &str) -> Option<KnowledgeError> {
        let needle = self.config.deny_source_contains.as_ref()?;
        uri.contains(needle).then(|| {
            KnowledgeError::new(
                "knowledge.source_denied",
                "source is denied by package policy",
            )
        })
    }

    /// Ingest one authorized source revision.
    #[allow(clippy::too_many_arguments)]
    pub async fn ingest(
        &mut self,
        principal: &str,
        namespace: &str,
        uri: &str,
        title: Option<&str>,
        allowed: Vec<String>,
    ) -> KnowledgeResult<IngestOutcome> {
        let source = self.source.open(uri)?;
        let canonical = source.uri.to_string_lossy().to_string();
        if let Some(denied) = self.policy_denial(&canonical) {
            return Err(denied);
        }
        let digest = source.digest.clone();
        let job_id = job_id(namespace, &canonical, &digest);
        if let Some(existing) = self.jobs.get(&job_id).cloned() {
            if existing.status == JobStatus::Indexed {
                return Ok(IngestOutcome {
                    covered_bytes: source.text.len(),
                    source_bytes: source.text.len(),
                    uncovered_bytes: 0,
                    job: existing,
                    converged: true,
                });
            }
        }

        self.jobs.store_source(&digest, &source.bytes)?;
        self.ensure_loaded(namespace).await?;
        let collection = collection_name(namespace);
        let previous = self.jobs.get(&job_id).cloned();
        self.delete_job_chunks(&collection, previous.as_ref())
            .await?;

        let title = title
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                source
                    .uri
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("document")
                    .to_string()
            });
        let mut metadata = BTreeMap::new();
        metadata.insert("job_id".into(), job_id.clone());
        metadata.insert("namespace".into(), namespace.to_string());
        metadata.insert("source_uri".into(), canonical.clone());
        metadata.insert("source_digest".into(), digest.clone());
        metadata.insert("source_revision".into(), digest.clone());
        metadata.insert("media_type".into(), source.media_type.clone());
        let document = Document {
            id: job_id.clone(),
            text: source.text.clone(),
            metadata: metadata.into_iter().collect(),
            source_uri: Some(canonical.clone()),
        };
        let preview = self.chunker.chunk(&document);
        if preview.len() > self.config.max_chunks {
            return Err(KnowledgeError::new(
                "knowledge.chunk_limit",
                "document produces more chunks than the configured limit",
            )
            .with("limit_chunks", self.config.max_chunks.to_string())
            .with("chunks", preview.len().to_string()));
        }
        let spans = validate_coverage(&source.text, &preview)?;
        let mut chunks = self
            .pipeline
            .ingest(&collection, &document)
            .await
            .map_err(|_| {
                KnowledgeError::new(
                    "knowledge.index_failed",
                    "native RAG pipeline could not index the document",
                )
            })?;
        if chunks.len() != spans.len() {
            return Err(KnowledgeError::new(
                "knowledge.chunk_gap",
                "native chunker produced an unexpected chunk count",
            ));
        }
        for (chunk, (start, end)) in chunks.iter_mut().zip(&spans) {
            chunk
                .metadata
                .insert("span_start".into(), start.to_string());
            chunk.metadata.insert("span_end".into(), end.to_string());
        }
        self.pipeline
            .vector_store()
            .upsert(&collection, &chunks)
            .await
            .map_err(|_| {
                KnowledgeError::new(
                    "knowledge.index_failed",
                    "native vector store rejected indexed chunks",
                )
            })?;
        let memory_ids = self
            .derive_memory(principal, namespace, &canonical, &digest, &chunks, &allowed)
            .await;
        self.supersede_previous(&collection, namespace, &canonical, &job_id)
            .await?;
        let now = common::now_ms();
        let job = JobRecord {
            job_id,
            namespace: namespace.to_string(),
            source_uri: canonical,
            source_digest: digest.clone(),
            source_revision: digest,
            title,
            media_type: source.media_type,
            allowed,
            created_by: principal.to_string(),
            status: JobStatus::Indexed,
            chunk_count: chunks.len(),
            index_collection: collection,
            memory_ids,
            created_at: previous.as_ref().map(|job| job.created_at).unwrap_or(now),
            indexed_at: Some(now),
            removed_at: None,
        };
        self.jobs.insert(job.clone())?;
        let covered_bytes = spans.last().map(|(_, end)| *end).unwrap_or(0);
        Ok(IngestOutcome {
            job,
            converged: false,
            covered_bytes,
            source_bytes: source.text.len(),
            uncovered_bytes: source.text.len().saturating_sub(covered_bytes),
        })
    }

    /// Query one namespace after loading indexed sources from durable state.
    pub async fn query(
        &mut self,
        principal: &str,
        namespace: &str,
        query: &str,
        top_k: Option<usize>,
    ) -> KnowledgeResult<QueryOutcome> {
        self.ensure_loaded(namespace).await?;
        let collection = collection_name(namespace);
        self.pipeline
            .create_collection(&collection)
            .await
            .map_err(|_| {
                KnowledgeError::new(
                    "knowledge.index_unavailable",
                    "namespace collection could not be opened",
                )
            })?;
        let requested = top_k.unwrap_or(self.config.top_k);
        let limit = requested.clamp(1, self.config.max_top_k);
        let results = self.pipeline.query(&collection, query).await.map_err(|_| {
            KnowledgeError::new("knowledge.query_failed", "native RAG query failed")
        })?;
        let mut citations = Vec::new();
        let mut stale = Vec::new();
        let mut seen = BTreeSet::new();
        for result in results {
            let job = match self.jobs.get(&result.chunk.document_id).cloned() {
                Some(job) => job,
                None => {
                    stale.push(serde_json::json!({"reason": "job_missing"}));
                    continue;
                }
            };
            if job.namespace != namespace {
                // A cross-namespace candidate is never observable.
                continue;
            }
            if !job.allowed.is_empty() && !job.allowed.iter().any(|id| id == principal) {
                // Denied candidates contribute no text, URI, or count detail.
                continue;
            }
            if job.status != JobStatus::Indexed {
                stale.push(serde_json::json!({
                    "source_uri": job.source_uri,
                    "reason": "removed",
                }));
                continue;
            }
            let current = self.source.current_digest(&job.source_uri);
            let stale_reason = match &current {
                Ok(digest) if digest == &job.source_digest => None,
                Ok(_) => Some("source_changed"),
                Err(_) => Some("source_missing"),
            };
            if let Some(reason) = stale_reason {
                stale.push(serde_json::json!({
                    "source_uri": job.source_uri,
                    "reason": reason,
                }));
                continue;
            }
            if result
                .chunk
                .metadata
                .get("source_digest")
                .is_some_and(|digest| digest != &job.source_digest)
            {
                stale.push(serde_json::json!({
                    "source_uri": job.source_uri,
                    "reason": "citation_digest_mismatch",
                }));
                continue;
            }
            let chunk_index = result
                .chunk
                .metadata
                .get("chunk_index")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_default();
            let span_start = result
                .chunk
                .metadata
                .get("span_start")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_default();
            let span_end = result
                .chunk
                .metadata
                .get("span_end")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(span_start);
            let citation = SourceCitation {
                citation_id: format!("{}#{chunk_index}", job.job_id),
                document_id: job.job_id.clone(),
                source_uri: job.source_uri.clone(),
                source_digest: job.source_digest.clone(),
                source_revision: job.source_revision.clone(),
                chunk_id: result.chunk.id.clone(),
                text: result.chunk.text.clone(),
                score: result.score,
                span_start,
                span_end,
            };
            if seen.insert(citation.citation_id.clone()) {
                citations.push(citation);
            }
            if citations.len() >= limit {
                break;
            }
        }
        Ok(QueryOutcome { citations, stale })
    }

    /// Erase every source revision ingested by one principal in one namespace.
    ///
    /// Chunks, catalogue rows, source bytes that no other revision references,
    /// and derived memory ids are removed. Other principals' shared sources
    /// remain indexed.
    pub async fn erase_principal(
        &mut self,
        namespace: &str,
        principal: &str,
    ) -> KnowledgeResult<usize> {
        let candidates: Vec<JobRecord> = self
            .jobs
            .jobs()
            .filter(|job| job.namespace == namespace && job.created_by == principal)
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(0);
        }
        self.ensure_loaded(namespace).await?;
        let collection = collection_name(namespace);
        let mut removed = 0usize;
        for job in candidates {
            self.delete_job_chunks(&collection, Some(&job)).await?;
            if let Some(memory) = &self.memory {
                let memory_principal =
                    common::Principal::new(job.created_by.clone(), namespace, &["*"]);
                for memory_id in &job.memory_ids {
                    let _ = memory.erase(&memory_principal, memory_id).await;
                }
            }
            self.jobs.remove(&job.job_id)?;
            if !self.jobs.digest_referenced(&job.source_digest) {
                self.jobs.delete_source(&job.source_digest)?;
            }
            removed += 1;
        }
        Ok(removed)
    }

    /// Apply package source retention by removing indexed jobs and their
    /// derived source bytes older than `cutoff_ms`.
    pub async fn retention(&mut self, cutoff_ms: i64) -> KnowledgeResult<usize> {
        let candidates: Vec<JobRecord> = self
            .jobs
            .jobs()
            .filter(|job| job.created_at < cutoff_ms)
            .cloned()
            .collect();
        let mut namespaces: BTreeSet<String> = BTreeSet::new();
        for job in &candidates {
            namespaces.insert(job.namespace.clone());
        }
        for namespace in &namespaces {
            self.ensure_loaded(namespace).await?;
        }
        let mut removed = 0usize;
        for job in candidates {
            let collection = collection_name(&job.namespace);
            self.delete_job_chunks(&collection, Some(&job)).await?;
            if let Some(memory) = &self.memory {
                let memory_principal =
                    common::Principal::new(job.created_by.clone(), &job.namespace, &["*"]);
                for memory_id in &job.memory_ids {
                    let _ = memory.erase(&memory_principal, memory_id).await;
                }
            }
            self.jobs.remove(&job.job_id)?;
            if !self.jobs.digest_referenced(&job.source_digest) {
                self.jobs.delete_source(&job.source_digest)?;
            }
            removed += 1;
        }
        Ok(removed)
    }

    /// Authorized removal of every active revision of one source URI.
    pub async fn remove(
        &mut self,
        principal: &str,
        namespace: &str,
        uri: &str,
    ) -> KnowledgeResult<Vec<JobRecord>> {
        let target = self.removal_target(uri);
        let candidates: Vec<JobRecord> = self
            .jobs
            .indexed_for_namespace(namespace)
            .into_iter()
            .filter(|job| same_source(&job.source_uri, &target))
            .filter(|job| {
                job.created_by == principal
                    || job.allowed.is_empty()
                    || job.allowed.iter().any(|id| id == principal)
            })
            .collect();
        if candidates.is_empty() {
            return Err(KnowledgeError::new(
                "knowledge.source_not_found",
                "no removable indexed source matches this URI",
            ));
        }
        self.ensure_loaded(namespace).await?;
        let collection = collection_name(namespace);
        let mut removed = Vec::new();
        for mut job in candidates {
            self.delete_job_chunks(&collection, Some(&job)).await?;
            job.status = JobStatus::Removed;
            job.removed_at = Some(common::now_ms());
            if let Some(memory) = &self.memory {
                let memory_principal = common::Principal::new(principal, namespace, &["*"]);
                for memory_id in &job.memory_ids {
                    let _ = memory.erase(&memory_principal, memory_id).await;
                }
            }
            self.jobs.insert(job.clone())?;
            removed.push(job);
        }
        Ok(removed)
    }

    async fn ensure_loaded(&mut self, namespace: &str) -> KnowledgeResult<()> {
        if self.loaded.contains(namespace) {
            return Ok(());
        }
        let collection = collection_name(namespace);
        self.pipeline
            .create_collection(&collection)
            .await
            .map_err(|_| {
                KnowledgeError::new(
                    "knowledge.index_unavailable",
                    "namespace collection could not be created",
                )
            })?;
        for job in self.jobs.indexed_for_namespace(namespace) {
            if job.chunk_count == 0 {
                continue;
            }
            let bytes = match self.jobs.read_source(&job.source_digest) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let text = match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => continue,
            };
            let mut metadata = BTreeMap::new();
            metadata.insert("job_id".into(), job.job_id.clone());
            metadata.insert("namespace".into(), job.namespace.clone());
            metadata.insert("source_uri".into(), job.source_uri.clone());
            metadata.insert("source_digest".into(), job.source_digest.clone());
            metadata.insert("source_revision".into(), job.source_revision.clone());
            metadata.insert("media_type".into(), job.media_type.clone());
            let document = Document {
                id: job.job_id.clone(),
                text: text.clone(),
                metadata: metadata.into_iter().collect(),
                source_uri: Some(job.source_uri.clone()),
            };
            let preview = self.chunker.chunk(&document);
            let spans = match validate_coverage(&text, &preview) {
                Ok(spans) => spans,
                Err(_) => continue,
            };
            let mut chunks = match self.pipeline.ingest(&collection, &document).await {
                Ok(chunks) => chunks,
                Err(_) => continue,
            };
            if chunks.len() != spans.len() {
                continue;
            }
            for (chunk, (start, end)) in chunks.iter_mut().zip(&spans) {
                chunk
                    .metadata
                    .insert("span_start".into(), start.to_string());
                chunk.metadata.insert("span_end".into(), end.to_string());
            }
            let _ = self
                .pipeline
                .vector_store()
                .upsert(&collection, &chunks)
                .await;
        }
        self.loaded.insert(namespace.to_string());
        Ok(())
    }

    async fn derive_memory(
        &self,
        principal: &str,
        namespace: &str,
        source_uri: &str,
        digest: &str,
        chunks: &[Chunk],
        allowed: &[String],
    ) -> Vec<String> {
        let Some(memory) = &self.memory else {
            return Vec::new();
        };
        let memory_principal = common::Principal::new(principal, namespace, &["*"]);
        let mut ids = Vec::new();
        for chunk in chunks {
            let label = format!("{}@{}#{}", source_uri, digest, chunk.id);
            if let Ok(item) = memory
                .remember_with_allowed(
                    &memory_principal,
                    namespace,
                    &label,
                    &chunk.text,
                    allowed.to_vec(),
                )
                .await
            {
                ids.push(item.id);
            }
        }
        ids
    }

    /// Remove earlier indexed revisions of the same canonical URI after a new
    /// revision is fully indexed, so the index never serves two revisions of
    /// one source under a live job.
    async fn supersede_previous(
        &mut self,
        collection: &str,
        namespace: &str,
        source_uri: &str,
        current_job: &str,
    ) -> KnowledgeResult<()> {
        let previous: Vec<JobRecord> = self
            .jobs
            .indexed_for_uri(namespace, source_uri)
            .into_iter()
            .filter(|job| job.job_id != current_job)
            .collect();
        for mut job in previous {
            self.delete_job_chunks(collection, Some(&job)).await?;
            if let Some(memory) = &self.memory {
                let principal = common::Principal::new(job.created_by.clone(), namespace, &["*"]);
                for memory_id in &job.memory_ids {
                    let _ = memory.erase(&principal, memory_id).await;
                }
            }
            job.status = JobStatus::Removed;
            job.removed_at = Some(common::now_ms());
            self.jobs.insert(job)?;
        }
        Ok(())
    }

    async fn delete_job_chunks(
        &self,
        collection: &str,
        job: Option<&JobRecord>,
    ) -> KnowledgeResult<()> {
        let Some(job) = job else {
            return Ok(());
        };
        if job.chunk_count == 0 {
            return Ok(());
        }
        let ids: Vec<String> = (0..job.chunk_count)
            .map(|index| format!("{}_{index}", job.job_id))
            .collect();
        let borrowed: Vec<&str> = ids.iter().map(String::as_str).collect();
        let _ = self.pipeline.create_collection(collection).await;
        let _ = self
            .pipeline
            .vector_store()
            .delete(collection, &borrowed)
            .await;
        Ok(())
    }

    fn removal_target(&self, uri: &str) -> String {
        match self.source.resolve(uri) {
            Ok(path) => path.to_string_lossy().to_string(),
            Err(_) => {
                let path = PathBuf::from(uri);
                if path.is_absolute() {
                    path.to_string_lossy().to_string()
                } else {
                    self.source
                        .roots
                        .first()
                        .map(|root| root.join(&path))
                        .unwrap_or(path)
                        .to_string_lossy()
                        .to_string()
                }
            }
        }
    }
}

fn same_source(left: &str, target: &str) -> bool {
    left == target
        || std::fs::canonicalize(left)
            .map(|path| path.to_string_lossy() == target)
            .unwrap_or(false)
}

fn collection_name(namespace: &str) -> String {
    let digest = Sha256::digest(namespace.as_bytes());
    format!("ns-{}", hex(&digest[..16]))
}

fn job_id(namespace: &str, uri: &str, digest: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update([0]);
    hasher.update(uri.as_bytes());
    hasher.update([0]);
    hasher.update(digest.as_bytes());
    format!("job-{}", hex(&hasher.finalize()[..12]))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn validate_coverage(text: &str, chunks: &[Chunk]) -> KnowledgeResult<Vec<(usize, usize)>> {
    let mut spans = Vec::with_capacity(chunks.len());
    let mut cursor: usize = 0;
    for (index, chunk) in chunks.iter().enumerate() {
        if chunk.id != format!("{}_{index}", chunk.document_id) {
            return Err(KnowledgeError::new(
                "knowledge.chunk_gap",
                "native chunk identity is not deterministic",
            ));
        }
        let end = cursor
            .checked_add(chunk.text.len())
            .ok_or_else(|| KnowledgeError::new("knowledge.chunk_gap", "chunk span overflow"))?;
        if text.get(cursor..end) != Some(chunk.text.as_str()) {
            return Err(KnowledgeError::new(
                "knowledge.chunk_gap",
                "native chunks do not cover the source text without gaps",
            )
            .with("chunk_index", index.to_string()));
        }
        spans.push((cursor, end));
        cursor = end;
    }
    if cursor != text.len() {
        return Err(KnowledgeError::new(
            "knowledge.chunk_gap",
            "native chunks do not cover the complete source text",
        )
        .with("covered_bytes", cursor.to_string())
        .with("source_bytes", text.len().to_string()));
    }
    Ok(spans)
}
