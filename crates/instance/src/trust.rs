//! Instance-scoped trust policy application.
//!
//! Trust evaluation is a local operator action. It reads the verified release
//! directory, a recorded policy, and exact signed advisory bytes. It never
//! contacts a registry and never changes another instance.

use crate::layout::{
    instance_paths, load_record, load_state, save_state, InstanceError, InstanceRecord,
    InstanceState, TrustStatus,
};
use crate::process;
use install::{InstalledAction, Installer, TrustPolicy};
use std::path::{Path, PathBuf};

/// Result of applying one explicit trust decision to an instance.
#[derive(Debug, Clone)]
pub struct ApplyTrustOutcome {
    /// Persistent record.
    pub record: InstanceRecord,
    /// Persisted lifecycle state.
    pub state: InstanceState,
    /// Continue, restrict, or quarantine.
    pub decision: InstalledAction,
    /// Redacted reason from the verification.
    pub reason: String,
    /// Advisory generation used for the decision.
    pub advisory_generation: Option<u64>,
}

/// Apply an explicit trust decision to one already installed instance.
///
/// `advisory` is exact signed advisory bytes supplied by the operator or a
/// distribution tool. Missing evidence follows the recorded offline policy;
/// it never silently broadens authority. A tampered, stale, or replayed
/// advisory is rejected before any instance state changes.
pub fn apply_trust(
    root: &Path,
    id: &str,
    policy: &TrustPolicy,
    advisory: Option<&[u8]>,
) -> Result<ApplyTrustOutcome, InstanceError> {
    let paths = instance_paths(root, id)?;
    let record = load_record(&paths)?;
    let mut state = load_state(&paths)?;
    let cache = cache_root(&record)?;
    let mut installer = Installer::new(cache, policy.clone());
    if let Some(bytes) = advisory {
        installer = installer.with_advisory_bytes(bytes.to_vec());
    }
    let decision = installer.decide_installed(
        &record.package,
        &record.version,
        &record.artifact_digest,
        &signer_for(&installer, &record)?,
    )?;
    if decision.action == InstalledAction::Quarantine
        && (state.pid.is_some() || state.lifecycle == "running")
    {
        state = process::stop(root, id)?.state;
    }
    state.trust = Some(TrustStatus {
        decision: decision.action,
        reason: decision.reason.clone(),
        advisory_generation: decision.advisory_generation,
        decided_at: now_ms(),
    });
    save_state(&paths, &state)?;
    Ok(ApplyTrustOutcome {
        record,
        state,
        decision: decision.action,
        reason: decision.reason,
        advisory_generation: decision.advisory_generation,
    })
}

/// Refuse new execution for an instance that an operator restricted or
/// quarantined. Inspect and stop remain available.
pub(crate) fn enforce(state: &InstanceState) -> Result<(), InstanceError> {
    match state.trust.as_ref().map(|trust| trust.decision) {
        Some(InstalledAction::Quarantine) => Err(InstanceError::new(
            "instance.quarantined",
            "trust policy quarantined this instance; an operator must apply a new decision",
        )),
        Some(InstalledAction::Restrict) => Err(InstanceError::new(
            "instance.trust_restricted",
            "trust policy restricts new execution for this instance",
        )),
        _ => Ok(()),
    }
}

fn signer_for(installer: &Installer, record: &InstanceRecord) -> Result<String, InstanceError> {
    if !record.signer.is_empty() {
        return Ok(record.signer.clone());
    }
    // Older instance records predate the recorded signer. Recover it from the
    // verified cache record for this exact digest instead of treating a
    // non-empty signer identity as an implicit grant.
    let installed = installer.installed()?;
    installed
        .into_iter()
        .find(|release| release.artifact_digest == record.artifact_digest)
        .map(|release| release.signer)
        .ok_or_else(|| {
            InstanceError::new(
                "instance.release_invalid",
                "installed release signer cannot be recovered from the verified cache",
            )
        })
}

fn cache_root(record: &InstanceRecord) -> Result<PathBuf, InstanceError> {
    let digest = record
        .release_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let packages = record.release_dir.parent();
    let cache = packages.and_then(Path::parent);
    let valid = packages.and_then(Path::file_name) == Some(std::ffi::OsStr::new("sha256"))
        && cache.and_then(Path::file_name) == Some(std::ffi::OsStr::new("packages"))
        && digest.len() == 64;
    if !valid {
        return Err(InstanceError::new(
            "instance.release_invalid",
            "recorded release is not under a verified packages/sha256 cache",
        ));
    }
    cache
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            InstanceError::new("instance.release_invalid", "release cache root is invalid")
        })
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
