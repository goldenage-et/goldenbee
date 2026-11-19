//! Durable, package-owned ingestion jobs and source-byte provenance.
//!
//! Jobs live in `<data_dir>/knowledge/jobs.json` and source bytes live under
//! `<data_dir>/knowledge/sources/sha256/<hex>`. Both are written with a
//! temporary file, `fsync`, and an atomic rename, so a restart observes either
//! the previous or the next complete record, never a partial one.
//!
//! The native vector index is a derived cache. On restart it is rebuilt from
//! these stored source bytes and job records; this is not claimed as durable
//! vector storage.

use crate::error::{KnowledgeError, KnowledgeResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

const JOB_SCHEMA: u32 = 1;

/// Lifecycle of one ingested source revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    /// Chunks were indexed and citations are currently servable.
    Indexed,
    /// Indexing failed; a retry may replace this record.
    Failed,
    /// An authorized removal tombstone. Retrieval never serves it again.
    Removed,
}

impl JobStatus {
    /// Stable string for tool output and evidence.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Indexed => "indexed",
            Self::Failed => "failed",
            Self::Removed => "removed",
        }
    }
}

/// One source revision and its index provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    /// Deterministic id derived from namespace, canonical URI, and digest.
    pub job_id: String,
    /// Owning namespace.
    pub namespace: String,
    /// Canonical absolute source path.
    pub source_uri: String,
    /// Digest of the exact bytes under `sources/sha256/<hex>`.
    pub source_digest: String,
    /// Source revision at this layer. Equal to `source_digest`.
    pub source_revision: String,
    /// Display title.
    pub title: String,
    /// Accepted media type.
    pub media_type: String,
    /// Allowed principals. Empty means every principal in the namespace.
    #[serde(default)]
    pub allowed: Vec<String>,
    /// Principal that ingested this revision.
    pub created_by: String,
    /// Current status.
    pub status: JobStatus,
    /// Number of native chunks stored for this revision.
    #[serde(default)]
    pub chunk_count: usize,
    /// Namespace collection name in the native vector store.
    pub index_collection: String,
    /// Native memory ids derived from this source, for authorized removal.
    #[serde(default)]
    pub memory_ids: Vec<String>,
    /// Creation timestamp in milliseconds.
    pub created_at: i64,
    /// Last successful indexing timestamp.
    #[serde(default)]
    pub indexed_at: Option<i64>,
    /// Authorized removal timestamp.
    #[serde(default)]
    pub removed_at: Option<i64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct JobFile {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    jobs: Vec<JobRecord>,
}

/// Atomic job catalogue plus content-addressed source store.
#[derive(Debug)]
pub struct JobStore {
    directory: PathBuf,
    jobs_path: PathBuf,
    sources_dir: PathBuf,
    jobs: BTreeMap<String, JobRecord>,
}

impl JobStore {
    /// Open or create the package store under `directory`.
    pub fn open(directory: &Path) -> KnowledgeResult<Self> {
        let sources_dir = directory.join("sources").join("sha256");
        std::fs::create_dir_all(&sources_dir).map_err(|_| {
            KnowledgeError::new(
                "knowledge.store_unavailable",
                "knowledge store directory could not be created",
            )
        })?;
        let jobs_path = directory.join("jobs.json");
        let jobs = match std::fs::read(&jobs_path) {
            Ok(bytes) => {
                let file: JobFile = serde_json::from_slice(&bytes).map_err(|_| {
                    KnowledgeError::new(
                        "knowledge.store_corrupt",
                        "knowledge jobs.json is not valid JSON",
                    )
                })?;
                if file.schema != 0 && file.schema != JOB_SCHEMA {
                    return Err(KnowledgeError::new(
                        "knowledge.store_corrupt",
                        "knowledge jobs.json uses an unsupported schema",
                    ));
                }
                file.jobs
                    .into_iter()
                    .map(|job| (job.job_id.clone(), job))
                    .collect()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(_) => {
                return Err(KnowledgeError::new(
                    "knowledge.store_unavailable",
                    "knowledge jobs.json is not readable",
                ))
            }
        };
        Ok(Self {
            directory: directory.to_path_buf(),
            jobs_path,
            sources_dir,
            jobs,
        })
    }

    /// Store directory.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// One job by id.
    pub fn get(&self, job_id: &str) -> Option<&JobRecord> {
        self.jobs.get(job_id)
    }

    /// All jobs in deterministic id order.
    pub fn jobs(&self) -> impl Iterator<Item = &JobRecord> {
        self.jobs.values()
    }

    /// Insert or replace a job and persist the catalogue atomically.
    pub fn insert(&mut self, job: JobRecord) -> KnowledgeResult<()> {
        self.jobs.insert(job.job_id.clone(), job);
        self.save()
    }

    /// Permanently remove one catalogue record.
    pub fn remove(&mut self, job_id: &str) -> KnowledgeResult<Option<JobRecord>> {
        let removed = self.jobs.remove(job_id);
        self.save()?;
        Ok(removed)
    }

    /// Whether any remaining job still references one source digest.
    pub fn digest_referenced(&self, digest: &str) -> bool {
        self.jobs
            .values()
            .any(|job| job.status == JobStatus::Indexed && job.source_digest == digest)
    }

    /// Delete the content-addressed source bytes for one digest.
    pub fn delete_source(&self, digest: &str) -> KnowledgeResult<()> {
        let path = self.source_path(digest)?;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(KnowledgeError::new(
                "knowledge.store_unavailable",
                "stored source bytes could not be removed",
            )),
        }
    }

    /// All indexed jobs in one namespace.
    pub fn indexed_for_namespace(&self, namespace: &str) -> Vec<JobRecord> {
        self.jobs
            .values()
            .filter(|job| job.namespace == namespace && job.status == JobStatus::Indexed)
            .cloned()
            .collect()
    }

    /// Active revisions for one canonical URI in a namespace.
    pub fn indexed_for_uri(&self, namespace: &str, source_uri: &str) -> Vec<JobRecord> {
        self.jobs
            .values()
            .filter(|job| {
                job.namespace == namespace
                    && job.source_uri == source_uri
                    && job.status == JobStatus::Indexed
            })
            .cloned()
            .collect()
    }

    /// Persist the content-addressed bytes for one digest.
    pub fn store_source(&self, digest: &str, bytes: &[u8]) -> KnowledgeResult<PathBuf> {
        let path = self.source_path(digest)?;
        if path.exists() {
            return Ok(path);
        }
        let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
        write_atomic(&temporary, &path, bytes)?;
        Ok(path)
    }

    /// Read stored bytes for one digest.
    pub fn read_source(&self, digest: &str) -> KnowledgeResult<Vec<u8>> {
        let path = self.source_path(digest)?;
        let bytes = std::fs::read(&path).map_err(|_| {
            KnowledgeError::new(
                "knowledge.source_missing",
                "stored source bytes are missing from the knowledge store",
            )
        })?;
        if common::bytes_digest(&bytes) != digest {
            return Err(KnowledgeError::new(
                "knowledge.source_digest_mismatch",
                "stored source bytes no longer match their digest",
            ));
        }
        Ok(bytes)
    }

    fn source_path(&self, digest: &str) -> KnowledgeResult<PathBuf> {
        let hex = digest.strip_prefix("sha256:").ok_or_else(|| {
            KnowledgeError::new(
                "knowledge.store_corrupt",
                "source digest is not a sha256 content address",
            )
        })?;
        if hex.is_empty()
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(KnowledgeError::new(
                "knowledge.store_corrupt",
                "source digest is not a sha256 content address",
            ));
        }
        Ok(self.sources_dir.join(hex))
    }

    fn save(&self) -> KnowledgeResult<()> {
        let file = JobFile {
            schema: JOB_SCHEMA,
            jobs: self.jobs.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|_| {
            KnowledgeError::new(
                "knowledge.store_unavailable",
                "knowledge jobs could not be serialized",
            )
        })?;
        let temporary = self
            .jobs_path
            .with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4().simple()));
        write_atomic(&temporary, &self.jobs_path, &bytes)
    }
}

fn write_atomic(temporary: &Path, destination: &Path, bytes: &[u8]) -> KnowledgeResult<()> {
    let write = || -> std::io::Result<()> {
        let mut file = File::create(temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(temporary, destination)?;
        if let Some(parent) = destination.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    };
    write().map_err(|_| {
        let _ = std::fs::remove_file(temporary);
        KnowledgeError::new(
            "knowledge.store_unavailable",
            "knowledge store could not be written atomically",
        )
    })
}
