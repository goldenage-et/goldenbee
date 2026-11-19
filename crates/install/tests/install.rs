//! Public installer behavior: real OCI layouts, real ed25519 signatures, and
//! real constrained smoke execution.

use base64::Engine;
use common::{bytes_digest, CapabilityDescriptor, PackageManifest, ReleaseMetadata, SeedAsset};
use ed25519_dalek::{Signer, SigningKey};
use install::{
    encode_metadata, write_layout, write_layout_with_seeds, InstallOutcome, Installer, SeedFile,
    TrustPolicy,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CANARY: &str = "sk-installer-canary-do-not-log";

fn key() -> SigningKey {
    SigningKey::from_bytes(&[7_u8; 32])
}

fn other_key() -> SigningKey {
    SigningKey::from_bytes(&[9_u8; 32])
}

fn public_key_b64() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key().verifying_key().as_bytes())
}

fn probe_binary(registered: &[&str], modes: &[&str], package: &str, version: &str) -> Vec<u8> {
    let registered = serde_json::to_string(registered).unwrap();
    let modes = serde_json::to_string(modes).unwrap();
    format!(
        "#!/bin/sh\n\
         case \"$1\" in\n\
         probe)\n\
           printf '%s' '{{\"package\":\"{package}\",\"version\":\"{version}\",\"protocol\":\"1.0\",\"adk\":\"2.3.0\",\"registered\":{registered},\"hooks\":[],\"launcher_modes\":{modes},\"serve_ready\":true}}'\n\
           ;;\n\
         dev|console)\n\
           printf 'launcher\\n'\n\
           ;;\n\
         serve)\n\
           printf 'serve composition ready\\n'\n\
           ;;\n\
         *)\n\
           exit 3\n\
           ;;\n\
         esac\n"
    )
    .into_bytes()
}

fn boundary_probe_binary() -> Vec<u8> {
    "#!/bin/sh\n\
     check_boundary() {\n\
       if [ \"${MALLOC_ARENA_MAX:-}\" != \"1\" ]; then\n\
         echo 'allocator arenas are not bounded' >&2\n\
         exit 4\n\
       fi\n\
       if [ \"$(ulimit -v)\" != \"524288\" ]; then\n\
         echo \"address-space ceiling changed: $(ulimit -v)\" >&2\n\
         exit 5\n\
       fi\n\
     }\n\
     case \"$1\" in\n\
     probe)\n\
       check_boundary\n\
       printf '%s' '{\"package\":\"demo\",\"version\":\"0.1.0\",\"protocol\":\"1.0\",\"adk\":\"2.3.0\",\"registered\":[\"demo\"],\"hooks\":[],\"launcher_modes\":[\"dev\"],\"serve_ready\":false}'\n\
       ;;\n\
     dev|console)\n\
       check_boundary\n\
       printf 'launcher\\n'\n\
       ;;\n\
     *)\n\
       exit 3\n\
       ;;\n\
     esac\n"
        .as_bytes()
        .to_vec()
}

fn metadata(registered: &[&str], modes: &[&str], binary: &[u8], source: &[u8]) -> ReleaseMetadata {
    ReleaseMetadata {
        schema: common::RELEASE_METADATA_SCHEMA,
        package: PackageManifest {
            name: "demo".into(),
            version: "0.1.0".into(),
            protocol: "1.0".into(),
            runtime: "0.1.0".into(),
            adk: "2.3.0".into(),
            capabilities: registered
                .iter()
                .map(|id| CapabilityDescriptor {
                    id: (*id).into(),
                    kind: "agent".into(),
                    description: "fixture capability".into(),
                    permissions: vec!["task:submit".into()],
                })
                .collect(),
            required_permissions: vec!["task:submit".into(), "session:write".into()],
            features: vec!["agents".into(), "runner".into(), "sessions".into()],
            launcher_modes: modes.iter().map(|mode| (*mode).into()).collect(),
            hooks: Vec::new(),
        },
        source_digest: bytes_digest(source),
        binary_digest: bytes_digest(binary),
        source_revision: "git:0123456789abcdef".into(),
        target: TrustPolicy::host_target(),
        protocol_min: "1.0".into(),
        protocol_max: "1.0".into(),
        signer: "publisher".into(),
        build: common::BuildEvidence {
            toolchain: "rust 1.83".into(),
            profile: "release".into(),
            command: "cargo build --release".into(),
            started_at: "2026-11-18T00:00:00Z".into(),
            finished_at: "2026-11-18T00:01:00Z".into(),
            reproducible: true,
            evidence_digest: bytes_digest(b"build log"),
        },
        seed_assets: Vec::new(),
    }
}

fn policy() -> TrustPolicy {
    TrustPolicy {
        trusted_signers: BTreeMap::from([("publisher".into(), public_key_b64())]),
        targets: vec![TrustPolicy::host_target()],
        runtimes: vec!["0.1.0".into()],
        adks: vec!["2.3.0".into()],
        protocols: vec!["1.0".into()],
        features: vec!["agents".into(), "runner".into(), "sessions".into()],
        approved_permissions: vec!["task:submit".into(), "session:write".into()],
        sensitive_permissions: vec![
            "credential:read".into(),
            "exec:host".into(),
            "net:outbound".into(),
        ],
        ..TrustPolicy::default()
    }
}

fn source_bytes() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "revision": "git:0123456789abcdef",
        "tree": bytes_digest(b"fixture tree"),
    }))
    .unwrap()
}

fn sign(metadata: &ReleaseMetadata, signing: &SigningKey) -> String {
    let digest = bytes_digest(&encode_metadata(metadata).unwrap());
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(signing.sign(digest.as_bytes()).to_bytes())
}

fn build(
    directory: &Path,
    metadata: &ReleaseMetadata,
    binary: &[u8],
    source: &[u8],
    signing: &SigningKey,
) {
    let signature = sign(metadata, signing);
    write_layout(directory, metadata, binary, source, &signature).unwrap();
}

fn digests(directory: &Path) -> install::ArtifactDigests {
    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("index.json")).unwrap()).unwrap();
    let digest = index["manifests"][0]["digest"].as_str().unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(blob_path(directory, digest)).unwrap()).unwrap();
    let metadata_digest = manifest["config"]["digest"].as_str().unwrap();
    let layers = manifest["layers"].as_array().unwrap();
    install::ArtifactDigests {
        artifact_digest: digest.into(),
        metadata_digest: metadata_digest.into(),
        binary_digest: layers[0]["digest"].as_str().unwrap().into(),
        source_digest: layers[1]["digest"].as_str().unwrap().into(),
        seed_digest: layers
            .iter()
            .find(|layer| layer["mediaType"] == install::SEED_MEDIA_TYPE)
            .map(|layer| layer["digest"].as_str().unwrap().into()),
    }
}

fn blob_path(directory: &Path, digest: &str) -> PathBuf {
    directory
        .join("blobs")
        .join("sha256")
        .join(digest.trim_start_matches("sha256:"))
}

fn root() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn cache_is_empty(root: &Path) {
    let packages = root.join("packages").join("sha256");
    let entries = std::fs::read_dir(&packages)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(entries, 0, "rejected release must not be cached");
    let staging = root.join("staging");
    let entries = std::fs::read_dir(&staging)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(
        entries, 0,
        "rejected release must clean its staging directory"
    );
}

fn audit(root: &Path) -> String {
    std::fs::read_to_string(root.join("audit.jsonl")).unwrap_or_default()
}

#[test]
fn inspects_a_valid_local_artifact_without_executing_it() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let metadata = metadata(&["demo"], &["dev"], &binary, &source);
    build(artifact.path(), &metadata, &binary, &source, &key());
    let cache = root();
    let installer = Installer::new(cache.path(), policy());

    let inspection = installer.inspect(artifact.path()).unwrap();

    assert_eq!(inspection.metadata.package.name, "demo");
    assert_eq!(inspection.metadata.package.version, "0.1.0");
    assert_eq!(inspection.metadata.target, TrustPolicy::host_target());
    assert_eq!(inspection.metadata.binary_digest, bytes_digest(&binary));
    assert_eq!(inspection.metadata.source_digest, bytes_digest(&source));
    assert_eq!(
        inspection.artifact_digest,
        digests(artifact.path()).artifact_digest
    );
    assert!(!cache.path().join("packages").exists());
}

#[test]
fn inspection_never_executes_the_candidate_binary() {
    let artifact = root();
    let sentinel = artifact.path().join("EXECUTED");
    let binary = format!("#!/bin/sh\ntouch '{}'\nexit 0\n", sentinel.display()).into_bytes();
    let source = source_bytes();
    let metadata = metadata(&["demo"], &["dev"], &binary, &source);
    build(artifact.path(), &metadata, &binary, &source, &key());
    let artifact_digests = digests(artifact.path());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            blob_path(artifact.path(), &artifact_digests.binary_digest),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    Installer::new(root().path(), policy())
        .inspect(artifact.path())
        .unwrap();

    assert!(
        !sentinel.exists(),
        "inspection must not execute the candidate"
    );
}

#[test]
fn tampered_binary_bytes_are_rejected_before_smoke() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let metadata = metadata(&["demo"], &["dev"], &binary, &source);
    build(artifact.path(), &metadata, &binary, &source, &key());
    let artifact_digests = digests(artifact.path());
    let path = blob_path(artifact.path(), &artifact_digests.binary_digest);
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();

    let cache = root();
    let error = Installer::new(cache.path(), policy())
        .install_blocking(artifact.path())
        .unwrap_err();

    assert_eq!(error.code, "install.layer_digest_mismatch");
    assert!(!audit(cache.path()).contains("smoke.run"));
    cache_is_empty(cache.path());
}

#[test]
fn tampered_manifest_bytes_are_rejected_before_smoke() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let metadata = metadata(&["demo"], &["dev"], &binary, &source);
    build(artifact.path(), &metadata, &binary, &source, &key());
    let artifact_digests = digests(artifact.path());
    let path = blob_path(artifact.path(), &artifact_digests.artifact_digest);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.push(b' ');
    std::fs::write(&path, bytes).unwrap();

    let cache = root();
    let error = Installer::new(cache.path(), policy())
        .install_blocking(artifact.path())
        .unwrap_err();

    assert_eq!(error.code, "install.manifest_digest_mismatch");
    assert!(!audit(cache.path()).contains("smoke.run"));
    cache_is_empty(cache.path());
}

#[test]
fn rewritten_metadata_without_a_new_signature_is_rejected() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let metadata = metadata(&["demo"], &["dev"], &binary, &source);
    build(artifact.path(), &metadata, &binary, &source, &key());
    let artifact_digests = digests(artifact.path());

    let mut changed = metadata.clone();
    changed.package.capabilities.push(CapabilityDescriptor {
        id: "ghost".into(),
        kind: "agent".into(),
        description: "unexpected".into(),
        permissions: vec!["task:submit".into()],
    });
    let changed_bytes = encode_metadata(&changed).unwrap();
    let changed_digest = bytes_digest(&changed_bytes);
    std::fs::write(blob_path(artifact.path(), &changed_digest), &changed_bytes).unwrap();
    let manifest_path = blob_path(artifact.path(), &artifact_digests.artifact_digest);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["config"]["digest"] = changed_digest.clone().into();
    manifest["config"]["size"] = changed_bytes.len().into();
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let new_manifest_digest = bytes_digest(&manifest_bytes);
    std::fs::write(
        blob_path(artifact.path(), &new_manifest_digest),
        &manifest_bytes,
    )
    .unwrap();
    let index_path = artifact.path().join("index.json");
    let mut index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
    index["manifests"][0]["digest"] = new_manifest_digest.into();
    index["manifests"][0]["size"] = manifest_bytes.len().into();
    std::fs::write(index_path, serde_json::to_vec(&index).unwrap()).unwrap();

    let cache = root();
    let error = Installer::new(cache.path(), policy())
        .install_blocking(artifact.path())
        .unwrap_err();

    assert_eq!(error.code, "install.signature_invalid");
    cache_is_empty(cache.path());
}

#[test]
fn bad_signature_and_untrusted_signer_are_rejected() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let metadata = metadata(&["demo"], &["dev"], &binary, &source);
    build(artifact.path(), &metadata, &binary, &source, &other_key());

    let cache = root();
    let error = Installer::new(cache.path(), policy())
        .install_blocking(artifact.path())
        .unwrap_err();
    assert_eq!(error.code, "install.signature_invalid");

    let artifact = root();
    let mut untrusted = metadata.clone();
    untrusted.signer = CANARY.into();
    build(artifact.path(), &untrusted, &binary, &source, &key());
    let cache = root();
    let error = Installer::new(cache.path(), policy())
        .install_blocking(artifact.path())
        .unwrap_err();
    assert_eq!(error.code, "install.signer_untrusted");
    let log = audit(cache.path());
    assert!(!log.contains(CANARY), "audit must redact signer canaries");
    assert!(log.contains("install.signer_untrusted"));
    cache_is_empty(cache.path());
}

#[test]
fn unsupported_target_is_rejected_before_smoke() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let mut metadata = metadata(&["demo"], &["dev"], &binary, &source);
    metadata.target = "aarch64-unknown-linux-gnu".into();
    build(artifact.path(), &metadata, &binary, &source, &key());

    let cache = root();
    let error = Installer::new(cache.path(), policy())
        .install_blocking(artifact.path())
        .unwrap_err();

    assert_eq!(error.code, "install.target_unsupported");
    assert!(!audit(cache.path()).contains("smoke.run"));
    cache_is_empty(cache.path());
}

#[test]
fn denied_sensitive_permission_is_rejected_before_execution_or_credentials() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let mut metadata = metadata(&["demo"], &["dev"], &binary, &source);
    metadata
        .package
        .required_permissions
        .push("credential:read".into());
    build(artifact.path(), &metadata, &binary, &source, &key());

    let cache = root();
    let error = Installer::new(cache.path(), policy())
        .install_blocking(artifact.path())
        .unwrap_err();

    assert_eq!(error.code, "install.sensitive_permission_denied");
    assert!(!audit(cache.path()).contains("smoke.run"));
    cache_is_empty(cache.path());
}

#[test]
fn capability_mismatch_after_smoke_is_rejected_and_cleaned_up() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let metadata = metadata(&["demo", "ghost"], &["dev"], &binary, &source);
    build(artifact.path(), &metadata, &binary, &source, &key());

    let cache = root();
    let error = Installer::new(cache.path(), policy())
        .install_blocking(artifact.path())
        .unwrap_err();

    assert_eq!(error.code, "install.smoke_mismatch");
    let log = audit(cache.path());
    assert!(log.contains("smoke.run"));
    assert!(log.contains("install.smoke_mismatch"));
    cache_is_empty(cache.path());
}

type Mutation = fn(&mut ReleaseMetadata);
#[test]
fn incompatible_runtime_adk_protocol_and_features_are_rejected_before_smoke() {
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let cases: Vec<(&str, Mutation)> = vec![
        ("install.runtime_incompatible", |metadata| {
            metadata.package.runtime = "9.9.9".into();
        }),
        ("install.adk_incompatible", |metadata| {
            metadata.package.adk = "9.9.9".into();
        }),
        ("install.protocol_incompatible", |metadata| {
            metadata.package.protocol = "9.9".into();
            metadata.protocol_min = "9.9".into();
            metadata.protocol_max = "9.9".into();
        }),
        ("install.feature_unsupported", |metadata| {
            metadata.package.features.push("sandbox".into());
        }),
    ];
    for (expected, mutate) in cases {
        let artifact = root();
        let mut metadata = metadata(&["demo"], &["dev"], &binary, &source);
        mutate(&mut metadata);
        build(artifact.path(), &metadata, &binary, &source, &key());
        let cache = root();
        let error = Installer::new(cache.path(), policy())
            .install_blocking(artifact.path())
            .unwrap_err();
        assert_eq!(error.code, expected);
        assert!(!audit(cache.path()).contains("smoke.run"));
        cache_is_empty(cache.path());
    }
}

#[test]
fn tampered_source_bytes_are_rejected_before_smoke() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let metadata = metadata(&["demo"], &["dev"], &binary, &source);
    build(artifact.path(), &metadata, &binary, &source, &key());
    let artifact_digests = digests(artifact.path());
    std::fs::write(
        blob_path(artifact.path(), &artifact_digests.source_digest),
        b"{\"revision\":\"git:other\",\"tree\":\"sha256:0000000000000000000000000000000000000000000000000000000000000000\"}",
    )
    .unwrap();

    let cache = root();
    let error = Installer::new(cache.path(), policy())
        .install_blocking(artifact.path())
        .unwrap_err();

    assert_eq!(error.code, "install.layer_digest_mismatch");
    assert!(!audit(cache.path()).contains("smoke.run"));
    cache_is_empty(cache.path());
}

#[test]
fn installs_immutably_converges_and_preserves_other_releases_and_user_profiles() {
    let cache = root();
    std::fs::create_dir_all(cache.path().join("profiles")).unwrap();
    let profile = cache.path().join("profiles/default.json");
    std::fs::write(&profile, b"{\"user\":\"owned\"}").unwrap();
    let installer = Installer::new(cache.path(), policy());

    let first_artifact = root();
    let first_binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let first_source = source_bytes();
    let first = metadata(&["demo"], &["dev"], &first_binary, &first_source);
    build(
        first_artifact.path(),
        &first,
        &first_binary,
        &first_source,
        &key(),
    );
    let first_outcome = installer.install_blocking(first_artifact.path()).unwrap();
    assert!(first_outcome.installed_now());
    let first_release = first_outcome.release().clone();
    assert!(first_release.binary.exists());
    assert_eq!(first_release.registered, vec!["demo".to_string()]);

    let second_artifact = root();
    let second_binary = probe_binary(&["demo", "other"], &["dev"], "demo", "0.2.0");
    let second_source = source_bytes();
    let mut second = metadata(&["demo", "other"], &["dev"], &second_binary, &second_source);
    second.package.version = "0.2.0".into();
    build(
        second_artifact.path(),
        &second,
        &second_binary,
        &second_source,
        &key(),
    );
    let second_outcome = installer.install_blocking(second_artifact.path()).unwrap();
    assert!(second_outcome.installed_now());
    assert_ne!(
        second_outcome.release().artifact_digest,
        first_release.artifact_digest
    );

    let repeat = installer.install_blocking(first_artifact.path()).unwrap();
    assert!(matches!(repeat, InstallOutcome::Converged(_)));
    assert_eq!(
        repeat.release().artifact_digest,
        first_release.artifact_digest
    );

    let installed = installer.installed().unwrap();
    assert_eq!(installed.len(), 2);
    assert_eq!(
        std::fs::read_to_string(&profile).unwrap(),
        "{\"user\":\"owned\"}"
    );
}

#[test]
fn interrupted_installation_converges_after_recovery() {
    let cache = root();
    std::fs::create_dir_all(cache.path().join("staging/interrupted")).unwrap();
    std::fs::write(
        cache.path().join("staging/interrupted/partial"),
        b"partial bytes",
    )
    .unwrap();
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let metadata = metadata(&["demo"], &["dev"], &binary, &source);
    build(artifact.path(), &metadata, &binary, &source, &key());
    let installer = Installer::new(cache.path(), policy());

    let outcome = installer.install_blocking(artifact.path()).unwrap();

    assert!(outcome.installed_now());
    let staging = cache.path().join("staging");
    let leftovers = std::fs::read_dir(&staging)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path() != cache.path().join("staging/interrupted"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(leftovers, 0);
    let repeat = installer.install_blocking(artifact.path()).unwrap();
    assert!(matches!(repeat, InstallOutcome::Converged(_)));
}

#[test]
fn smoke_keeps_the_fixed_address_space_ceiling_with_bounded_allocator_arenas() {
    let artifact = root();
    let binary = boundary_probe_binary();
    let source = source_bytes();
    let metadata = metadata(&["demo"], &["dev"], &binary, &source);
    build(artifact.path(), &metadata, &binary, &source, &key());

    // The probe and launcher fail inside the boundary if the address-space
    // ceiling is raised, or if allocator arenas are left unconstrained.
    let outcome = Installer::new(root().path(), policy())
        .install_blocking(artifact.path())
        .expect("bounded smoke passes at the fixed 512 MiB ceiling");
    assert!(matches!(outcome, InstallOutcome::Installed(_)));
    assert_eq!(outcome.release().registered, vec!["demo".to_string()]);
    assert_eq!(outcome.release().launcher_modes, vec!["dev".to_string()]);
}

#[test]
fn seeds_are_verified_materialized_and_never_change_capabilities() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let seeds = vec![
        SeedFile::new("assets/prompts/system.md", b"seed prompt"),
        SeedFile::new(
            "assets/agents/demo.toml",
            b"name = \"demo\"\nmodel = \"deterministic\"\nprompt = \"system\"\n",
        ),
    ];
    let mut metadata = metadata(&["demo"], &["dev"], &binary, &source);
    metadata.seed_assets = install::seed_assets(&seeds);
    let signature = sign(&metadata, &key());
    write_layout_with_seeds(
        artifact.path(),
        &metadata,
        &binary,
        &source,
        &seeds,
        &signature,
    )
    .unwrap();

    let cache = root();
    let outcome = Installer::new(cache.path(), policy())
        .install_blocking(artifact.path())
        .unwrap();

    assert_eq!(outcome.release().registered, vec!["demo".to_string()]);
    assert_eq!(outcome.release().seeds.len(), 2);
    for seed in &seeds {
        let path = outcome.release().directory.join("seeds").join(&seed.path);
        assert_eq!(std::fs::read(&path).unwrap(), seed.bytes, "{}", seed.path);
    }
    // Seed assets are instance inputs, not compiled capabilities.
    assert_eq!(outcome.release().registered, vec!["demo".to_string()]);
}

#[test]
fn tampered_seed_catalog_is_rejected_before_acceptance() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let seeds = vec![SeedFile::new("assets/prompts/system.md", b"seed prompt")];
    let mut metadata = metadata(&["demo"], &["dev"], &binary, &source);
    metadata.seed_assets = install::seed_assets(&seeds);
    let signature = sign(&metadata, &key());
    let digests = write_layout_with_seeds(
        artifact.path(),
        &metadata,
        &binary,
        &source,
        &seeds,
        &signature,
    )
    .unwrap();
    let seed_digest = digests.seed_digest.expect("seed catalog digest");
    std::fs::write(blob_path(artifact.path(), &seed_digest), b"tampered").unwrap();

    let cache = root();
    let error = Installer::new(cache.path(), policy())
        .install_blocking(artifact.path())
        .expect_err("tampered seed catalog must be rejected");
    assert_eq!(error.code, "install.layer_digest_mismatch");
    cache_is_empty(&cache.path().join("cache"));
}

#[test]
fn declared_seeds_without_a_catalog_are_rejected_and_seed_paths_cannot_escape() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let mut declared = metadata(&["demo"], &["dev"], &binary, &source);
    declared.seed_assets = vec![SeedAsset {
        path: "prompts/system.md".into(),
        digest: bytes_digest(b"seed prompt"),
    }];
    let signature = sign(&declared, &key());
    let error = write_layout(artifact.path(), &declared, &binary, &source, &signature)
        .expect_err("metadata-only seeds must be rejected");
    assert_eq!(error.code, "install.seed_missing");

    let escaping = root();
    let mut escaping_metadata = metadata(&["demo"], &["dev"], &binary, &source);
    let seeds = vec![SeedFile::new("../escape.md", b"escape")];
    escaping_metadata.seed_assets = install::seed_assets(&seeds);
    let signature = sign(&escaping_metadata, &key());
    let error = write_layout_with_seeds(
        escaping.path(),
        &escaping_metadata,
        &binary,
        &source,
        &seeds,
        &signature,
    )
    .expect_err("escaping seed path must be rejected");
    assert_eq!(error.code, "install.seed_invalid");
}

#[test]
fn cli_inspect_and_install_use_the_same_verification_path() {
    let artifact = root();
    let binary = probe_binary(&["demo"], &["dev"], "demo", "0.1.0");
    let source = source_bytes();
    let metadata = metadata(&["demo"], &["dev"], &binary, &source);
    build(artifact.path(), &metadata, &binary, &source, &key());
    let cache = root();
    let trust = cache.path().join("trust.json");
    std::fs::write(&trust, serde_json::to_vec(&policy()).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&trust, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let inspect = std::process::Command::new(env!("CARGO_BIN_EXE_gb-install"))
        .args([
            "inspect",
            "--artifact",
            artifact.path().to_str().unwrap(),
            "--trust",
            trust.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        inspect.status.success(),
        "{}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(report["package"], "demo");
    assert_eq!(report["signer"], "publisher");

    let root_arg = cache.path().join("cache");
    let install = std::process::Command::new(env!("CARGO_BIN_EXE_gb-install"))
        .args([
            "install",
            "--artifact",
            artifact.path().to_str().unwrap(),
            "--trust",
            trust.to_str().unwrap(),
            "--root",
            root_arg.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&install.stdout).unwrap();
    assert_eq!(report["outcome"], "installed");
    assert_eq!(report["registered"][0], "demo");

    let rejected = std::process::Command::new(env!("CARGO_BIN_EXE_gb-install"))
        .args([
            "install",
            "--artifact",
            artifact.path().to_str().unwrap(),
            "--root",
            root_arg.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("install.usage"));
}

trait BlockingInstall {
    fn install_blocking(&self, artifact: &Path) -> Result<InstallOutcome, install::InstallError>;
}

impl BlockingInstall for Installer {
    fn install_blocking(&self, artifact: &Path) -> Result<InstallOutcome, install::InstallError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(self.install(artifact))
    }
}
