//! On-host Worker release inspection, immutable cache installation, and
//! constrained post-verification acceptance.
//!
//! Trust roots, installation state, and executable authority live here rather
//! than in `common`. `common` carries the signed metadata representations.

pub mod artifact;
mod error;
mod policy;
pub mod registry;
pub mod release;
mod smoke;
pub mod trust;

pub use ::registry::{PublicQuery, PublicRelease};
pub use artifact::{
    encode_metadata, is_digest, read_layout, seed_assets, write_layout, write_layout_with_seeds,
    ArtifactDigests, SeedFile, VerifiedLayout, ARTIFACT_TYPE, BINARY_MEDIA_TYPE, INDEX_MEDIA_TYPE,
    LAYOUT_VERSION, MANIFEST_MEDIA_TYPE, METADATA_MEDIA_TYPE, SEED_MEDIA_TYPE,
    SIGNATURE_ANNOTATION, SOURCE_MEDIA_TYPE,
};
pub use error::InstallError;
pub use policy::TrustPolicy;
pub use registry::{bundle_from_layout, RegistryClient, ReleaseBundle};
pub use smoke::{ProbeReport, SmokeEvidence};
pub use trust::{
    AdvisoryEnvelope, AdvisoryKey, AdvisoryKeySnapshot, AdvisoryPayload, AdvisoryState,
    DiscoveryStatus, InstalledAction, InstalledDecision, OfflinePolicy, PolicySnapshot,
    ReleaseDecision, Revocation, RotationPolicy, SignerRootSnapshot, VerifiedAdvisory,
    ADVISORY_SCHEMA, ADVISORY_STATE_SCHEMA,
};

use common::ReleaseMetadata;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Verified, execution-free view of one local release artifact.
#[derive(Debug, Clone)]
pub struct Inspection {
    /// Signed release metadata.
    pub metadata: ReleaseMetadata,
    /// Immutable manifest digest.
    pub artifact_digest: String,
    /// Signed metadata config digest.
    pub metadata_digest: String,
    /// Effective trust decision and redacted reason.
    pub trust: TrustReport,
}

/// Redacted reason for one accepted trust decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustReport {
    /// `accepted`.
    pub decision: String,
    /// Effective publisher tier.
    pub signer_tier: String,
    /// Verified advisory generation, when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisory_generation: Option<u64>,
    /// Advisory authority identity, when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisory_authority: Option<String>,
    /// Digest of the recorded policy representation.
    pub policy_digest: String,
    /// Redacted operator-facing reason.
    pub reason: String,
}

/// A release present in the verified package cache.
#[derive(Debug, Clone)]
pub struct Installed {
    /// Immutable manifest digest.
    pub artifact_digest: String,
    /// Package name.
    pub package: String,
    /// Package version.
    pub version: String,
    /// Trusted signer identity.
    pub signer: String,
    /// Target triple.
    pub target: String,
    /// Advisory generation that permitted installation, when configured.
    pub advisory_generation: Option<u64>,
    /// Immutable release directory.
    pub directory: PathBuf,
    /// Executable path inside the release directory.
    pub binary: PathBuf,
    /// Capabilities confirmed by the smoke run.
    pub registered: Vec<String>,
    /// Launcher modes confirmed by the smoke run.
    pub launcher_modes: Vec<String>,
    /// Verified seed assets materialized below `<release>/seeds`.
    pub seeds: Vec<common::SeedAsset>,
}

/// Result of an installation request.
#[derive(Debug, Clone)]
pub enum InstallOutcome {
    /// The release was verified, smoked, and cached.
    Installed(Installed),
    /// The same immutable digest was already installed and verified.
    Converged(Installed),
}

impl InstallOutcome {
    /// Installed release.
    pub fn release(&self) -> &Installed {
        match self {
            Self::Installed(release) | Self::Converged(release) => release,
        }
    }

    /// Whether this call changed the cache.
    pub fn installed_now(&self) -> bool {
        matches!(self, Self::Installed(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallRecord {
    schema: u32,
    artifact_digest: String,
    metadata_digest: String,
    binary_digest: String,
    source_digest: String,
    package: String,
    version: String,
    signer: String,
    target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    advisory_generation: Option<u64>,
    launcher_modes: Vec<String>,
    registered: Vec<String>,
    #[serde(default)]
    seeds: Vec<common::SeedAsset>,
    installed_at: i64,
}

#[derive(Debug, Serialize)]
struct AuditEntry {
    at: i64,
    action: String,
    outcome: String,
    code: String,
    package: String,
    version: String,
    digest: String,
    signer: String,
    detail: String,
}

/// On-host installer bound to one cache root and trust policy.
#[derive(Debug, Clone)]
pub struct Installer {
    root: PathBuf,
    policy: TrustPolicy,
    advisory: Option<Vec<u8>>,
}

impl Installer {
    /// Create an installer for a cache root.
    pub fn new(root: impl Into<PathBuf>, policy: TrustPolicy) -> Self {
        Self {
            root: root.into(),
            policy,
            advisory: None,
        }
    }

    /// Attach exact signed advisory bytes. The advisory is verified before any
    /// candidate execution and is never fetched implicitly.
    pub fn with_advisory_bytes(mut self, advisory: impl Into<Vec<u8>>) -> Self {
        self.advisory = Some(advisory.into());
        self
    }

    /// Attach a local signed advisory file.
    pub fn with_advisory_file(self, path: &Path) -> Result<Self, InstallError> {
        let bytes = std::fs::read(path).map_err(|_| {
            InstallError::new(
                "install.advisory_unavailable",
                "advisory file is unreadable",
            )
        })?;
        Ok(self.with_advisory_bytes(bytes))
    }

    /// Trust policy in force.
    pub fn policy(&self) -> &TrustPolicy {
        &self.policy
    }

    /// Cache root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Highest advisory generation accepted for this cache root.
    pub fn advisory_state(&self) -> Result<Option<AdvisoryState>, InstallError> {
        let path = self.advisory_state_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(InstallError::new(
                    "install.storage_unavailable",
                    "accepted advisory state is unreadable",
                ));
            }
        };
        let state: AdvisoryState = serde_json::from_slice(&bytes).map_err(|_| {
            InstallError::new(
                "install.storage_unavailable",
                "accepted advisory state is malformed",
            )
        })?;
        if state.schema != trust::ADVISORY_STATE_SCHEMA {
            return Err(InstallError::new(
                "install.storage_unavailable",
                "accepted advisory state schema is unsupported",
            ));
        }
        Ok(Some(state))
    }

    /// Decide what happens to an installed release without contacting a
    /// registry. A valid advisory is persisted so a later apply cannot replay
    /// an older generation.
    pub fn decide_installed(
        &self,
        package: &str,
        version: &str,
        artifact_digest: &str,
        signer: &str,
    ) -> Result<InstalledDecision, InstallError> {
        let advisory = match &self.advisory {
            Some(_) => self.verify_trust_advisory(artifact_digest, true)?,
            None => None,
        };
        let decision = self.policy.decide_installed(
            package,
            version,
            artifact_digest,
            signer,
            advisory.as_ref(),
        );
        self.audit(
            "trust.installed",
            decision.action.as_str(),
            "",
            package,
            version,
            artifact_digest,
            signer,
            &decision.reason,
        );
        Ok(decision)
    }

    /// Inspect and verify one artifact without executing the candidate binary.
    pub fn inspect(&self, artifact: &Path) -> Result<Inspection, InstallError> {
        let layout = artifact::read_layout(artifact)?;
        self.policy.verify(&layout)?;
        let advisory = self.verify_trust_advisory(&layout.artifact_digest, false)?;
        let decision = self.policy.decide_release(
            &layout.metadata,
            &layout.artifact_digest,
            advisory.as_ref(),
        )?;
        Ok(Inspection {
            metadata: layout.metadata,
            artifact_digest: layout.artifact_digest,
            metadata_digest: common::bytes_digest(&layout.metadata_bytes),
            trust: TrustReport {
                decision: "accepted".into(),
                signer_tier: decision.signer_tier,
                advisory_generation: decision.advisory_generation,
                advisory_authority: advisory.map(|value| value.authority),
                policy_digest: self.policy.snapshot().policy_digest,
                reason: "signature, tier, advisory, compatibility, and permissions accepted".into(),
            },
        })
    }

    /// Verify, smoke, and install one artifact immutably by digest.
    pub async fn install(&self, artifact: &Path) -> Result<InstallOutcome, InstallError> {
        let layout = match artifact::read_layout(artifact) {
            Ok(layout) => layout,
            Err(error) => {
                self.audit(
                    "release.reject",
                    "rejected",
                    error.code,
                    "",
                    "",
                    "",
                    "",
                    &error.message,
                );
                return Err(error);
            }
        };
        if let Err(error) = self.policy.verify(&layout) {
            self.audit(
                "release.reject",
                "rejected",
                error.code,
                &layout.metadata.package.name,
                &layout.metadata.package.version,
                &layout.artifact_digest,
                &layout.metadata.signer,
                &error.message,
            );
            return Err(error);
        }
        let advisory = match self.verify_trust_advisory(&layout.artifact_digest, true) {
            Ok(advisory) => advisory,
            Err(error) => {
                self.audit(
                    "trust.reject",
                    "rejected",
                    error.code,
                    &layout.metadata.package.name,
                    &layout.metadata.package.version,
                    &layout.artifact_digest,
                    &layout.metadata.signer,
                    &error.message,
                );
                return Err(error);
            }
        };
        let decision = match self.policy.decide_release(
            &layout.metadata,
            &layout.artifact_digest,
            advisory.as_ref(),
        ) {
            Ok(decision) => decision,
            Err(error) => {
                self.audit(
                    "trust.reject",
                    "rejected",
                    error.code,
                    &layout.metadata.package.name,
                    &layout.metadata.package.version,
                    &layout.artifact_digest,
                    &layout.metadata.signer,
                    &error.message,
                );
                return Err(error);
            }
        };
        self.audit(
            "trust.accept",
            "accepted",
            "",
            &layout.metadata.package.name,
            &layout.metadata.package.version,
            &layout.artifact_digest,
            &layout.metadata.signer,
            &format!(
                "tier {} advisory {}",
                decision.signer_tier,
                decision
                    .advisory_generation
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "not-required".into())
            ),
        );
        self.install_verified(layout, decision.advisory_generation)
            .await
    }

    fn verify_trust_advisory(
        &self,
        artifact_digest: &str,
        persist: bool,
    ) -> Result<Option<VerifiedAdvisory>, InstallError> {
        let Some(bytes) = &self.advisory else {
            if self.policy.require_advisory {
                return Err(InstallError::new(
                    "install.advisory_required",
                    "a verified advisory is required before this trust-sensitive install",
                ));
            }
            return Ok(None);
        };
        let prior = self.advisory_state()?;
        let verified = self
            .policy
            .verify_advisory(bytes, prior.as_ref(), common::now_ms())?;
        if persist
            && prior.as_ref().is_none_or(|state| {
                state.generation != verified.payload.generation
                    || state.payload_digest != verified.payload_digest
            })
        {
            let state = AdvisoryState {
                schema: trust::ADVISORY_STATE_SCHEMA,
                generation: verified.payload.generation,
                payload_digest: verified.payload_digest.clone(),
                accepted_at: common::now_ms(),
            };
            self.write_advisory_state(&state, artifact_digest)?;
        }
        Ok(Some(verified))
    }

    fn write_advisory_state(
        &self,
        state: &AdvisoryState,
        artifact_digest: &str,
    ) -> Result<(), InstallError> {
        let directory = self.root.join("trust");
        std::fs::create_dir_all(&directory).map_err(storage)?;
        set_private_mode(&directory, 0o700)?;
        let path = self.advisory_state_path();
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| InstallError::new("install.storage_unavailable", error.to_string()))?;
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temporary).map_err(storage)?;
            file.write_all(&bytes).map_err(storage)?;
            file.sync_all().map_err(storage)?;
        }
        set_private_mode(&temporary, 0o600)?;
        std::fs::rename(&temporary, &path).map_err(storage)?;
        if let Some(parent) = path.parent() {
            if let Ok(directory) = std::fs::File::open(parent) {
                let _ = directory.sync_all();
            }
        }
        self.audit(
            "trust.advisory",
            "accepted",
            "",
            "",
            "",
            artifact_digest,
            "",
            &format!("advisory generation {}", state.generation),
        );
        Ok(())
    }

    fn advisory_state_path(&self) -> PathBuf {
        self.root.join("trust").join("advisory.json")
    }

    async fn install_verified(
        &self,
        layout: VerifiedLayout,
        advisory_generation: Option<u64>,
    ) -> Result<InstallOutcome, InstallError> {
        let metadata_digest = common::bytes_digest(&layout.metadata_bytes);
        let record = InstallRecord {
            schema: 1,
            artifact_digest: layout.artifact_digest.clone(),
            metadata_digest,
            binary_digest: layout.metadata.binary_digest.clone(),
            source_digest: layout.metadata.source_digest.clone(),
            package: layout.metadata.package.name.clone(),
            version: layout.metadata.package.version.clone(),
            signer: layout.metadata.signer.clone(),
            target: layout.metadata.target.clone(),
            advisory_generation,
            launcher_modes: Vec::new(),
            registered: Vec::new(),
            seeds: layout.metadata.seed_assets.clone(),
            installed_at: now_ms(),
        };
        let final_dir = self.release_dir(&layout.artifact_digest);
        if final_dir.exists() {
            let installed = self.verify_existing(&final_dir, &layout.artifact_digest)?;
            self.audit(
                "release.install",
                "converged",
                "",
                &installed.package,
                &installed.version,
                &installed.artifact_digest,
                &installed.signer,
                "immutable digest already present",
            );
            return Ok(InstallOutcome::Converged(installed));
        }

        std::fs::create_dir_all(self.staging_dir()).map_err(|error| {
            InstallError::new(
                "install.storage_unavailable",
                format!("installer cache is unavailable: {error}"),
            )
        })?;
        std::fs::create_dir_all(self.packages_dir()).map_err(|error| {
            InstallError::new(
                "install.storage_unavailable",
                format!("installer cache is unavailable: {error}"),
            )
        })?;
        let stage = self.staging_dir().join(format!(
            "release-{}-{}-{}",
            layout.artifact_digest.trim_start_matches("sha256:"),
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&stage).map_err(|error| {
            InstallError::new(
                "install.storage_unavailable",
                format!("staging directory could not be created: {error}"),
            )
        })?;
        let cleanup = StagingGuard {
            path: stage.clone(),
        };
        write_release(&stage, &layout, &record)?;

        self.audit(
            "smoke.run",
            "started",
            "",
            &record.package,
            &record.version,
            &record.artifact_digest,
            &record.signer,
            "constrained sandbox smoke",
        );
        let evidence =
            match smoke::run(&stage, &layout.metadata, &stage.join("smoke-workspace")).await {
                Ok(evidence) => evidence,
                Err(error) => {
                    self.audit(
                        "release.reject",
                        "rejected",
                        error.code,
                        &record.package,
                        &record.version,
                        &record.artifact_digest,
                        &record.signer,
                        &error.message,
                    );
                    return Err(error);
                }
            };
        let _ = std::fs::remove_dir_all(stage.join("smoke-workspace"));
        let mut record = record;
        record.registered = evidence.registered.clone();
        record.launcher_modes = evidence.modes.clone();
        std::fs::write(
            stage.join("record.json"),
            serde_json::to_vec_pretty(&record).map_err(|error| {
                InstallError::new("install.storage_unavailable", error.to_string())
            })?,
        )
        .map_err(|error| {
            InstallError::new(
                "install.storage_unavailable",
                format!("install record could not be written: {error}"),
            )
        })?;

        if final_dir.exists() {
            drop(cleanup);
            let installed = self.verify_existing(&final_dir, &layout.artifact_digest)?;
            self.audit(
                "release.install",
                "converged",
                "",
                &installed.package,
                &installed.version,
                &installed.artifact_digest,
                &installed.signer,
                "concurrent install converged on the same digest",
            );
            return Ok(InstallOutcome::Converged(installed));
        }
        match std::fs::rename(&stage, &final_dir) {
            Ok(()) => {}
            Err(_error) if final_dir.exists() => {
                drop(cleanup);
                let installed = self.verify_existing(&final_dir, &layout.artifact_digest)?;
                self.audit(
                    "release.install",
                    "converged",
                    "",
                    &installed.package,
                    &installed.version,
                    &installed.artifact_digest,
                    &installed.signer,
                    "concurrent install converged on the same digest",
                );
                return Ok(InstallOutcome::Converged(installed));
            }
            Err(error) => {
                return Err(InstallError::new(
                    "install.storage_unavailable",
                    format!("verified release could not be published: {error}"),
                ));
            }
        }
        drop(cleanup);
        let installed = installed_from_record(&final_dir, &record)?;
        self.audit(
            "release.install",
            "installed",
            "",
            &installed.package,
            &installed.version,
            &installed.artifact_digest,
            &installed.signer,
            "signature, compatibility, permissions, and smoke accepted",
        );
        Ok(InstallOutcome::Installed(installed))
    }

    /// List verified releases in the cache.
    pub fn installed(&self) -> Result<Vec<Installed>, InstallError> {
        let entries = match std::fs::read_dir(self.packages_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(InstallError::new(
                    "install.storage_unavailable",
                    format!("installer cache is unreadable: {error}"),
                ));
            }
        };
        let mut installed = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                InstallError::new(
                    "install.storage_unavailable",
                    format!("installer cache is unreadable: {error}"),
                )
            })?;
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let digest = format!("sha256:{}", entry.file_name().to_string_lossy());
            installed.push(self.verify_existing(&entry.path(), &digest)?);
        }
        installed.sort_by(|left, right| left.artifact_digest.cmp(&right.artifact_digest));
        Ok(installed)
    }

    fn verify_existing(
        &self,
        directory: &Path,
        expected_digest: &str,
    ) -> Result<Installed, InstallError> {
        let bytes = std::fs::read(directory.join("record.json")).map_err(|_| {
            InstallError::new(
                "install.cache_corrupt",
                "installed release has no installation record",
            )
        })?;
        let record: InstallRecord = serde_json::from_slice(&bytes).map_err(|_| {
            InstallError::new(
                "install.cache_corrupt",
                "installed release record is malformed",
            )
        })?;
        if record.artifact_digest != expected_digest {
            return Err(InstallError::new(
                "install.cache_corrupt",
                "installed release digest does not match its cache path",
            ));
        }
        let metadata_bytes = std::fs::read(directory.join("metadata.json")).map_err(|_| {
            InstallError::new("install.cache_corrupt", "installed release has no metadata")
        })?;
        if common::bytes_digest(&metadata_bytes) != record.metadata_digest {
            return Err(InstallError::new(
                "install.cache_corrupt",
                "installed release metadata was modified",
            ));
        }
        let binary = std::fs::read(directory.join("binary")).map_err(|_| {
            InstallError::new("install.cache_corrupt", "installed release has no binary")
        })?;
        if common::bytes_digest(&binary) != record.binary_digest {
            return Err(InstallError::new(
                "install.cache_corrupt",
                "installed release binary was modified",
            ));
        }
        installed_from_record(directory, &record)
    }

    fn release_dir(&self, digest: &str) -> PathBuf {
        self.packages_dir()
            .join(digest.trim_start_matches("sha256:"))
    }

    fn packages_dir(&self) -> PathBuf {
        self.root.join("packages").join("sha256")
    }

    fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }

    #[allow(clippy::too_many_arguments)]
    fn audit(
        &self,
        action: &str,
        outcome: &str,
        code: &str,
        package: &str,
        version: &str,
        digest: &str,
        signer: &str,
        detail: &str,
    ) {
        let entry = AuditEntry {
            at: now_ms(),
            action: common::redact(action),
            outcome: common::redact(outcome),
            code: common::redact(code),
            package: common::redact(package),
            version: common::redact(version),
            digest: common::redact(digest),
            signer: common::redact(signer),
            detail: common::redact(detail),
        };
        let Ok(line) = serde_json::to_string(&entry) else {
            return;
        };
        let _ = std::fs::create_dir_all(&self.root);
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("audit.jsonl"))
        {
            use std::io::Write;
            let _ = writeln!(file, "{line}");
        }
    }
}

struct StagingGuard {
    path: PathBuf,
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_release(
    directory: &Path,
    layout: &VerifiedLayout,
    record: &InstallRecord,
) -> Result<(), InstallError> {
    std::fs::write(directory.join("metadata.json"), &layout.metadata_bytes).map_err(storage)?;
    std::fs::write(directory.join("manifest.json"), &layout.manifest_bytes).map_err(storage)?;
    std::fs::write(directory.join("source.json"), &layout.source).map_err(storage)?;
    let binary = directory.join("binary");
    std::fs::write(&binary, &layout.binary).map_err(storage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
            .map_err(storage)?;
    }
    artifact::write_seed_files(directory, &layout.seed_files)?;
    std::fs::write(
        directory.join("record.json"),
        serde_json::to_vec_pretty(record)
            .map_err(|error| InstallError::new("install.storage_unavailable", error.to_string()))?,
    )
    .map_err(storage)?;
    Ok(())
}

fn installed_from_record(
    directory: &Path,
    record: &InstallRecord,
) -> Result<Installed, InstallError> {
    Ok(Installed {
        artifact_digest: record.artifact_digest.clone(),
        package: record.package.clone(),
        version: record.version.clone(),
        signer: record.signer.clone(),
        target: record.target.clone(),
        advisory_generation: record.advisory_generation,
        directory: directory.to_path_buf(),
        binary: directory.join("binary"),
        registered: record.registered.clone(),
        launcher_modes: record.launcher_modes.clone(),
        seeds: record.seeds.clone(),
    })
}

fn set_private_mode(path: &Path, mode: u32) -> Result<(), InstallError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(storage)?;
    }
    let _ = mode;
    Ok(())
}

fn storage(error: std::io::Error) -> InstallError {
    InstallError::new(
        "install.storage_unavailable",
        format!("verified release could not be staged: {error}"),
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
