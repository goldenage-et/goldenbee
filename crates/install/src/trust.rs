//! Signed release advisories, publisher tiers, and installed-release decisions.
//!
//! Advisories are ordinary detached ed25519 signatures over a canonical JSON
//! payload. The installer verifies the signature against an operator-recorded
//! advisory key, enforces freshness and monotonic generation, and only then
//! applies release or signer revocations. The registry and `common` never
//! become trust authorities.

use crate::error::InstallError;
use base64::Engine;
use common::bytes_digest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Advisory schema understood by this installer.
pub const ADVISORY_SCHEMA: u32 = 1;
/// Advisory state schema.
pub const ADVISORY_STATE_SCHEMA: u32 = 1;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;
const CLOCK_SKEW_MS: i64 = 300_000;

/// One operator-recorded advisory signing key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryKey {
    /// Stable authority identity named by the advisory envelope.
    pub id: String,
    /// Base64 ed25519 public key.
    pub key: String,
    /// Millisecond Unix cutoff after which this key is retired. Retirement is
    /// widened by [`RotationPolicy::advisory_key_overlap_secs`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<i64>,
}

/// Explicit key-rotation behavior.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationPolicy {
    /// Additional seconds an advisory key remains acceptable past `not_after`.
    /// Zero retires the key exactly at its cutoff.
    #[serde(default)]
    pub advisory_key_overlap_secs: u64,
}

/// What happens to an already installed release when current evidence revokes
/// or cannot be evaluated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstalledAction {
    /// Installed execution continues; new installs and upgrades remain gated.
    #[default]
    Continue,
    /// Running work continues, but new starts and profile mutations are denied.
    Restrict,
    /// Running work is stopped and new starts are denied.
    Quarantine,
}

impl InstalledAction {
    /// Stable lowercase name used in reports and state files.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Restrict => "restrict",
            Self::Quarantine => "quarantine",
        }
    }
}

impl std::fmt::Display for InstalledAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Explicit offline behavior for already installed releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfflinePolicy {
    /// Action when a verified advisory revokes the installed release or signer.
    #[serde(default)]
    pub installed_revoked: InstalledAction,
    /// Action when required advisory evidence is unavailable or unverifiable.
    #[serde(default)]
    pub installed_without_advisory: InstalledAction,
}

impl Default for OfflinePolicy {
    fn default() -> Self {
        Self {
            installed_revoked: InstalledAction::Continue,
            installed_without_advisory: InstalledAction::Continue,
        }
    }
}

/// One prohibited release or signer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
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
    /// Redacted explanation shown to operators.
    #[serde(default)]
    pub reason: String,
}

/// Signed advisory body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryPayload {
    /// Advisory schema.
    pub schema: u32,
    /// Monotonic generation. A lower generation can never replace a newer one.
    pub generation: u64,
    /// Millisecond Unix issue time.
    pub issued_at: i64,
    /// Optional millisecond Unix expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// Prohibited releases and signers.
    #[serde(default)]
    pub revocations: Vec<Revocation>,
}

impl AdvisoryPayload {
    /// Reject a malformed advisory before it can change any decision.
    pub fn validate(&self) -> Result<(), InstallError> {
        if self.schema != ADVISORY_SCHEMA {
            return Err(InstallError::new(
                "install.advisory_invalid",
                "advisory schema is unsupported",
            ));
        }
        if self.generation == 0 {
            return Err(InstallError::new(
                "install.advisory_invalid",
                "advisory generation must be positive",
            ));
        }
        if self.issued_at <= 0 {
            return Err(InstallError::new(
                "install.advisory_invalid",
                "advisory issue time is invalid",
            ));
        }
        if self
            .expires_at
            .is_some_and(|expires_at| expires_at <= self.issued_at)
        {
            return Err(InstallError::new(
                "install.advisory_invalid",
                "advisory expiry does not follow its issue time",
            ));
        }
        for revocation in &self.revocations {
            match revocation.kind.as_str() {
                "release" => {
                    if !crate::artifact::is_digest(&revocation.digest) {
                        return Err(InstallError::new(
                            "install.advisory_invalid",
                            "release revocation requires an exact sha256 digest",
                        ));
                    }
                }
                "signer" => {
                    if revocation.signer.trim().is_empty() {
                        return Err(InstallError::new(
                            "install.advisory_invalid",
                            "signer revocation requires a signer identity",
                        ));
                    }
                }
                _ => {
                    return Err(InstallError::new(
                        "install.advisory_invalid",
                        "advisory revocation kind is unsupported",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Detached advisory envelope. `payload` is standard base64 of the canonical
/// serialized [`AdvisoryPayload`] bytes and `signature` is base64 ed25519 over
/// the `sha256:` digest of those bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvisoryEnvelope {
    /// Envelope schema.
    pub schema: u32,
    /// Authority identity resolved against the policy's advisory keys.
    pub authority: String,
    /// Standard base64 payload bytes.
    pub payload: String,
    /// Standard base64 ed25519 signature.
    pub signature: String,
}

/// Verified advisory with its exact signed bytes.
#[derive(Debug, Clone)]
pub struct VerifiedAdvisory {
    /// Authority identity.
    pub authority: String,
    /// Parsed and validated payload.
    pub payload: AdvisoryPayload,
    /// `sha256:` digest of the exact payload bytes that were signed.
    pub payload_digest: String,
}

/// Persisted highest accepted advisory identity, used to reject replay and
/// generation rollback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryState {
    /// State schema.
    pub schema: u32,
    /// Highest accepted generation.
    pub generation: u64,
    /// Digest of that generation's payload bytes.
    pub payload_digest: String,
    /// Millisecond Unix acceptance time.
    pub accepted_at: i64,
}

/// Release acceptance details retained for inspection and audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseDecision {
    /// Effective publisher tier.
    pub signer_tier: String,
    /// Advisory generation that permitted the decision, when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisory_generation: Option<u64>,
}

/// Local trust presentation for one public discovery result. It is computed
/// from the installer's recorded policy and verified advisory; the registry
/// status is never substituted for this decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryStatus {
    /// `trusted`, `unverified`, `untrusted`, `denied`, `revoked`,
    /// `quarantined`, or `advisory_required`.
    pub status: String,
    /// Redacted operator-facing reason.
    pub reason: String,
    /// Advisory generation used for the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisory_generation: Option<u64>,
}

/// Installed-release decision under the recorded offline policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledDecision {
    /// Continue, restrict, or quarantine.
    pub action: InstalledAction,
    /// Redacted operator-facing reason.
    pub reason: String,
    /// Advisory generation used for the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisory_generation: Option<u64>,
}

/// One signer root as recorded in the effective policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignerRootSnapshot {
    /// Signer identity.
    pub signer: String,
    /// Fingerprint of the public key, never the key itself.
    pub public_key_fingerprint: String,
    /// Effective publisher tier.
    pub tier: String,
}

/// One advisory key as recorded in the effective policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryKeySnapshot {
    /// Authority identity.
    pub id: String,
    /// Fingerprint of the public key.
    pub public_key_fingerprint: String,
    /// Retirement cutoff, when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<i64>,
}

/// Operator-visible effective trust policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySnapshot {
    /// Recorded signer roots and tiers.
    pub signer_roots: Vec<SignerRootSnapshot>,
    /// Recorded publisher tiers.
    pub publisher_tiers: BTreeMap<String, String>,
    /// Tiers permitted for new installs.
    pub allowed_tiers: Vec<String>,
    /// Recorded advisory signing keys.
    pub advisory_keys: Vec<AdvisoryKeySnapshot>,
    /// Required advisory freshness in seconds.
    pub advisory_freshness_secs: u64,
    /// Whether a verified advisory is mandatory for new installs.
    pub require_advisory: bool,
    /// Recorded rotation behavior.
    pub rotation: RotationPolicy,
    /// Recorded offline behavior.
    pub offline: OfflinePolicy,
    /// Digest of the policy representation.
    pub policy_digest: String,
}

/// Verify one signed advisory against the recorded keys, freshness, and prior
/// accepted generation. This never executes a release.
pub fn verify_advisory(
    policy: &crate::policy::TrustPolicy,
    bytes: &[u8],
    prior: Option<&AdvisoryState>,
    now: i64,
) -> Result<VerifiedAdvisory, InstallError> {
    let envelope: AdvisoryEnvelope = serde_json::from_slice(bytes).map_err(|_| {
        InstallError::new("install.advisory_invalid", "advisory envelope is malformed")
    })?;
    if envelope.schema != ADVISORY_SCHEMA {
        return Err(InstallError::new(
            "install.advisory_invalid",
            "advisory envelope schema is unsupported",
        ));
    }
    let payload_bytes = B64.decode(&envelope.payload).map_err(|_| {
        InstallError::new(
            "install.advisory_invalid",
            "advisory payload is not valid base64",
        )
    })?;
    let payload_digest = bytes_digest(&payload_bytes);
    let mut candidates = policy
        .advisory_keys
        .iter()
        .filter(|key| key.id == envelope.authority)
        .peekable();
    if candidates.peek().is_none() {
        return Err(InstallError::new(
            "install.advisory_authority_untrusted",
            "advisory authority is not trusted",
        ));
    }
    let overlap_ms = policy
        .rotation
        .advisory_key_overlap_secs
        .saturating_mul(1_000) as i64;
    let mut retired = false;
    let mut active_attempted = false;
    for key in candidates {
        if key
            .not_after
            .is_some_and(|not_after| now > not_after.saturating_add(overlap_ms))
        {
            retired = true;
            continue;
        }
        active_attempted = true;
        if runtime::crypto::verify_release_signature(&key.key, &payload_digest, &envelope.signature)
            .is_err()
        {
            continue;
        }
        let payload: AdvisoryPayload = serde_json::from_slice(&payload_bytes).map_err(|_| {
            InstallError::new("install.advisory_invalid", "advisory payload is malformed")
        })?;
        payload.validate()?;
        let age = now.saturating_sub(payload.issued_at);
        if payload.issued_at > now.saturating_add(CLOCK_SKEW_MS) {
            return Err(InstallError::new(
                "install.advisory_stale",
                "advisory issue time is in the future",
            ));
        }
        if age > policy.advisory_freshness_secs.saturating_mul(1_000) as i64 {
            return Err(InstallError::new(
                "install.advisory_stale",
                "advisory is older than the required freshness window",
            ));
        }
        if payload
            .expires_at
            .is_some_and(|expires_at| now > expires_at)
        {
            return Err(InstallError::new(
                "install.advisory_stale",
                "advisory has expired",
            ));
        }
        if let Some(prior) = prior {
            if payload.generation < prior.generation
                || (payload.generation == prior.generation
                    && payload_digest != prior.payload_digest)
            {
                return Err(InstallError::new(
                    "install.advisory_replay",
                    "advisory generation would replace newer accepted policy",
                ));
            }
        }
        return Ok(VerifiedAdvisory {
            authority: envelope.authority,
            payload,
            payload_digest,
        });
    }
    if retired && !active_attempted {
        return Err(InstallError::new(
            "install.advisory_key_retired",
            "advisory signing key has been retired",
        ));
    }
    Err(InstallError::new(
        "install.advisory_signature_invalid",
        "advisory signature does not verify against a trusted key",
    ))
}

/// Effective tier for one signer. A missing tier is explicitly `unverified`.
pub fn signer_tier(policy: &crate::policy::TrustPolicy, signer: &str) -> String {
    policy
        .publisher_tiers
        .get(signer)
        .cloned()
        .unwrap_or_else(|| "unverified".into())
}

/// Fingerprint one recorded public key without exposing the key bytes.
pub fn key_fingerprint(key: &str) -> String {
    bytes_digest(key.as_bytes())
}
