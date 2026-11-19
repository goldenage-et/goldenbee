//! Reproducible release assembly over the existing signed OCI package format.
//!
//! The assembler binds a source identity, actual package probe output, signed
//! release metadata, a complete file checksum list, and an ed25519 signature
//! over that list. It never invents a second artifact or signature format.

use crate::artifact::{
    encode_metadata, read_layout, seed_assets, write_layout_with_seeds, SeedFile,
};
use crate::error::InstallError;
use base64::Engine;
use common::{
    bytes_digest, BuildEvidence, CapabilityDescriptor, PackageManifest, ReleaseMetadata,
    RELEASE_METADATA_SCHEMA,
};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

mod process;

/// Release assembly input schema.
pub const RELEASE_SPEC_SCHEMA: u32 = 1;
/// Written release manifest schema.
pub const RELEASE_MANIFEST_SCHEMA: u32 = 1;

/// Source identity bound into every package in one release assembly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    /// Verifiable source revision, for example `git:<commit>`.
    pub revision: String,
    /// Digest of the source tree archive or equivalent reproducible tree.
    pub tree_digest: String,
    /// Whether the working tree contained uncommitted changes at assembly.
    #[serde(default)]
    pub dirty: bool,
}

/// One package requested by a release specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSpec {
    /// Package name. Must match the probed binary.
    pub name: String,
    /// Package version. Must match the probed binary.
    pub version: String,
    /// Built Linux executable path.
    pub binary: PathBuf,
    /// Declared capabilities. Checked against the binary's actual probe.
    #[serde(default)]
    pub capabilities: Vec<CapabilityDescriptor>,
    /// Required permissions.
    #[serde(default)]
    pub required_permissions: Vec<String>,
    /// Compiled framework features required by this package.
    #[serde(default)]
    pub features: Vec<String>,
    /// Native Launcher modes claimed by this package.
    #[serde(default)]
    pub launcher_modes: Vec<String>,
    /// Compiled Worker hooks.
    #[serde(default)]
    pub hooks: Vec<String>,
    /// Optional relative seed-asset files below the package directory.
    #[serde(default)]
    pub seeds: Vec<String>,
}

/// One non-package executable shipped beside the Worker packages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSpec {
    /// Advertised artifact name, for example `gateway` or `cli`.
    pub name: String,
    /// Artifact kind, for example `executable` or `library-record`.
    pub kind: String,
    /// Built artifact path.
    pub path: PathBuf,
}

/// Complete release assembly specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseSpec {
    /// Specification schema.
    pub schema: u32,
    /// Human-readable release identity.
    pub release: String,
    /// Release signer identity trusted by installers.
    pub signer: String,
    /// Target triple advertised by every package.
    pub target: String,
    /// Shared Worker runtime compatibility.
    pub runtime: String,
    /// Pinned ADK compatibility.
    pub adk: String,
    /// Management protocol version implemented by every package.
    pub protocol: String,
    /// Lowest compatible protocol version.
    #[serde(default)]
    pub protocol_min: String,
    /// Highest compatible protocol version.
    #[serde(default)]
    pub protocol_max: String,
    /// Source revision, tree digest, and dirty flag.
    pub source: SourceIdentity,
    /// Self-contained Git bundle carrying the exact source commit.
    pub source_bundle: PathBuf,
    /// Build evidence recorded in signed package metadata.
    #[serde(default)]
    pub build: BuildEvidence,
    /// Packages included in this release.
    pub packages: Vec<PackageSpec>,
    /// Non-package artifacts shipped in the same signed release assembly.
    #[serde(default)]
    pub artifacts: Vec<ArtifactSpec>,
}

/// One shipped non-package artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseArtifact {
    /// Advertised name.
    pub name: String,
    /// Artifact kind.
    pub kind: String,
    /// Copied relative path below the release output root.
    pub file: String,
    /// Digest of the copied bytes.
    pub digest: String,
}

/// One assembled package entry in the written release manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleasePackage {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// Target triple.
    pub target: String,
    /// Relative directory below the release output root.
    pub directory: String,
    /// Immutable OCI manifest digest.
    pub artifact_digest: String,
    /// Signed metadata digest.
    pub metadata_digest: String,
    /// Binary payload digest.
    pub binary_digest: String,
    /// Source descriptor digest.
    pub source_digest: String,
    /// Capabilities actually reported by the binary probe.
    pub capabilities: Vec<String>,
    /// Launcher modes actually reported by the binary probe.
    pub launcher_modes: Vec<String>,
    /// Hooks actually reported by the binary probe.
    pub hooks: Vec<String>,
}

/// Written release manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    /// Manifest schema.
    pub schema: u32,
    /// Release identity.
    pub release: String,
    /// Signer identity.
    pub signer: String,
    /// Base64 public key that verifies `SHA256SUMS.ed25519`.
    pub signer_public_key: String,
    /// Source identity.
    pub source: SourceIdentity,
    /// Target triple.
    pub target: String,
    /// Shared runtime compatibility.
    pub runtime: String,
    /// Pinned ADK compatibility.
    pub adk: String,
    /// Management protocol.
    pub protocol: String,
    /// Packages in this release.
    pub packages: Vec<ReleasePackage>,
    /// Non-package artifacts shipped in this release.
    #[serde(default)]
    pub artifacts: Vec<ReleaseArtifact>,
    /// Digest of the exact release specification bytes.
    pub spec_digest: String,
    /// Digest of the build/provenance record.
    pub provenance_digest: String,
}

/// Result of assembling one release.
#[derive(Debug, Clone, Serialize)]
pub struct AssemblyReport {
    /// Output directory.
    pub output: PathBuf,
    /// Release identity.
    pub release: String,
    /// Written manifest.
    pub manifest: ReleaseManifest,
    /// Signed checksum digest.
    pub checksums_digest: String,
}

/// Generate one ed25519 signing seed and matching public key.
pub fn keygen() -> (String, String) {
    let signing = SigningKey::generate(&mut OsRng);
    let seed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signing.to_bytes());
    let public =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signing.verifying_key().as_bytes());
    (seed, public)
}

/// Assemble a release from a specification and exact signing seed file.
pub fn assemble(
    spec_path: &Path,
    output: &Path,
    signing_key: &Path,
) -> Result<AssemblyReport, InstallError> {
    let spec_bytes = std::fs::read(spec_path).map_err(|_| {
        InstallError::new(
            "release.spec_unavailable",
            "release specification is unreadable",
        )
    })?;
    let spec: ReleaseSpec = serde_json::from_slice(&spec_bytes).map_err(|_| {
        InstallError::new("release.spec_invalid", "release specification is malformed")
    })?;
    if spec.schema != RELEASE_SPEC_SCHEMA {
        return Err(InstallError::new(
            "release.spec_invalid",
            "release specification schema is unsupported",
        ));
    }
    if spec.release.trim().is_empty()
        || spec.signer.trim().is_empty()
        || spec.target.trim().is_empty()
        || spec.packages.is_empty()
    {
        return Err(InstallError::new(
            "release.spec_invalid",
            "release, signer, target, and at least one package are required",
        ));
    }
    let mut names = BTreeSet::new();
    for package in &spec.packages {
        validate_name(&package.name)?;
        validate_name(&package.version)?;
        if !names.insert(&package.name) {
            return Err(invalid("duplicate package name"));
        }
        for seed in &package.seeds {
            validate_path(seed)?;
        }
    }
    names.clear();
    for artifact in &spec.artifacts {
        validate_name(&artifact.name)?;
        if !names.insert(&artifact.name) {
            return Err(invalid("duplicate artifact name"));
        }
    }
    validate_name(&spec.target)?;
    if spec.source.dirty && spec.build.reproducible {
        return Err(invalid(
            "dirty input cannot claim reproducible build evidence",
        ));
    }
    let signing = load_signing_key(signing_key)?;
    let archive = source_archive(&spec.source_bundle, &spec.source)?;

    let source_bytes = serde_json::to_vec(&serde_json::json!({
        "revision": spec.source.revision,
        "tree": spec.source.tree_digest,
        "dirty": spec.source.dirty,
    }))
    .map_err(|error| InstallError::new("release.spec_invalid", error.to_string()))?;

    // Probe every package before writing anything. The probe is the binary's
    // own report, so the signed metadata cannot drift from compiled
    // registrations.
    let mut probes = Vec::new();
    for package in &spec.packages {
        let report = probe(package)?;
        if report.runtime != spec.runtime
            || report.adk != spec.adk
            || report.protocol != spec.protocol
        {
            return Err(invalid(
                "binary compatibility disagrees with release specification",
            ));
        }
        probes.push(report);
    }

    // Refuse existing destinations. Never traverse or remove operator files,
    // keys, symlinks, logs, or a prior partially assembled release.
    std::fs::create_dir(output).map_err(storage_error)?;
    std::fs::write(
        output.join("source.bundle"),
        std::fs::read(&spec.source_bundle).map_err(storage_error)?,
    )
    .map_err(storage_error)?;
    std::fs::write(output.join("source.tar"), archive).map_err(storage_error)?;
    std::fs::write(output.join("release-spec.json"), &spec_bytes).map_err(storage_error)?;

    let binaries = spec
        .packages
        .iter()
        .map(|package| std::fs::read(&package.binary).map_err(storage_error))
        .collect::<Result<Vec<_>, _>>()?;
    let binary_digests: BTreeMap<&str, String> = spec
        .packages
        .iter()
        .zip(&binaries)
        .map(|(package, bytes)| (package.name.as_str(), bytes_digest(bytes)))
        .collect();
    let mut artifacts = Vec::new();
    if !spec.artifacts.is_empty() {
        let directory = output.join("artifacts");
        std::fs::create_dir_all(&directory).map_err(storage_error)?;
        for artifact in &spec.artifacts {
            let bytes = std::fs::read(&artifact.path).map_err(storage_error)?;
            let file = format!("artifacts/{}", artifact.name);
            std::fs::write(output.join(&file), &bytes).map_err(storage_error)?;
            let mode = if artifact.kind == "executable" {
                0o755
            } else {
                0o644
            };
            std::fs::set_permissions(output.join(&file), std::fs::Permissions::from_mode(mode))
                .map_err(storage_error)?;
            artifacts.push(ReleaseArtifact {
                name: artifact.name.clone(),
                kind: artifact.kind.clone(),
                file,
                digest: bytes_digest(&bytes),
            });
        }
    }
    let provenance = serde_json::json!({
        "release": spec.release,
        "source": spec.source,
        "target": spec.target,
        "runtime": spec.runtime,
        "adk": spec.adk,
        "protocol": spec.protocol,
        "protocol_min": spec.protocol_min,
        "protocol_max": spec.protocol_max,
        "spec_digest": bytes_digest(&spec_bytes),
        "binaries": binary_digests,
        "artifacts": &artifacts,
        "build": spec.build,
        "probes": probes,
    });
    let provenance_bytes = serde_json::to_vec_pretty(&provenance)
        .map_err(|error| InstallError::new("release.spec_invalid", error.to_string()))?;
    let provenance_digest = bytes_digest(&provenance_bytes);
    std::fs::write(output.join("provenance.json"), &provenance_bytes).map_err(storage_error)?;

    let mut packages = Vec::new();
    for ((package, probe), binary) in spec.packages.iter().zip(&probes).zip(&binaries) {
        let mut seeds = Vec::new();
        let base = package.binary.parent().unwrap_or_else(|| Path::new("."));
        for seed in &package.seeds {
            let path = base.join(seed);
            let bytes = std::fs::read(&path).map_err(|_| {
                InstallError::new(
                    "release.seed_unavailable",
                    format!("seed asset '{seed}' is unreadable"),
                )
            })?;
            seeds.push(SeedFile::new(seed.clone(), bytes));
        }
        let capabilities = if package.capabilities.is_empty() {
            probe.capabilities.clone()
        } else {
            package.capabilities.clone()
        };
        let required_permissions = if package.required_permissions.is_empty() {
            probe.required_permissions.clone()
        } else {
            package.required_permissions.clone()
        };
        let features = if package.features.is_empty() {
            probe.features.clone()
        } else {
            package.features.clone()
        };
        let launcher_modes = if package.launcher_modes.is_empty() {
            probe.launcher_modes.clone()
        } else {
            package.launcher_modes.clone()
        };
        let hooks = if package.hooks.is_empty() {
            probe.hooks.clone()
        } else {
            package.hooks.clone()
        };
        let metadata = ReleaseMetadata {
            schema: RELEASE_METADATA_SCHEMA,
            package: PackageManifest {
                name: package.name.clone(),
                version: package.version.clone(),
                protocol: spec.protocol.clone(),
                runtime: spec.runtime.clone(),
                adk: spec.adk.clone(),
                capabilities,
                required_permissions,
                features,
                launcher_modes,
                hooks,
            },
            source_digest: bytes_digest(&source_bytes),
            binary_digest: bytes_digest(binary),
            source_revision: spec.source.revision.clone(),
            target: spec.target.clone(),
            protocol_min: spec.protocol_min.clone(),
            protocol_max: spec.protocol_max.clone(),
            signer: spec.signer.clone(),
            build: BuildEvidence {
                evidence_digest: provenance_digest.clone(),
                ..spec.build.clone()
            },
            seed_assets: seed_assets(&seeds),
        };
        let metadata_bytes = encode_metadata(&metadata)?;
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            signing
                .sign(bytes_digest(&metadata_bytes).as_bytes())
                .to_bytes(),
        );
        let directory = format!(
            "packages/{}-{}-{}",
            package.name, package.version, spec.target
        );
        let path = output.join(&directory);
        let digests =
            write_layout_with_seeds(&path, &metadata, binary, &source_bytes, &seeds, &signature)?;
        packages.push(ReleasePackage {
            name: package.name.clone(),
            version: package.version.clone(),
            target: spec.target.clone(),
            directory,
            artifact_digest: digests.artifact_digest,
            metadata_digest: digests.metadata_digest,
            binary_digest: digests.binary_digest,
            source_digest: digests.source_digest,
            capabilities: probe.registered.clone(),
            launcher_modes: probe.launcher_modes.clone(),
            hooks: probe.hooks.clone(),
        });
    }

    let manifest = ReleaseManifest {
        schema: RELEASE_MANIFEST_SCHEMA,
        release: spec.release,
        signer: spec.signer,
        signer_public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(signing.verifying_key().as_bytes()),
        source: spec.source,
        target: spec.target,
        runtime: spec.runtime,
        adk: spec.adk,
        protocol: spec.protocol,
        packages,
        artifacts,
        spec_digest: bytes_digest(&spec_bytes),
        provenance_digest,
    };
    std::fs::write(
        output.join("release-manifest.json"),
        serde_json::to_vec_pretty(&manifest)
            .map_err(|error| InstallError::new("release.spec_invalid", error.to_string()))?,
    )
    .map_err(storage_error)?;

    std::fs::write(
        output.join("signer-public-key.txt"),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signing.verifying_key().as_bytes()),
    )
    .map_err(storage_error)?;
    let checksums = checksum_lines(output)?;
    let checksum_bytes = checksums.as_bytes();
    std::fs::write(output.join("SHA256SUMS"), checksum_bytes).map_err(storage_error)?;
    let checksum_signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(signing.sign(checksum_bytes).to_bytes());
    std::fs::write(output.join("SHA256SUMS.ed25519"), checksum_signature).map_err(storage_error)?;

    Ok(AssemblyReport {
        output: output.to_path_buf(),
        release: manifest.release.clone(),
        manifest,
        checksums_digest: bytes_digest(checksum_bytes),
    })
}

/// Verify one assembled release directory, checksum file, and detached
/// ed25519 signature. Source identity and immutable package digests are checked
/// without executing any package binary.
pub fn verify(output: &Path, trusted_public_key: &str) -> Result<ReleaseManifest, InstallError> {
    let manifest_bytes = std::fs::read(output.join("release-manifest.json")).map_err(|_| {
        InstallError::new(
            "release.manifest_unavailable",
            "release manifest is unreadable",
        )
    })?;
    let manifest: ReleaseManifest = serde_json::from_slice(&manifest_bytes).map_err(|_| {
        InstallError::new("release.manifest_invalid", "release manifest is malformed")
    })?;
    if manifest.schema != RELEASE_MANIFEST_SCHEMA {
        return Err(InstallError::new(
            "release.manifest_invalid",
            "release manifest schema is unsupported",
        ));
    }
    let public = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(trusted_public_key.trim().as_bytes())
        .map_err(|_| {
            InstallError::new(
                "release.signature_invalid",
                "signer public key is malformed",
            )
        })?;
    let public: [u8; 32] = public.try_into().map_err(|_| {
        InstallError::new(
            "release.signature_invalid",
            "signer public key is malformed",
        )
    })?;
    let verifying = VerifyingKey::from_bytes(&public).map_err(|_| {
        InstallError::new("release.signature_invalid", "signer public key is invalid")
    })?;
    let signature = std::fs::read_to_string(output.join("SHA256SUMS.ed25519")).map_err(|_| {
        InstallError::new(
            "release.signature_unavailable",
            "checksum signature is unreadable",
        )
    })?;
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature.trim().as_bytes())
        .map_err(|_| {
            InstallError::new(
                "release.signature_invalid",
                "checksum signature is malformed",
            )
        })?;
    let signature: [u8; 64] = signature.try_into().map_err(|_| {
        InstallError::new(
            "release.signature_invalid",
            "checksum signature is malformed",
        )
    })?;
    let signature = ed25519_dalek::Signature::from_bytes(&signature);
    let checksums = std::fs::read(output.join("SHA256SUMS")).map_err(|_| {
        InstallError::new("release.checksums_unavailable", "checksums are unreadable")
    })?;
    verifying
        .verify_strict(&checksums, &signature)
        .map_err(|_| {
            InstallError::new(
                "release.signature_invalid",
                "checksum signature does not verify",
            )
        })?;

    if checksum_lines(output)?.as_bytes() != checksums {
        return Err(InstallError::new(
            "release.checksum_mismatch",
            "signed checksum inventory does not cover exactly the release files and bytes",
        ));
    }
    if manifest.signer_public_key != trusted_public_key.trim() {
        return Err(invalid("manifest signer key differs from trusted key"));
    }
    let archive = source_archive(&output.join("source.bundle"), &manifest.source)?;
    if archive != std::fs::read(output.join("source.tar")).map_err(storage_error)? {
        return Err(invalid("source archive differs from its Git commit"));
    }
    if bytes_digest(&std::fs::read(output.join("release-spec.json")).map_err(storage_error)?)
        != manifest.spec_digest
        || bytes_digest(&std::fs::read(output.join("provenance.json")).map_err(storage_error)?)
            != manifest.provenance_digest
    {
        return Err(invalid(
            "provenance or specification digest differs from manifest",
        ));
    }

    for package in &manifest.packages {
        validate_path(&package.directory)?;
        let layout = read_layout(&output.join(&package.directory))?;
        if layout.artifact_digest != package.artifact_digest {
            return Err(InstallError::new(
                "release.manifest_mismatch",
                format!("package '{}' digest does not match", package.name),
            ));
        }
        if layout.metadata.source_revision != manifest.source.revision {
            return Err(InstallError::new(
                "release.manifest_mismatch",
                format!("package '{}' source revision does not match", package.name),
            ));
        }
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&layout.signature)
            .map_err(|_| invalid("invalid package signature"))?;
        let signature = ed25519_dalek::Signature::from_slice(&signature)
            .map_err(|_| invalid("invalid package signature"))?;
        verifying
            .verify_strict(bytes_digest(&layout.metadata_bytes).as_bytes(), &signature)
            .map_err(|_| invalid("package signature does not verify"))?;
        let metadata = &layout.metadata;
        let source: serde_json::Value = serde_json::from_slice(&layout.source)
            .map_err(|_| invalid("invalid package source descriptor"))?;
        if metadata.package.name != package.name
            || metadata.package.version != package.version
            || metadata.target != manifest.target
            || metadata.package.runtime != manifest.runtime
            || metadata.package.adk != manifest.adk
            || metadata.package.protocol != manifest.protocol
            || metadata.signer != manifest.signer
            || metadata.build.evidence_digest != manifest.provenance_digest
            || bytes_digest(&layout.metadata_bytes) != package.metadata_digest
            || metadata.binary_digest != package.binary_digest
            || metadata.source_digest != package.source_digest
            || source["tree"] != manifest.source.tree_digest
            || source["dirty"] != manifest.source.dirty
        {
            return Err(invalid(
                "package metadata disagrees with authenticated release",
            ));
        }
    }
    for artifact in &manifest.artifacts {
        validate_path(&artifact.file)?;
        if bytes_digest(&std::fs::read(output.join(&artifact.file)).map_err(storage_error)?)
            != artifact.digest
        {
            return Err(invalid("artifact digest differs from manifest"));
        }
    }
    Ok(manifest)
}

#[derive(Debug, Serialize, Deserialize)]
struct Probe {
    package: String,
    version: String,
    runtime: String,
    adk: String,
    protocol: String,
    registered: Vec<String>,
    #[serde(default)]
    hooks: Vec<String>,
    #[serde(default)]
    launcher_modes: Vec<String>,
    #[serde(default)]
    capabilities: Vec<CapabilityDescriptor>,
    #[serde(default)]
    required_permissions: Vec<String>,
    #[serde(default)]
    features: Vec<String>,
}

fn probe(package: &PackageSpec) -> Result<Probe, InstallError> {
    let output =
        process::run(Command::new(&package.binary).arg("probe"), 1024 * 1024).map_err(|_| {
            InstallError::new(
                "release.probe_failed",
                format!("package '{}' could not be probed", package.name),
            )
        })?;
    if !output.status.success() {
        return Err(InstallError::new(
            "release.probe_failed",
            format!("package '{}' probe failed", package.name),
        ));
    }
    let probe: Probe = serde_json::from_slice(&output.stdout).map_err(|_| {
        InstallError::new(
            "release.probe_invalid",
            format!("package '{}' probe is malformed", package.name),
        )
    })?;
    if probe.package != package.name || probe.version != package.version {
        return Err(InstallError::new(
            "release.probe_mismatch",
            format!(
                "package '{}' probe reports {} {}",
                package.name, probe.package, probe.version
            ),
        ));
    }
    let declared_capabilities = if package.capabilities.is_empty() {
        &probe.capabilities
    } else {
        &package.capabilities
    };
    let mut expected: Vec<String> = declared_capabilities
        .iter()
        .map(|capability| capability.id.clone())
        .collect();
    expected.sort();
    let mut actual = probe.registered.clone();
    actual.sort();
    if expected != actual {
        return Err(InstallError::new(
            "release.probe_mismatch",
            format!(
                "package '{}' capabilities do not match its probe",
                package.name
            ),
        ));
    }
    if !package.launcher_modes.is_empty() && probe.launcher_modes != package.launcher_modes {
        return Err(InstallError::new(
            "release.probe_mismatch",
            format!(
                "package '{}' launcher modes do not match its probe",
                package.name
            ),
        ));
    }
    if !package.hooks.is_empty() && probe.hooks != package.hooks {
        return Err(InstallError::new(
            "release.probe_mismatch",
            format!("package '{}' hooks do not match its probe", package.name),
        ));
    }
    if (!package.features.is_empty() && package.features != probe.features)
        || (!package.required_permissions.is_empty()
            && package.required_permissions != probe.required_permissions)
        || (!package.capabilities.is_empty() && package.capabilities != probe.capabilities)
    {
        return Err(invalid(
            "declared features, permissions, or capabilities differ from binary probe",
        ));
    }
    Ok(probe)
}

fn checksum_lines(root: &Path) -> Result<String, InstallError> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    let mut text = String::new();
    for relative in files {
        if relative == "SHA256SUMS" || relative == "SHA256SUMS.ed25519" {
            continue;
        }
        let bytes = std::fs::read(root.join(&relative)).map_err(storage_error)?;
        text.push_str(&format!("{}  {}\n", bytes_digest(&bytes), relative));
    }
    Ok(text)
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<String>,
) -> Result<(), InstallError> {
    let entries = std::fs::read_dir(directory).map_err(storage_error)?;
    for entry in entries {
        let entry = entry.map_err(storage_error)?;
        let path = entry.path();
        let kind = entry.file_type().map_err(storage_error)?;
        if kind.is_symlink() {
            return Err(invalid("release files cannot be symlinks"));
        }
        if kind.is_dir() {
            collect_files(root, &path, files)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(root).map_err(|_| {
                InstallError::new("release.path_invalid", "release path escapes output")
            })?;
            let relative = relative
                .to_str()
                .ok_or_else(|| invalid("non-UTF8 release path"))?;
            validate_path(relative)?;
            files.push(relative.to_string());
        } else {
            return Err(invalid("release contains a non-regular file"));
        }
    }
    Ok(())
}

fn load_signing_key(path: &Path) -> Result<SigningKey, InstallError> {
    let text = std::fs::read_to_string(path).map_err(|_| {
        InstallError::new(
            "release.key_unavailable",
            "release signing key is unreadable",
        )
    })?;
    let text = text.trim();
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(text))
        .map_err(|_| {
            InstallError::new("release.key_invalid", "release signing key is malformed")
        })?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
        InstallError::new("release.key_invalid", "release signing key is malformed")
    })?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn storage_error<E: std::fmt::Display>(error: E) -> InstallError {
    InstallError::new("release.storage_unavailable", error.to_string())
}

fn invalid(message: &str) -> InstallError {
    InstallError::new("release.spec_invalid", message)
}

fn validate_name(name: &str) -> Result<(), InstallError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.+".contains(&byte))
    {
        return Err(invalid("release names must be single safe path components"));
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<(), InstallError> {
    if path.is_empty()
        || path.contains(['\\', '\n', '\r', '\0'])
        || !Path::new(path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Err(invalid("release path must stay inside output"));
    }
    Ok(())
}

fn source_archive(bundle: &Path, source: &SourceIdentity) -> Result<Vec<u8>, InstallError> {
    let revision = source
        .revision
        .strip_prefix("git:")
        .ok_or_else(|| invalid("source requires a Git commit"))?;
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("source revision requires a full Git commit hash"));
    }
    let directory = tempfile::tempdir().map_err(storage_error)?;
    let git = |args: &[&str]| -> Result<Vec<u8>, InstallError> {
        let output = process::run(
            Command::new("git")
                .current_dir(directory.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .args(args),
            64 * 1024 * 1024,
        )?;
        if !output.status.success() {
            return Err(invalid("source bundle does not verify against its commit"));
        }
        Ok(output.stdout)
    };
    let bundle = std::fs::canonicalize(bundle).map_err(storage_error)?;
    let bundle = bundle
        .to_str()
        .ok_or_else(|| invalid("non-UTF8 source bundle path"))?;
    git(&["init", "--bare", "--quiet"])?;
    git(&[
        "-c",
        "protocol.file.allow=always",
        "fetch",
        "--quiet",
        "--no-tags",
        bundle,
        revision,
    ])?;
    let archive = git(&["archive", "--format=tar", revision])?;
    if bytes_digest(&archive) != source.tree_digest {
        return Err(invalid(
            "source tree digest does not match the bundled Git commit",
        ));
    }
    Ok(archive)
}
