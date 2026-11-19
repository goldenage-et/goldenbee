//! Immutable artifact metadata and scoped grants.

use serde::{Deserialize, Serialize};

/// Immutable artifact metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// Artifact identity. Opaque; never a filesystem path or URL.
    pub id: String,
    /// Task provenance.
    pub task_id: String,
    /// Native invocation that produced this finalized version.
    pub native_invocation_id: String,
    /// Task attempt that produced this version.
    pub attempt: u32,
    /// Trace identity of the producing execution.
    #[serde(default)]
    pub trace_id: String,
    /// Immutable native artifact version.
    pub version: i64,
    /// Namespace.
    pub namespace: String,
    /// Owner principal.
    pub owner: String,
    /// Artifact name.
    pub name: String,
    /// Content type.
    pub content_type: String,
    /// Byte length.
    pub size: u64,
    /// SHA-256 digest as `sha256:<hex>`.
    pub digest: String,
    /// Finalized artifacts are immutable.
    pub finalized: bool,
    /// Creation time.
    pub created_at: i64,
}

/// Scoped, expiring artifact grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactGrant {
    /// Artifact identity.
    pub artifact_id: String,
    /// Bearer grant value.
    pub grant: String,
    /// Expiry.
    pub expires_at: i64,
}
