//! Public registry protocol types.
//!
//! These are the wire shapes shared by publishers, readers, and the private
//! registry service. Server-only configuration and bearer-token handling live
//! in the infrastructure registry application, not in this client crate.

use common::CapabilityDescriptor;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable registry-client failure. Messages are redacted before they reach
/// operator output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryError {
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Redacted human-readable detail.
    pub message: String,
    /// HTTP status class.
    pub status: u16,
}

impl RegistryError {
    /// Build an error with a stable code and default status.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        let status = match code {
            "registry.unauthorized" => 401,
            "registry.forbidden" => 403,
            "registry.not_found" => 404,
            "registry.conflict" => 409,
            "registry.limit" => 413,
            "registry.digest_mismatch" => 422,
            "registry.unavailable" => 500,
            _ => 400,
        };
        Self {
            code,
            message: common::redact(&message.into()),
            status,
        }
    }

    /// A rejected client request.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("registry.invalid", message)
    }
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RegistryError {}

/// Maximum public search result bound.
pub const PUBLIC_MAX_RESULTS: usize = 200;
/// Default public search result bound.
pub const PUBLIC_DEFAULT_RESULTS: usize = 50;
/// Default public download bound.
pub const PUBLIC_DEFAULT_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// One operator-recorded public revocation. It mirrors the installer advisory
/// shape but is never trusted in place of the installer's verified advisory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicRevocation {
    /// `release` or `signer`.
    pub kind: String,
    /// Optional package narrowing for a release revocation.
    #[serde(default)]
    pub package: String,
    /// Optional version narrowing for a release revocation.
    #[serde(default)]
    pub version: String,
    /// Exact `sha256:` release manifest digest for a release revocation.
    #[serde(default)]
    pub digest: String,
    /// Signer identity for a signer revocation.
    #[serde(default)]
    pub signer: String,
    /// Redacted explanation shown during discovery.
    #[serde(default)]
    pub reason: String,
}

impl PublicRevocation {
    /// Reject a malformed public status entry before it can be served.
    pub fn validate(&self) -> Result<(), RegistryError> {
        match self.kind.as_str() {
            "release" if !is_sha256_digest(&self.digest) => Err(RegistryError::invalid(
                "public release revocation requires an exact sha256 digest",
            )),
            "signer" if self.signer.trim().is_empty() => Err(RegistryError::invalid(
                "public signer revocation requires a signer identity",
            )),
            "release" | "signer" => Ok(()),
            _ => Err(RegistryError::invalid(
                "public revocation kind is unsupported",
            )),
        }
    }
}

fn is_sha256_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Anonymous discovery filters. Every field is optional and bounded before it
/// reaches storage.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PublicQuery {
    /// Repository owner filter.
    pub publisher: Option<String>,
    /// Package name filter.
    pub package: Option<String>,
    /// Declared capability id filter.
    pub capability: Option<String>,
    /// Exact target triple filter.
    pub target: Option<String>,
    /// Exact Worker runtime compatibility filter.
    pub runtime: Option<String>,
    /// Exact pinned ADK compatibility filter.
    pub adk: Option<String>,
    /// One supported management protocol version filter.
    pub protocol: Option<String>,
    /// Requested result bound. The registry clamps it to `max_results`.
    pub limit: Option<usize>,
}

impl PublicQuery {
    /// Reject oversized or control-character filter values before scanning.
    pub fn validate(&self) -> Result<(), RegistryError> {
        for (label, value, maximum) in [
            ("publisher", self.publisher.as_deref(), 64),
            ("package", self.package.as_deref(), 64),
            ("capability", self.capability.as_deref(), 128),
            ("target", self.target.as_deref(), 128),
            ("runtime", self.runtime.as_deref(), 64),
            ("adk", self.adk.as_deref(), 64),
            ("protocol", self.protocol.as_deref(), 32),
        ] {
            if let Some(value) = value {
                if value.len() > maximum
                    || value
                        .chars()
                        .any(|character| character.is_control() || character == '/')
                {
                    return Err(RegistryError::invalid(format!(
                        "public {label} filter is not a safe value"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Explicit publisher status shown with public release metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublisherTrust {
    /// `official`, `verified`, `community`, `unverified`, `revoked`, or
    /// `quarantined`.
    pub status: String,
    /// Redacted operator-facing reason.
    pub reason: String,
    /// Status source. Public discovery never infers trust from metadata.
    pub evidence: String,
    /// Whether this metadata claims the release is safe to execute.
    pub safety_asserted: bool,
    /// Whether on-host installer trust evidence is still mandatory.
    pub install_requires_local_trust_policy: bool,
}

/// One public release view. It wraps the immutable version entry and adds only
/// presentation status; it is not an installation grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicRelease {
    /// Immutable release metadata.
    #[serde(flatten)]
    pub entry: VersionEntry,
    /// Distribution object kind. Public discovery describes executable Worker
    /// Packages, not profiles or Client releases.
    pub kind: String,
    /// Explicit publisher trust presentation.
    pub publisher_trust: PublisherTrust,
}

/// Build evidence surfaced with discovery metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildEvidenceView {
    /// Compiler or toolchain identity.
    pub toolchain: String,
    /// Build profile.
    pub profile: String,
    /// Reproducible command.
    pub command: String,
    /// Start time.
    pub started_at: String,
    /// Finish time.
    pub finished_at: String,
    /// Whether the publisher recorded a reproducible build.
    pub reproducible: bool,
    /// Digest of a build log or evidence artifact.
    pub evidence_digest: String,
}

/// Immutable version metadata returned to authorized readers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionEntry {
    /// Repository owner namespace.
    pub publisher: String,
    /// Package name.
    pub package: String,
    /// Package version. Bound immutably to `artifact_digest`.
    pub version: String,
    /// OCI manifest digest, the immutable release identity.
    pub artifact_digest: String,
    /// Signed metadata config digest.
    pub metadata_digest: String,
    /// Source descriptor digest.
    pub source_digest: String,
    /// Source revision bound into signed metadata.
    pub source_revision: String,
    /// Executable digest.
    pub binary_digest: String,
    /// Target triple.
    pub target: String,
    /// Shared Worker runtime compatibility.
    pub runtime: String,
    /// Pinned ADK compatibility.
    pub adk: String,
    /// Lowest protocol version.
    pub protocol_min: String,
    /// Highest protocol version.
    pub protocol_max: String,
    /// Declared compiled features.
    pub features: Vec<String>,
    /// Declared grants from required permissions and capabilities.
    pub permissions: Vec<String>,
    /// Declared executable capabilities.
    pub capabilities: Vec<CapabilityDescriptor>,
    /// Every content-addressed blob that belongs to this immutable release.
    /// Public download uses this set so an anonymous request cannot address
    /// blobs from private or unpublished content.
    #[serde(default)]
    pub blobs: Vec<String>,
    /// Signer identity.
    pub signer: String,
    /// Publication time in milliseconds since the Unix epoch.
    pub published_at: i64,
    /// Build evidence.
    pub build: BuildEvidenceView,
}

/// Result of committing one release descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishReceipt {
    /// `published` or `converged`.
    pub outcome: String,
    /// Committed immutable entry.
    pub entry: VersionEntry,
}

/// A local signed layout prepared for upload.
#[derive(Debug, Clone)]
pub struct ReleaseBundle {
    /// Signed release metadata.
    pub metadata: common::ReleaseMetadata,
    /// Exact `index.json` bytes, including the detached signature annotation.
    pub index_json: Vec<u8>,
    /// Every content-addressed blob referenced by the descriptor.
    pub blobs: Vec<(String, Vec<u8>)>,
}
