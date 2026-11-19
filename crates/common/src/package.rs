//! Worker package manifests, releases, and installed instances.

use crate::error::{ApiError, ContractResult};
use crate::limits::Limits;
use crate::worker::CapabilityDescriptor;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A package manifest declaration checked against compiled registrations.
///
/// This is a metadata representation only. Trust decisions, installed state,
/// and executable authority stay with the on-host installer and Worker runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageManifest {
    /// Package name. Matches the crate and directory name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// Wire protocol range implemented by the package's management routes.
    pub protocol: String,
    /// Shared Worker runtime compatibility.
    pub runtime: String,
    /// Pinned ADK compatibility.
    pub adk: String,
    /// Declared executable capabilities.
    pub capabilities: Vec<CapabilityDescriptor>,
    /// Required permissions.
    #[serde(default)]
    pub required_permissions: Vec<String>,
    /// Compiled framework features required by the package.
    #[serde(default)]
    pub features: Vec<String>,
    /// Native launcher modes the package composes, for example `dev` and
    /// `serve`.
    #[serde(default)]
    pub launcher_modes: Vec<String>,
    /// Compiled hooks implemented by the package.
    #[serde(default)]
    pub hooks: Vec<String>,
}

impl PackageManifest {
    /// Reject duplicate or malformed declarations before admission.
    pub fn validate(&self) -> ContractResult<()> {
        if self.name.trim().is_empty() || self.name.len() > 64 {
            return Err(ApiError::new(
                "package.name_invalid",
                "package name must be 1-64 characters",
            ));
        }
        if self.version.trim().is_empty() || self.version.len() > 64 {
            return Err(ApiError::new(
                "package.version_invalid",
                "package version must be 1-64 characters",
            ));
        }
        if self.capabilities.is_empty() {
            return Err(ApiError::new(
                "package.capability_missing",
                "a package must declare at least one executable capability",
            ));
        }
        let mut seen = BTreeSet::new();
        for capability in &self.capabilities {
            if capability.id.trim().is_empty() || capability.id.len() > 128 {
                return Err(ApiError::new(
                    "package.capability_invalid",
                    "capability id must be 1-128 characters",
                ));
            }
            if !seen.insert(capability.id.as_str()) {
                return Err(ApiError::new(
                    "package.duplicate_capability",
                    format!("capability '{}' is declared twice", capability.id),
                ));
            }
        }
        for mode in &self.launcher_modes {
            if !matches!(mode.as_str(), "dev" | "serve" | "console") {
                return Err(ApiError::new(
                    "package.launcher_mode_invalid",
                    format!("launcher mode '{mode}' is not supported"),
                ));
            }
        }
        Ok(())
    }

    /// Convert declarations into runtime configuration capabilities.
    pub fn runtime_capabilities(&self) -> Vec<CapabilityDescriptor> {
        self.capabilities.clone()
    }
}

/// Optional bundled seed asset. Seed assets are user-overridable inputs, never
/// compiled capabilities or executable hooks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedAsset {
    /// Asset name relative to the seed root.
    pub path: String,
    /// Asset digest.
    pub digest: String,
}

/// Build evidence bound into one signed release.
///
/// The fields are descriptive metadata. They never grant authority and are
/// surfaced for operator inspection; `evidence_digest` may address a stored
/// build log or reproducibility record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildEvidence {
    /// Compiler or toolchain identity.
    #[serde(default)]
    pub toolchain: String,
    /// Build profile, for example `release`.
    #[serde(default)]
    pub profile: String,
    /// Reproducible command that produced the binary.
    #[serde(default)]
    pub command: String,
    /// RFC 3339 start time.
    #[serde(default)]
    pub started_at: String,
    /// RFC 3339 finish time.
    #[serde(default)]
    pub finished_at: String,
    /// Whether the publisher recorded a reproducible build.
    #[serde(default)]
    pub reproducible: bool,
    /// Digest of a build log or evidence artifact.
    #[serde(default)]
    pub evidence_digest: String,
}

/// Signed release metadata stored in the immutable package artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseMetadata {
    /// Metadata schema version.
    pub schema: u32,
    /// Package identity, compatibility, capabilities, and permissions.
    pub package: PackageManifest,
    /// Digest of the source descriptor blob.
    pub source_digest: String,
    /// Digest of the executable binary blob.
    pub binary_digest: String,
    /// Source revision bound into the signed metadata. Must match the source
    /// descriptor `revision` when both are present.
    #[serde(default)]
    pub source_revision: String,
    /// Target triple the binary was built for.
    pub target: String,
    /// Lowest management protocol version supported by the package.
    #[serde(default)]
    pub protocol_min: String,
    /// Highest management protocol version supported by the package.
    #[serde(default)]
    pub protocol_max: String,
    /// Signer identity resolved against the trusted signer set.
    pub signer: String,
    /// Build evidence for this exact artifact.
    #[serde(default)]
    pub build: BuildEvidence,
    /// Optional bundled seed assets. Empty releases install without user
    /// assets and never activate these entries automatically.
    #[serde(default)]
    pub seed_assets: Vec<SeedAsset>,
}

impl ReleaseMetadata {
    /// Declared management protocol range. Older metadata that predates the
    /// range fields falls back to the single `package.protocol` value.
    pub fn protocol_range(&self) -> (&str, &str) {
        let minimum = if self.protocol_min.is_empty() {
            self.package.protocol.as_str()
        } else {
            self.protocol_min.as_str()
        };
        let maximum = if self.protocol_max.is_empty() {
            self.package.protocol.as_str()
        } else {
            self.protocol_max.as_str()
        };
        (minimum, maximum)
    }

    /// Reject a range that is malformed or excludes the package's declared
    /// protocol version.
    pub fn validate_protocol_range(&self) -> ContractResult<()> {
        let (minimum, maximum) = self.protocol_range();
        let minimum = parse_protocol(minimum).ok_or_else(|| {
            ApiError::new(
                "package.protocol_range_invalid",
                "release protocol minimum is not a major.minor version",
            )
        })?;
        let maximum = parse_protocol(maximum).ok_or_else(|| {
            ApiError::new(
                "package.protocol_range_invalid",
                "release protocol maximum is not a major.minor version",
            )
        })?;
        if minimum > maximum {
            return Err(ApiError::new(
                "package.protocol_range_invalid",
                "release protocol range is inverted",
            ));
        }
        let declared = parse_protocol(&self.package.protocol).ok_or_else(|| {
            ApiError::new(
                "package.protocol_invalid",
                "package protocol is not a major.minor version",
            )
        })?;
        if declared < minimum || declared > maximum {
            return Err(ApiError::new(
                "package.protocol_range_invalid",
                "package protocol is outside its declared release range",
            ));
        }
        Ok(())
    }
}

/// Current release metadata schema.
pub const RELEASE_METADATA_SCHEMA: u32 = 1;

fn parse_protocol(value: &str) -> Option<(u32, u32)> {
    let (major, minor) = value.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Whether one supported protocol version falls inside an inclusive range.
pub fn protocol_in_range(version: &str, minimum: &str, maximum: &str) -> bool {
    let (Some(version), Some(minimum), Some(maximum)) = (
        parse_protocol(version),
        parse_protocol(minimum),
        parse_protocol(maximum),
    ) else {
        return false;
    };
    version >= minimum && version <= maximum
}

/// Installed release metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    /// Package name.
    pub package: String,
    /// Version.
    pub version: String,
    /// Content digest.
    pub digest: String,
    /// Signer identity.
    pub signer: String,
    /// Target triple.
    pub target: String,
    /// Manifest digest.
    pub manifest_digest: String,
    /// Added permissions compared with the installed release.
    #[serde(default)]
    pub added_permissions: Vec<String>,
    /// Revocation list generation.
    pub revocation_generation: u64,
}

/// Worker Instance metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    /// Stable instance identity.
    pub id: String,
    /// Package.
    pub package: String,
    /// Version.
    pub version: String,
    /// Data directory.
    pub data_dir: String,
    /// Profile names.
    #[serde(default)]
    pub profiles: Vec<String>,
    /// Secret references.
    #[serde(default)]
    pub secret_refs: Vec<String>,
    /// Resource limits.
    #[serde(default)]
    pub limits: Limits,
    /// Binary version.
    pub binary_version: String,
    /// Persisted-data schema version.
    pub data_schema: u32,
    /// Previous binary version for explicit rollback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
    /// Previous release digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_digest: Option<String>,
    /// Data backup path created before an upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_backup: Option<String>,
    /// Quarantined reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine: Option<String>,
}
