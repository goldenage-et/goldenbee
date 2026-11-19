//! Package-owned authorized file source adapter.
//!
//! Contract:
//!
//! * A source URI is either absolute or relative to the first configured root.
//! * The canonical target must stay inside one configured root.
//! * A symlink target is refused; only a regular file is accepted.
//! * Only UTF-8 `.txt` and `.md` files are supported in this slice.
//! * The adapter never reads past `max_document_bytes + 1`; oversize input is
//!   rejected instead of silently truncated.
//! * Invalid UTF-8 returns `knowledge.partial_extraction` with the first
//!   invalid byte offset and indexes nothing.
//!
//! This is the only ingestion source in the package. Retrieval itself is owned
//! by the pinned native `adk-rag` pipeline.

use crate::error::{KnowledgeError, KnowledgeResult};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Byte ceiling applied to one document before it is read completely.
pub const DEFAULT_MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

/// Authorized document source rooted at operator-selected directories.
#[derive(Debug, Clone)]
pub struct FileSource {
    /// Canonical roots.
    pub roots: Vec<PathBuf>,
    /// Maximum accepted source length in bytes.
    pub max_document_bytes: usize,
}

/// One opened document with extracted text and provenance.
#[derive(Debug, Clone)]
pub struct OpenedSource {
    /// Canonical absolute source path.
    pub uri: PathBuf,
    /// Digest of the exact bytes read.
    pub digest: String,
    /// Media type accepted by the adapter.
    pub media_type: String,
    /// Exact source bytes.
    pub bytes: Vec<u8>,
    /// UTF-8 extracted text.
    pub text: String,
}

impl FileSource {
    /// Create an adapter over canonical roots. Roots were validated at startup.
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            max_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
        }
    }

    /// Override the byte ceiling.
    pub fn with_max_document_bytes(mut self, bytes: usize) -> Self {
        self.max_document_bytes = bytes.max(1);
        self
    }

    /// Resolve and validate one source URI without reading its content.
    pub fn resolve(&self, uri: &str) -> KnowledgeResult<PathBuf> {
        let requested = PathBuf::from(uri.trim());
        if uri.trim().is_empty() {
            return Err(KnowledgeError::new(
                "knowledge.source_invalid",
                "a source URI is required",
            ));
        }
        let candidate = if requested.is_absolute() {
            requested
        } else {
            self.roots
                .first()
                .ok_or_else(|| {
                    KnowledgeError::new(
                        "knowledge.source_unavailable",
                        "no source root is configured for relative URIs",
                    )
                })?
                .join(requested)
        };
        let metadata = std::fs::symlink_metadata(&candidate).map_err(|_| {
            KnowledgeError::new("knowledge.source_not_found", "source is not readable")
        })?;
        if metadata.file_type().is_symlink() {
            return Err(KnowledgeError::new(
                "knowledge.source_symlink",
                "symbolic-link sources are not accepted",
            ));
        }
        let canonical = std::fs::canonicalize(&candidate).map_err(|_| {
            KnowledgeError::new("knowledge.source_not_found", "source is not readable")
        })?;
        if !self
            .roots
            .iter()
            .any(|root| canonical.starts_with(root) && &canonical != root)
        {
            return Err(KnowledgeError::new(
                "knowledge.source_outside_roots",
                "source resolves outside every configured source root",
            ));
        }
        let canonical_metadata = std::fs::metadata(&canonical).map_err(|_| {
            KnowledgeError::new(
                "knowledge.source_not_found",
                "source metadata is not readable",
            )
        })?;
        if !canonical_metadata.is_file() {
            return Err(KnowledgeError::new(
                "knowledge.source_not_file",
                "source is not a regular file",
            ));
        }
        Ok(canonical)
    }

    /// Read and classify one authorized source.
    pub fn open(&self, uri: &str) -> KnowledgeResult<OpenedSource> {
        let canonical = self.resolve(uri)?;
        let metadata = std::fs::metadata(&canonical).map_err(|_| {
            KnowledgeError::new(
                "knowledge.source_not_found",
                "source metadata is not readable",
            )
        })?;
        let media_type = media_type_for(&canonical);
        if media_type.is_none() {
            return Err(KnowledgeError::new(
                "knowledge.unsupported_format",
                "only UTF-8 .txt and .md documents are supported",
            )
            .with("detected_media_type", detected_media_type(&canonical))
            .with("extension", extension_of(&canonical)));
        }
        let media_type = media_type.unwrap_or("text/plain");
        if metadata.len() > self.max_document_bytes as u64 {
            return Err(KnowledgeError::new(
                "knowledge.document_too_large",
                "document exceeds the configured ingestion limit",
            )
            .with("limit_bytes", self.max_document_bytes.to_string())
            .with("source_bytes", metadata.len().to_string()));
        }
        let mut bytes = Vec::new();
        File::open(&canonical)
            .and_then(|file| {
                file.take(self.max_document_bytes as u64 + 1)
                    .read_to_end(&mut bytes)
            })
            .map_err(|_| {
                KnowledgeError::new("knowledge.source_not_found", "source is not readable")
            })?;
        if bytes.len() > self.max_document_bytes {
            return Err(KnowledgeError::new(
                "knowledge.document_too_large",
                "document grew past the configured ingestion limit while being read",
            )
            .with("limit_bytes", self.max_document_bytes.to_string()));
        }
        if looks_binary(&bytes) {
            return Err(KnowledgeError::new(
                "knowledge.unsupported_format",
                "source is not UTF-8 text",
            )
            .with("detected_media_type", "application/octet-stream"));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|error| {
                KnowledgeError::new(
                    "knowledge.partial_extraction",
                    "source is not valid UTF-8; no truncated text was indexed",
                )
                .with("invalid_offset", error.valid_up_to().to_string())
            })?
            .to_string();
        Ok(OpenedSource {
            digest: common::bytes_digest(&bytes),
            uri: canonical,
            media_type: media_type.to_string(),
            bytes,
            text,
        })
    }

    /// Digest the current bytes at a stored source URI.
    ///
    /// Retrieval calls this before exposing a chunk. A changed, removed, or
    /// unreadable source is stale even when the in-memory index still holds a
    /// derived chunk.
    pub fn current_digest(&self, uri: &str) -> KnowledgeResult<String> {
        self.open(uri).map(|source| source.digest)
    }
}

fn media_type_for(path: &Path) -> Option<&'static str> {
    match extension_of(path).as_str() {
        "txt" => Some("text/plain"),
        "md" | "markdown" => Some("text/markdown"),
        _ => None,
    }
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn detected_media_type(path: &Path) -> &'static str {
    match extension_of(path).as_str() {
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    }
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8 * 1024).any(|byte| *byte == 0)
}
