//! Signed advisory, signer trust, reactor freshness, replay, rotation, and
//! offline-policy behavior at the installer seam.
//!
//! These tests exercise the public `TrustPolicy` and `Installer` interfaces.
//! A release is only accepted after provenance, trusted signer tier, verified
//! advisory generation, freshness, and replay checks. Candidate binaries are
//! never executed for inspection and never executed at all when trust rejects
//! the release.

use base64::Engine;
use common::{bytes_digest, BuildEvidence, CapabilityDescriptor, PackageManifest, ReleaseMetadata};
use ed25519_dalek::{Signer, SigningKey};
use install::{
    encode_metadata, write_layout, AdvisoryEnvelope, AdvisoryKey, AdvisoryPayload, InstalledAction,
    Installer, OfflinePolicy, Revocation, RotationPolicy, TrustPolicy,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn public_key(signing: &SigningKey) -> String {
    B64.encode(signing.verifying_key().as_bytes())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn source() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "revision": "git:trust-fixture",
        "tree": bytes_digest(b"trust fixture tree"),
    }))
    .unwrap()
}

fn probe_binary(version: &str, sentinel: &Path) -> Vec<u8> {
    format!(
        "#!/bin/sh\n\
         (echo executed > '{}') 2>/dev/null || true\n\
         case \"$1\" in\n\
         probe)\n\
           printf '%s' '{{\"package\":\"demo\",\"version\":\"{version}\",\"protocol\":\"1.0\",\"adk\":\"2.3.0\",\"registered\":[\"demo\"],\"hooks\":[],\"launcher_modes\":[\"dev\"],\"serve_ready\":true}}'\n\
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
         esac\n",
        sentinel.display()
    )
    .into_bytes()
}

fn metadata(version: &str, binary: &[u8], source: &[u8]) -> ReleaseMetadata {
    ReleaseMetadata {
        schema: common::RELEASE_METADATA_SCHEMA,
        package: PackageManifest {
            name: "demo".into(),
            version: version.into(),
            protocol: "1.0".into(),
            runtime: "0.1.0".into(),
            adk: "2.3.0".into(),
            capabilities: vec![CapabilityDescriptor {
                id: "demo".into(),
                kind: "agent".into(),
                description: "fixture capability".into(),
                permissions: vec!["task:submit".into()],
            }],
            required_permissions: vec!["task:submit".into()],
            features: vec!["agents".into(), "runner".into(), "sessions".into()],
            launcher_modes: vec!["dev".into()],
            hooks: Vec::new(),
        },
        source_digest: bytes_digest(source),
        binary_digest: bytes_digest(binary),
        source_revision: "git:trust-fixture".into(),
        target: TrustPolicy::host_target(),
        protocol_min: "1.0".into(),
        protocol_max: "1.0".into(),
        signer: "publisher".into(),
        build: BuildEvidence {
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

fn policy(signing: &SigningKey) -> TrustPolicy {
    TrustPolicy {
        trusted_signers: BTreeMap::from([("publisher".into(), public_key(signing))]),
        publisher_tiers: BTreeMap::from([("publisher".into(), "verified".into())]),
        targets: vec![TrustPolicy::host_target()],
        runtimes: vec!["0.1.0".into()],
        adks: vec!["2.3.0".into()],
        protocols: vec!["1.0".into()],
        features: vec!["agents".into(), "runner".into(), "sessions".into()],
        approved_permissions: vec!["task:submit".into()],
        sensitive_permissions: vec!["credential:read".into(), "exec:host".into()],
        advisory_keys: Vec::new(),
        advisory_freshness_secs: 3_600,
        require_advisory: true,
        allowed_tiers: vec!["official".into(), "verified".into()],
        rotation: RotationPolicy::default(),
        offline: OfflinePolicy::default(),
    }
}

fn signed_advisory(payload: &AdvisoryPayload, authority: &str, signing: &SigningKey) -> Vec<u8> {
    let payload_bytes = serde_json::to_vec(payload).unwrap();
    let digest = bytes_digest(&payload_bytes);
    let signature = B64.encode(signing.sign(digest.as_bytes()).to_bytes());
    serde_json::to_vec(&AdvisoryEnvelope {
        schema: 1,
        authority: authority.into(),
        payload: B64.encode(payload_bytes),
        signature,
    })
    .unwrap()
}

fn advisory(generation: u64, issued_at: i64, revocations: Vec<Revocation>) -> AdvisoryPayload {
    AdvisoryPayload {
        schema: 1,
        generation,
        issued_at,
        expires_at: None,
        revocations,
    }
}

fn release_revocation(digest: &str) -> Revocation {
    Revocation {
        kind: "release".into(),
        package: "demo".into(),
        version: String::new(),
        digest: digest.into(),
        signer: String::new(),
        reason: "revoked for testing".into(),
    }
}

fn signer_revocation() -> Revocation {
    Revocation {
        kind: "signer".into(),
        package: String::new(),
        version: String::new(),
        digest: String::new(),
        signer: "publisher".into(),
        reason: "compromised publisher key".into(),
    }
}

fn build(
    directory: &Path,
    metadata: &ReleaseMetadata,
    binary: &[u8],
    source: &[u8],
    signing: &SigningKey,
) {
    let digest = bytes_digest(&encode_metadata(metadata).unwrap());
    let signature = B64.encode(signing.sign(digest.as_bytes()).to_bytes());
    write_layout(directory, metadata, binary, source, &signature).unwrap();
}

fn digest_of(directory: &Path) -> String {
    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("index.json")).unwrap()).unwrap();
    index["manifests"][0]["digest"].as_str().unwrap().into()
}

fn fixture(version: &str) -> (tempfile::TempDir, Vec<u8>, String) {
    let directory = tempfile::tempdir().unwrap();
    let sentinel = directory.path().join("EXECUTED");
    let binary = probe_binary(version, &sentinel);
    let source = source();
    let metadata = metadata(version, &binary, &source);
    build(directory.path(), &metadata, &binary, &source, &key(7));
    let digest = digest_of(directory.path());
    (directory, binary, digest)
}

fn advisory_policy(signing: &SigningKey, advisory_signing: &SigningKey) -> TrustPolicy {
    let mut policy = policy(signing);
    policy.advisory_keys = vec![AdvisoryKey {
        id: "advisories".into(),
        key: public_key(advisory_signing),
        not_after: None,
    }];
    policy
}

fn never_executed(sentinel: &Path, cache: &Path) {
    assert!(
        !sentinel.exists(),
        "trust rejection must happen before the candidate binary executes"
    );
    assert!(
        !audit(cache).contains("smoke.run"),
        "trust rejection must happen before smoke execution starts"
    );
}

fn audit(root: &Path) -> String {
    std::fs::read_to_string(root.join("audit.jsonl")).unwrap_or_default()
}

#[test]
fn signed_advisory_revoking_a_digest_blocks_install_before_execution() {
    let (artifact, _, digest) = fixture("0.1.0");
    let sentinel = artifact.path().join("EXECUTED");
    let policy = advisory_policy(&key(7), &key(11));
    let payload = advisory(1, now_ms(), vec![release_revocation(&digest)]);
    let signed = signed_advisory(&payload, "advisories", &key(11));
    let cache = tempfile::tempdir().unwrap();

    let error = Installer::new(cache.path(), policy)
        .with_advisory_bytes(signed)
        .install_blocking(artifact.path())
        .unwrap_err();

    assert_eq!(error.code, "install.release_revoked");
    never_executed(&sentinel, cache.path());
    assert!(cache_is_empty(cache.path()));
}

#[test]
fn revoked_signer_is_rejected_even_with_a_valid_release_signature() {
    let (artifact, _, _) = fixture("0.1.0");
    let sentinel = artifact.path().join("EXECUTED");
    let policy = advisory_policy(&key(7), &key(11));
    let payload = advisory(1, now_ms(), vec![signer_revocation()]);
    let signed = signed_advisory(&payload, "advisories", &key(11));
    let cache = tempfile::tempdir().unwrap();

    let error = Installer::new(cache.path(), policy)
        .with_advisory_bytes(signed)
        .install_blocking(artifact.path())
        .unwrap_err();

    assert_eq!(error.code, "install.signer_revoked");
    never_executed(&sentinel, cache.path());
    cache_is_empty(cache.path());
}

#[test]
fn revoked_or_disallowed_publisher_tier_blocks_install() {
    let (artifact, _, _) = fixture("0.1.0");
    let sentinel = artifact.path().join("EXECUTED");
    let cache = tempfile::tempdir().unwrap();
    let mut revoked = policy(&key(7));
    revoked
        .publisher_tiers
        .insert("publisher".into(), "revoked".into());
    let error = Installer::new(cache.path(), revoked)
        .install_blocking(artifact.path())
        .unwrap_err();
    assert_eq!(error.code, "install.signer_tier_denied");
    never_executed(&sentinel, cache.path());

    let mut disallowed = policy(&key(7));
    disallowed
        .publisher_tiers
        .insert("publisher".into(), "community".into());
    let error = Installer::new(cache.path(), disallowed)
        .install_blocking(artifact.path())
        .unwrap_err();
    assert_eq!(error.code, "install.signer_tier_denied");
    never_executed(&sentinel, cache.path());
}

#[test]
fn tampered_or_stale_advisory_cannot_replace_the_accepted_generation() {
    let (artifact, _, _) = fixture("0.1.0");
    let policy = advisory_policy(&key(7), &key(11));
    let cache = tempfile::tempdir().unwrap();
    let accepted = advisory(4, now_ms(), Vec::new());
    let accepted_bytes = signed_advisory(&accepted, "advisories", &key(11));
    Installer::new(cache.path(), policy.clone())
        .with_advisory_bytes(accepted_bytes)
        .install_blocking(artifact.path())
        .unwrap();
    let state = Installer::new(cache.path(), policy.clone())
        .advisory_state()
        .unwrap()
        .unwrap();
    assert_eq!(state.generation, 4);

    let mut tampered = signed_advisory(&advisory(5, now_ms(), Vec::new()), "advisories", &key(11));
    let last = tampered.len() - 1;
    tampered[last] ^= 0xff;
    let error = Installer::new(cache.path(), policy.clone())
        .with_advisory_bytes(tampered)
        .install_blocking(artifact.path())
        .unwrap_err();
    assert_eq!(error.code, "install.advisory_invalid");

    let stale = advisory(5, now_ms() - 4_000_000, Vec::new());
    let error = Installer::new(cache.path(), policy.clone())
        .with_advisory_bytes(signed_advisory(&stale, "advisories", &key(11)))
        .install_blocking(artifact.path())
        .unwrap_err();
    assert_eq!(error.code, "install.advisory_stale");

    let unchanged = Installer::new(cache.path(), policy)
        .advisory_state()
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.generation, 4);
    assert_eq!(unchanged.payload_digest, state.payload_digest);
}

#[test]
fn replayed_advisory_generation_cannot_replace_newer_policy() {
    let (artifact, _, _) = fixture("0.1.0");
    let policy = advisory_policy(&key(7), &key(11));
    let cache = tempfile::tempdir().unwrap();
    let newer = advisory(7, now_ms(), Vec::new());
    Installer::new(cache.path(), policy.clone())
        .with_advisory_bytes(signed_advisory(&newer, "advisories", &key(11)))
        .install_blocking(artifact.path())
        .unwrap();

    let replay = advisory(6, now_ms(), Vec::new());
    let error = Installer::new(cache.path(), policy.clone())
        .with_advisory_bytes(signed_advisory(&replay, "advisories", &key(11)))
        .install_blocking(artifact.path())
        .unwrap_err();
    assert_eq!(error.code, "install.advisory_replay");

    let conflicting = advisory(7, now_ms(), Vec::new());
    let error = Installer::new(cache.path(), policy.clone())
        .with_advisory_bytes(signed_advisory(&conflicting, "advisories", &key(11)))
        .install_blocking(artifact.path())
        .unwrap_err();
    assert_eq!(error.code, "install.advisory_replay");

    let state = Installer::new(cache.path(), policy)
        .advisory_state()
        .unwrap()
        .unwrap();
    assert_eq!(state.generation, 7);
}

#[test]
fn advisory_authority_rotation_retires_the_old_key() {
    let (artifact, _, _) = fixture("0.1.0");
    let cache = tempfile::tempdir().unwrap();
    let old = key(11);
    let new = key(12);
    let payload = advisory(1, now_ms(), Vec::new());
    let old_bytes = signed_advisory(&payload, "advisories", &old);
    let new_bytes = signed_advisory(&payload, "advisories", &new);

    let mut retired = advisory_policy(&key(7), &old);
    retired.advisory_keys[0].not_after = Some(now_ms() - 1_000);
    let error = Installer::new(cache.path(), retired)
        .with_advisory_bytes(old_bytes)
        .install_blocking(artifact.path())
        .unwrap_err();
    assert_eq!(error.code, "install.advisory_key_retired");

    let mut rotated = advisory_policy(&key(7), &new);
    rotated.advisory_keys[0].not_after = None;
    Installer::new(cache.path(), rotated)
        .with_advisory_bytes(new_bytes)
        .install_blocking(artifact.path())
        .unwrap();
}

#[test]
fn required_advisory_missing_blocks_a_new_install() {
    let (artifact, _, _) = fixture("0.1.0");
    let sentinel = artifact.path().join("EXECUTED");
    let cache = tempfile::tempdir().unwrap();
    let error = Installer::new(cache.path(), advisory_policy(&key(7), &key(11)))
        .install_blocking(artifact.path())
        .unwrap_err();
    assert_eq!(error.code, "install.advisory_required");
    never_executed(&sentinel, cache.path());
}

#[test]
fn policy_snapshot_records_roots_tiers_freshness_rotation_and_offline_policy() {
    let mut policy = advisory_policy(&key(7), &key(11));
    policy.offline = OfflinePolicy {
        installed_revoked: InstalledAction::Quarantine,
        installed_without_advisory: InstalledAction::Restrict,
    };
    policy.rotation = RotationPolicy {
        advisory_key_overlap_secs: 60,
    };
    let snapshot = policy.snapshot();
    assert_eq!(snapshot.publisher_tiers["publisher"], "verified");
    assert_eq!(snapshot.advisory_freshness_secs, 3_600);
    assert!(snapshot.require_advisory);
    assert_eq!(snapshot.rotation.advisory_key_overlap_secs, 60);
    assert_eq!(
        snapshot.offline.installed_revoked,
        InstalledAction::Quarantine
    );
    assert_eq!(
        snapshot.offline.installed_without_advisory,
        InstalledAction::Restrict
    );
    assert_eq!(snapshot.advisory_keys.len(), 1);
    assert!(snapshot.signer_roots[0]
        .public_key_fingerprint
        .starts_with("sha256:"));
}

#[test]
fn advisory_policy_without_recorded_freshness_is_invalid() {
    let mut policy = advisory_policy(&key(7), &key(11));
    policy.advisory_freshness_secs = 0;
    let error = policy.validate().unwrap_err();
    assert_eq!(error.code, "trust.policy_invalid");
}

#[test]
fn policy_file_must_not_be_group_or_world_writable() {
    let policy = advisory_policy(&key(7), &key(11));
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), serde_json::to_vec(&policy).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o666)).unwrap();
        let error = TrustPolicy::load(file.path()).unwrap_err();
        assert_eq!(error.code, "trust.policy_insecure");
    }
    #[cfg(not(unix))]
    {
        TrustPolicy::load(file.path()).unwrap();
    }
}

#[test]
fn advisory_revokes_only_the_exact_release_digest() {
    let (revoked_artifact, _, revoked_digest) = fixture("0.1.0");
    let (clean_artifact, _, _) = fixture("0.2.0");
    let policy = advisory_policy(&key(7), &key(11));
    let payload = advisory(2, now_ms(), vec![release_revocation(&revoked_digest)]);
    let signed = signed_advisory(&payload, "advisories", &key(11));
    let cache = tempfile::tempdir().unwrap();

    let error = Installer::new(cache.path(), policy.clone())
        .with_advisory_bytes(signed.clone())
        .install_blocking(revoked_artifact.path())
        .unwrap_err();
    assert_eq!(error.code, "install.release_revoked");

    Installer::new(cache.path(), policy)
        .with_advisory_bytes(signed)
        .install_blocking(clean_artifact.path())
        .unwrap();
}

#[test]
fn unavailable_registry_denies_new_install_but_does_not_stop_installed_execution() {
    let (artifact, _, digest) = fixture("0.1.0");
    let cache = tempfile::tempdir().unwrap();
    let policy = advisory_policy(&key(7), &key(11));
    let payload = advisory(1, now_ms(), Vec::new());
    let signed = signed_advisory(&payload, "advisories", &key(11));
    let installed = Installer::new(cache.path(), policy.clone())
        .with_advisory_bytes(signed)
        .install_blocking(artifact.path())
        .unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let client = install::RegistryClient::new(format!("http://{addr}"), "reader-token").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let installer = Installer::new(cache.path(), policy);
    let error = runtime
        .block_on(client.install("acme", "demo", &digest, &installer))
        .unwrap_err();
    assert_eq!(error.code, "install.registry_unavailable");
    assert!(
        error.message.contains("registry server is not available"),
        "registry outage must be reported without breaking installed execution: {error}"
    );

    let output = std::process::Command::new(&installed.release().binary)
        .arg("probe")
        .output()
        .unwrap();
    assert!(output.status.success(), "installed release must still run");
}

#[test]
fn cli_trust_reports_recorded_policy_and_verified_advisory() {
    let directory = tempfile::tempdir().unwrap();
    let trust = directory.path().join("trust.json");
    let advisory_file = directory.path().join("advisory.json");
    let policy = advisory_policy(&key(7), &key(11));
    std::fs::write(&trust, serde_json::to_vec(&policy).unwrap()).unwrap();
    let payload = advisory(9, now_ms(), Vec::new());
    std::fs::write(
        &advisory_file,
        signed_advisory(&payload, "advisories", &key(11)),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&trust, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gb-install"))
        .args([
            "trust",
            "--trust",
            trust.to_str().unwrap(),
            "--advisories",
            advisory_file.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["policy"]["require_advisory"], true);
    assert_eq!(report["policy"]["publisher_tiers"]["publisher"], "verified");
    assert_eq!(report["policy"]["advisory_freshness_secs"], 3_600);
    assert_eq!(report["advisory"]["generation"], 9);
    assert_eq!(report["advisory"]["authority"], "advisories");
}

#[test]
fn cli_trust_rejects_a_tampered_advisory() {
    let directory = tempfile::tempdir().unwrap();
    let trust = directory.path().join("trust.json");
    let advisory_file = directory.path().join("advisory.json");
    std::fs::write(
        &trust,
        serde_json::to_vec(&advisory_policy(&key(7), &key(11))).unwrap(),
    )
    .unwrap();
    let mut tampered = signed_advisory(&advisory(1, now_ms(), Vec::new()), "advisories", &key(11));
    let last = tampered.len() - 1;
    tampered[last] ^= 0xff;
    std::fs::write(&advisory_file, tampered).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&trust, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gb-install"))
        .args([
            "trust",
            "--trust",
            trust.to_str().unwrap(),
            "--advisories",
            advisory_file.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("install.advisory_invalid"), "{stderr}");
}

fn cache_is_empty(root: &Path) -> bool {
    let packages = root.join("packages").join("sha256");
    let entries = std::fs::read_dir(&packages)
        .map(|entries| entries.count())
        .unwrap_or(0);
    entries == 0
}

trait BlockingInstall {
    fn install_blocking(
        &self,
        artifact: &Path,
    ) -> Result<install::InstallOutcome, install::InstallError>;
}

impl BlockingInstall for Installer {
    fn install_blocking(
        &self,
        artifact: &Path,
    ) -> Result<install::InstallOutcome, install::InstallError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(self.install(artifact))
    }
}

#[test]
fn discovery_status_uses_verified_advisory_and_never_grants_installation() {
    let (_, _, digest) = fixture("0.1.0");
    let policy = advisory_policy(&key(7), &key(11));
    let payload = advisory(1, now_ms(), vec![release_revocation(&digest)]);
    let signed = signed_advisory(&payload, "advisories", &key(11));
    let verified = policy
        .verify_advisory(&signed, None, now_ms())
        .expect("advisory verifies");

    let revoked = policy.discovery_status("demo", "0.1.0", &digest, "publisher", Some(&verified));
    assert_eq!(revoked.status, "revoked");
    assert!(revoked.reason.contains("revoked for testing"));

    let untrusted = policy.discovery_status(
        "demo",
        "0.1.0",
        &digest,
        "unknown-publisher",
        Some(&verified),
    );
    assert_eq!(untrusted.status, "untrusted");

    let missing_evidence = policy.discovery_status("demo", "0.1.0", &digest, "publisher", None);
    assert_eq!(missing_evidence.status, "advisory_required");
}
