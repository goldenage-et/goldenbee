//! Installer trust and compatibility policy.

use crate::artifact::VerifiedLayout;
use crate::error::InstallError;
use crate::trust::{
    self, AdvisoryKey, AdvisoryKeySnapshot, AdvisoryState, DiscoveryStatus, InstalledAction,
    InstalledDecision, OfflinePolicy, PolicySnapshot, ReleaseDecision, RotationPolicy,
    SignerRootSnapshot, VerifiedAdvisory,
};
use common::ReleaseMetadata;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const TIERS: &[&str] = &[
    "official",
    "verified",
    "community",
    "unverified",
    "revoked",
    "quarantined",
];

/// Explicit signer roots, publisher tiers, advisory authority, compatibility
/// windows, permission grants, rotation, and offline policy.
///
/// This state belongs to the installer. `common` only carries the metadata
/// representations that are evaluated against it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustPolicy {
    /// Trusted signer identities and base64 ed25519 public keys.
    #[serde(default)]
    pub trusted_signers: BTreeMap<String, String>,
    /// Recorded publisher trust tier per signer identity.
    #[serde(default)]
    pub publisher_tiers: BTreeMap<String, String>,
    /// If non-empty, new installs accept only these publisher tiers. Revoked
    /// and quarantined tiers are always denied.
    #[serde(default)]
    pub allowed_tiers: Vec<String>,
    /// Trusted advisory signing keys. Both `require_advisory` and any recorded
    /// key demand an explicit freshness window.
    #[serde(default)]
    pub advisory_keys: Vec<AdvisoryKey>,
    /// Maximum accepted advisory age in seconds.
    #[serde(default)]
    pub advisory_freshness_secs: u64,
    /// Whether a verified advisory is mandatory before a new install.
    #[serde(default)]
    pub require_advisory: bool,
    /// Recorded key-rotation behavior.
    #[serde(default)]
    pub rotation: RotationPolicy,
    /// Explicit behavior for already installed releases.
    #[serde(default)]
    pub offline: OfflinePolicy,
    /// Permitted target triples.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Compatible Worker runtime versions.
    #[serde(default)]
    pub runtimes: Vec<String>,
    /// Compatible pinned ADK versions.
    #[serde(default)]
    pub adks: Vec<String>,
    /// Supported management protocol versions.
    #[serde(default)]
    pub protocols: Vec<String>,
    /// Required compiled features the host can support.
    #[serde(default)]
    pub features: Vec<String>,
    /// Permissions the operator explicitly grants to installed releases.
    #[serde(default)]
    pub approved_permissions: Vec<String>,
    /// Permissions treated as sensitive. A sensitive declaration must also be
    /// present in `approved_permissions` or installation is denied.
    #[serde(default)]
    pub sensitive_permissions: Vec<String>,
}

impl TrustPolicy {
    /// The target triple this host is expected to execute.
    pub fn host_target() -> String {
        if cfg!(target_os = "linux") {
            format!("{}-unknown-linux-gnu", std::env::consts::ARCH)
        } else {
            format!(
                "{}-unknown-{}",
                std::env::consts::ARCH,
                std::env::consts::OS
            )
        }
    }

    /// Load a policy file. The file must not be group- or world-writable so an
    /// unauthorized local client cannot replace trust roots.
    pub fn load(path: &std::path::Path) -> Result<Self, InstallError> {
        secure_policy_file(path)?;
        let bytes = std::fs::read(path).map_err(|_| {
            InstallError::new(
                "trust.policy_unavailable",
                "trust policy file is unreadable",
            )
        })?;
        let policy: Self = serde_json::from_slice(&bytes)
            .map_err(|_| InstallError::new("trust.policy_invalid", "trust policy is malformed"))?;
        policy.validate()?;
        Ok(policy)
    }

    /// Reject a policy that cannot make a safe, recorded decision.
    pub fn validate(&self) -> Result<(), InstallError> {
        if self.require_advisory || !self.advisory_keys.is_empty() {
            if self.advisory_keys.is_empty() {
                return Err(InstallError::new(
                    "trust.policy_invalid",
                    "required advisory evidence needs at least one advisory key",
                ));
            }
            if self.advisory_freshness_secs == 0 {
                return Err(InstallError::new(
                    "trust.policy_invalid",
                    "advisory evidence needs an explicit freshness window",
                ));
            }
        }
        for key in &self.advisory_keys {
            if key.id.trim().is_empty() || key.key.trim().is_empty() {
                return Err(InstallError::new(
                    "trust.policy_invalid",
                    "advisory key needs an identity and public key",
                ));
            }
        }
        for tier in self
            .publisher_tiers
            .values()
            .chain(self.allowed_tiers.iter())
        {
            if !TIERS.contains(&tier.as_str()) {
                return Err(InstallError::new(
                    "trust.policy_invalid",
                    format!("publisher tier '{tier}' is not supported"),
                ));
            }
        }
        Ok(())
    }

    /// Verify signer trust, signature, compatibility, tiers, and permission
    /// grants. This function never executes the candidate binary.
    pub fn verify(&self, layout: &VerifiedLayout) -> Result<(), InstallError> {
        let metadata = &layout.metadata;
        let public_key = self.trusted_signers.get(&metadata.signer).ok_or_else(|| {
            InstallError::new(
                "install.signer_untrusted",
                format!("signer '{}' is not trusted", metadata.signer),
            )
        })?;
        let metadata_digest = common::bytes_digest(&layout.metadata_bytes);
        runtime::crypto::verify_release_signature(public_key, &metadata_digest, &layout.signature)
            .map_err(|_| {
                InstallError::new(
                    "install.signature_invalid",
                    "release signature does not verify against trusted metadata",
                )
            })?;
        self.check_signer_tier(&metadata.signer)?;
        self.check_compatibility(metadata)?;
        self.check_permissions(metadata)?;
        Ok(())
    }

    /// Verify a signed advisory against this policy and a previously accepted
    /// generation.
    pub fn verify_advisory(
        &self,
        bytes: &[u8],
        prior: Option<&AdvisoryState>,
        now: i64,
    ) -> Result<VerifiedAdvisory, InstallError> {
        trust::verify_advisory(self, bytes, prior, now)
    }

    /// Apply publisher tiers and advisory revocations to one exact release.
    pub fn decide_release(
        &self,
        metadata: &ReleaseMetadata,
        artifact_digest: &str,
        advisory: Option<&VerifiedAdvisory>,
    ) -> Result<ReleaseDecision, InstallError> {
        let tier = trust::signer_tier(self, &metadata.signer);
        self.check_tier(&tier)?;
        if let Some(advisory) = advisory {
            for revocation in &advisory.payload.revocations {
                match revocation.kind.as_str() {
                    "release"
                        if revocation.digest == artifact_digest
                            && (revocation.package.is_empty()
                                || revocation.package == metadata.package.name)
                            && (revocation.version.is_empty()
                                || revocation.version == metadata.package.version) =>
                    {
                        return Err(InstallError::new(
                            "install.release_revoked",
                            if revocation.reason.is_empty() {
                                "release digest is revoked".to_string()
                            } else {
                                format!("release digest is revoked: {}", revocation.reason)
                            },
                        ));
                    }
                    "signer" if revocation.signer == metadata.signer => {
                        return Err(InstallError::new(
                            "install.signer_revoked",
                            if revocation.reason.is_empty() {
                                "release signer is revoked".to_string()
                            } else {
                                format!("release signer is revoked: {}", revocation.reason)
                            },
                        ));
                    }
                    _ => {}
                }
            }
        }
        Ok(ReleaseDecision {
            signer_tier: tier,
            advisory_generation: advisory.map(|value| value.payload.generation),
        })
    }

    /// Decide what happens to an already installed release. This never makes a
    /// network request and never widens authority when evidence is missing.
    pub fn decide_installed(
        &self,
        package: &str,
        version: &str,
        artifact_digest: &str,
        signer: &str,
        advisory: Option<&VerifiedAdvisory>,
    ) -> InstalledDecision {
        let generation = advisory.map(|value| value.payload.generation);
        if !self.trusted_signers.contains_key(signer) {
            return InstalledDecision {
                action: self.offline.installed_revoked,
                reason: "installed release signer is no longer trusted".into(),
                advisory_generation: generation,
            };
        }
        let tier = trust::signer_tier(self, signer);
        if matches!(tier.as_str(), "revoked" | "quarantined")
            || (!self.allowed_tiers.is_empty() && !self.allowed_tiers.contains(&tier))
        {
            return InstalledDecision {
                action: self.offline.installed_revoked,
                reason: format!("installed release publisher tier '{tier}' is not permitted"),
                advisory_generation: generation,
            };
        }
        let Some(advisory) = advisory else {
            return InstalledDecision {
                action: if self.require_advisory {
                    self.offline.installed_without_advisory
                } else {
                    InstalledAction::Continue
                },
                reason: if self.require_advisory {
                    "required advisory evidence is unavailable".into()
                } else {
                    "no advisory is required".into()
                },
                advisory_generation: None,
            };
        };
        for revocation in &advisory.payload.revocations {
            match revocation.kind.as_str() {
                "release"
                    if revocation.digest == artifact_digest
                        && (revocation.package.is_empty() || revocation.package == package)
                        && (revocation.version.is_empty() || revocation.version == version) =>
                {
                    return InstalledDecision {
                        action: self.offline.installed_revoked,
                        reason: if revocation.reason.is_empty() {
                            "installed release digest is revoked".into()
                        } else {
                            format!("installed release digest is revoked: {}", revocation.reason)
                        },
                        advisory_generation: generation,
                    };
                }
                "signer" if revocation.signer == signer => {
                    return InstalledDecision {
                        action: self.offline.installed_revoked,
                        reason: if revocation.reason.is_empty() {
                            "installed release signer is revoked".into()
                        } else {
                            format!("installed release signer is revoked: {}", revocation.reason)
                        },
                        advisory_generation: generation,
                    };
                }
                _ => {}
            }
        }
        InstalledDecision {
            action: InstalledAction::Continue,
            reason: "verified advisory permits the installed release".into(),
            advisory_generation: generation,
        }
    }

    /// Operator-visible effective policy without raw key material.
    pub fn snapshot(&self) -> PolicySnapshot {
        let mut signer_roots: Vec<SignerRootSnapshot> = self
            .trusted_signers
            .iter()
            .map(|(signer, key)| SignerRootSnapshot {
                signer: signer.clone(),
                public_key_fingerprint: trust::key_fingerprint(key),
                tier: trust::signer_tier(self, signer),
            })
            .collect();
        signer_roots.sort_by(|left, right| left.signer.cmp(&right.signer));
        let mut publisher_tiers = self.publisher_tiers.clone();
        for signer in self.trusted_signers.keys() {
            publisher_tiers
                .entry(signer.clone())
                .or_insert_with(|| "unverified".into());
        }
        PolicySnapshot {
            signer_roots,
            publisher_tiers,
            allowed_tiers: self.allowed_tiers.clone(),
            advisory_keys: self
                .advisory_keys
                .iter()
                .map(|key| AdvisoryKeySnapshot {
                    id: key.id.clone(),
                    public_key_fingerprint: trust::key_fingerprint(&key.key),
                    not_after: key.not_after,
                })
                .collect(),
            advisory_freshness_secs: self.advisory_freshness_secs,
            require_advisory: self.require_advisory,
            rotation: self.rotation,
            offline: self.offline,
            policy_digest: common::bytes_digest(
                &serde_json::to_vec(self)
                    .unwrap_or_else(|_| b"unserializable-trust-policy".to_vec()),
            ),
        }
    }

    /// Local trust presentation for one public discovery result. This is not an
    /// installation grant: compatibility, feature, and permission checks still
    /// run at installation. It exists so discovery can show the same verified
    /// signer tier and advisory revocation status as the installer.
    pub fn discovery_status(
        &self,
        package: &str,
        version: &str,
        artifact_digest: &str,
        signer: &str,
        advisory: Option<&VerifiedAdvisory>,
    ) -> DiscoveryStatus {
        let generation = advisory.map(|value| value.payload.generation);
        if !self.trusted_signers.contains_key(signer) {
            return DiscoveryStatus {
                status: "untrusted".into(),
                reason: format!("signer '{signer}' is not trusted by the local policy"),
                advisory_generation: generation,
            };
        }
        let tier = trust::signer_tier(self, signer);
        if matches!(tier.as_str(), "revoked" | "quarantined") {
            return DiscoveryStatus {
                status: tier.clone(),
                reason: format!("publisher tier '{tier}' is not permitted on this host"),
                advisory_generation: generation,
            };
        }
        if !self.allowed_tiers.is_empty() && !self.allowed_tiers.contains(&tier) {
            return DiscoveryStatus {
                status: "denied".into(),
                reason: format!("publisher tier '{tier}' is not in the recorded allowed tiers"),
                advisory_generation: generation,
            };
        }
        let Some(advisory) = advisory else {
            if self.require_advisory {
                return DiscoveryStatus {
                    status: "advisory_required".into(),
                    reason: "a verified advisory is required before installation".into(),
                    advisory_generation: None,
                };
            }
            return DiscoveryStatus {
                status: "trusted".into(),
                reason:
                    "local signer policy accepts this release; compatibility and permissions still apply"
                        .into(),
                advisory_generation: None,
            };
        };
        for revocation in &advisory.payload.revocations {
            let matched = match revocation.kind.as_str() {
                "release" => {
                    revocation.digest == artifact_digest
                        && (revocation.package.is_empty() || revocation.package == package)
                        && (revocation.version.is_empty() || revocation.version == version)
                }
                "signer" => revocation.signer == signer,
                _ => false,
            };
            if matched {
                return DiscoveryStatus {
                    status: "revoked".into(),
                    reason: if revocation.reason.is_empty() {
                        "release or signer is revoked by the verified advisory".into()
                    } else {
                        format!(
                            "release or signer is revoked by the verified advisory: {}",
                            revocation.reason
                        )
                    },
                    advisory_generation: generation,
                };
            }
        }
        DiscoveryStatus {
            status: "trusted".into(),
            reason: "verified advisory and signer policy accept this release; compatibility and permissions still apply"
                .into(),
            advisory_generation: generation,
        }
    }

    fn check_signer_tier(&self, signer: &str) -> Result<(), InstallError> {
        let tier = trust::signer_tier(self, signer);
        self.check_tier(&tier)
    }

    fn check_tier(&self, tier: &str) -> Result<(), InstallError> {
        if matches!(tier, "revoked" | "quarantined")
            || (!self.allowed_tiers.is_empty()
                && !self.allowed_tiers.iter().any(|allowed| allowed == tier))
        {
            return Err(InstallError::new(
                "install.signer_tier_denied",
                format!("publisher tier '{tier}' is not permitted on this host"),
            ));
        }
        Ok(())
    }

    fn check_compatibility(&self, metadata: &ReleaseMetadata) -> Result<(), InstallError> {
        if !contains(&self.targets, &metadata.target) {
            return Err(InstallError::new(
                "install.target_unsupported",
                format!("target '{}' is not supported on this host", metadata.target),
            ));
        }
        if !contains(&self.runtimes, &metadata.package.runtime) {
            return Err(InstallError::new(
                "install.runtime_incompatible",
                "package requires an incompatible Worker runtime",
            ));
        }
        if !contains(&self.adks, &metadata.package.adk) {
            return Err(InstallError::new(
                "install.adk_incompatible",
                "package requires an incompatible pinned ADK release",
            ));
        }
        let (minimum, maximum) = metadata.protocol_range();
        if !self
            .protocols
            .iter()
            .any(|version| common::protocol_in_range(version, minimum, maximum))
        {
            return Err(InstallError::new(
                "install.protocol_incompatible",
                "package protocol range is not supported",
            ));
        }
        for feature in &metadata.package.features {
            if !contains(&self.features, feature) {
                return Err(InstallError::new(
                    "install.feature_unsupported",
                    format!("required feature '{feature}' is not available"),
                ));
            }
        }
        Ok(())
    }

    fn check_permissions(&self, metadata: &ReleaseMetadata) -> Result<(), InstallError> {
        let approved: BTreeSet<&str> = self
            .approved_permissions
            .iter()
            .map(String::as_str)
            .collect();
        let mut declared = BTreeSet::new();
        declared.extend(
            metadata
                .package
                .required_permissions
                .iter()
                .map(String::as_str),
        );
        for capability in &metadata.package.capabilities {
            declared.extend(capability.permissions.iter().map(String::as_str));
        }
        for permission in declared {
            if self
                .sensitive_permissions
                .iter()
                .any(|value| value == permission)
                && !approved.contains(permission)
            {
                return Err(InstallError::new(
                    "install.sensitive_permission_denied",
                    format!("sensitive permission '{permission}' is not explicitly granted"),
                ));
            }
            if !approved.contains(permission) {
                return Err(InstallError::new(
                    "install.permission_denied",
                    format!("permission '{permission}' is not granted on this host"),
                ));
            }
        }
        Ok(())
    }
}

fn contains(values: &[String], value: &str) -> bool {
    values.iter().any(|candidate| candidate == value)
}

#[cfg(unix)]
fn secure_policy_file(path: &std::path::Path) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path).map_err(|_| {
        InstallError::new(
            "trust.policy_unavailable",
            "trust policy file is unreadable",
        )
    })?;
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(InstallError::new(
            "trust.policy_insecure",
            "trust policy file must not be group- or world-writable",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_policy_file(_path: &std::path::Path) -> Result<(), InstallError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_target_is_a_linux_triple() {
        assert!(TrustPolicy::host_target().ends_with("-unknown-linux-gnu"));
    }
}
