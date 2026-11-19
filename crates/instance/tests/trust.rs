//! Instance-scoped trust outcomes: explicit continue, restrict, and quarantine
//! for already-installed releases, with no registry dependency.

use base64::Engine;
use common::{bytes_digest, BuildEvidence, CapabilityDescriptor, PackageManifest, ReleaseMetadata};
use ed25519_dalek::{Signer, SigningKey};
use install::{
    encode_metadata, write_layout, AdvisoryEnvelope, AdvisoryKey, AdvisoryPayload, InstalledAction,
    Installer, OfflinePolicy, Revocation, TrustPolicy,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
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
        "revision": "git:instance-trust-fixture",
        "tree": bytes_digest(b"instance trust tree"),
    }))
    .unwrap()
}

fn binary() -> Vec<u8> {
    "#!/bin/sh\n\
     (echo executed > \"$PWD/EXECUTED\") 2>/dev/null || true\n\
     case \"$1\" in\n\
     probe)\n\
       printf '%s' '{\"package\":\"demo\",\"version\":\"0.1.0\",\"protocol\":\"1.0\",\"adk\":\"2.3.0\",\"registered\":[\"demo\"],\"hooks\":[],\"launcher_modes\":[\"dev\"],\"serve_ready\":true}'\n\
       ;;\n\
     dev|console)\n\
       printf 'launcher\\n'\n\
       ;;\n\
     serve)\n\
       printf 'serve composition ready\\n'\n\
       exit 1\n\
       ;;\n\
     *)\n\
       exit 3\n\
       ;;\n\
     esac\n"
        .as_bytes()
        .to_vec()
}

fn metadata(binary: &[u8], source: &[u8]) -> ReleaseMetadata {
    ReleaseMetadata {
        schema: common::RELEASE_METADATA_SCHEMA,
        package: PackageManifest {
            name: "demo".into(),
            version: "0.1.0".into(),
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
        source_revision: "git:instance-trust-fixture".into(),
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

fn base_policy(require_advisory: bool) -> TrustPolicy {
    TrustPolicy {
        trusted_signers: BTreeMap::from([("publisher".into(), public_key(&key(7)))]),
        publisher_tiers: BTreeMap::from([("publisher".into(), "verified".into())]),
        targets: vec![TrustPolicy::host_target()],
        runtimes: vec!["0.1.0".into()],
        adks: vec!["2.3.0".into()],
        protocols: vec!["1.0".into()],
        features: vec!["agents".into(), "runner".into(), "sessions".into()],
        approved_permissions: vec!["task:submit".into()],
        sensitive_permissions: Vec::new(),
        advisory_keys: vec![AdvisoryKey {
            id: "advisories".into(),
            key: public_key(&key(11)),
            not_after: None,
        }],
        advisory_freshness_secs: 3_600,
        require_advisory,
        allowed_tiers: vec!["official".into(), "verified".into()],
        rotation: Default::default(),
        offline: OfflinePolicy::default(),
    }
}

fn signed_advisory(payload: &AdvisoryPayload) -> Vec<u8> {
    let payload_bytes = serde_json::to_vec(payload).unwrap();
    let digest = bytes_digest(&payload_bytes);
    let signature = B64.encode(key(11).sign(digest.as_bytes()).to_bytes());
    serde_json::to_vec(&AdvisoryEnvelope {
        schema: 1,
        authority: "advisories".into(),
        payload: B64.encode(payload_bytes),
        signature,
    })
    .unwrap()
}

fn revocation(digest: &str) -> Revocation {
    Revocation {
        kind: "release".into(),
        package: "demo".into(),
        version: String::new(),
        digest: digest.into(),
        signer: String::new(),
        reason: "revoked for testing".into(),
    }
}

struct Fixture {
    cache: tempfile::TempDir,
    instances: tempfile::TempDir,
    digest: String,
}

fn installed_fixture() -> Fixture {
    let cache = tempfile::tempdir().unwrap();
    let artifact = tempfile::tempdir().unwrap();
    let binary = binary();
    let source = source();
    let metadata = metadata(&binary, &source);
    let digest = bytes_digest(&encode_metadata(&metadata).unwrap());
    let signature = B64.encode(key(7).sign(digest.as_bytes()).to_bytes());
    write_layout(artifact.path(), &metadata, &binary, &source, &signature).unwrap();
    let installer = Installer::new(cache.path(), base_policy(false));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(installer.install(artifact.path()))
        .unwrap();
    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(artifact.path().join("index.json")).unwrap())
            .unwrap();
    let artifact_digest = index["manifests"][0]["digest"]
        .as_str()
        .unwrap()
        .to_string();
    Fixture {
        cache,
        instances: tempfile::tempdir().unwrap(),
        digest: artifact_digest,
    }
}

fn create_instance(fixture: &Fixture, id: &str) {
    let release = release_dir(fixture, &fixture.digest);
    let mut request = instance::CreateRequest::new(fixture.instances.path(), id, release);
    request.grants = vec!["task:submit".into(), "task:read".into()];
    instance::create(request).unwrap();
}

fn sentinel(fixture: &Fixture, id: &str) -> PathBuf {
    fixture.instances.path().join(id).join("EXECUTED")
}

fn release_dir(fixture: &Fixture, digest: &str) -> PathBuf {
    fixture
        .cache
        .path()
        .join("packages")
        .join("sha256")
        .join(digest.trim_start_matches("sha256:"))
}

fn policy_with(action: InstalledAction) -> TrustPolicy {
    let mut policy = base_policy(true);
    policy.offline = OfflinePolicy {
        installed_revoked: action,
        installed_without_advisory: action,
    };
    policy
}

#[test]
fn revoked_release_quarantine_blocks_start_and_cannot_be_cleared_by_start_or_inspect() {
    let fixture = installed_fixture();
    create_instance(&fixture, "alpha");
    let payload = AdvisoryPayload {
        schema: 1,
        generation: 1,
        issued_at: now_ms(),
        expires_at: None,
        revocations: vec![revocation(&fixture.digest)],
    };
    let signed = signed_advisory(&payload);

    let outcome = instance::apply_trust(
        fixture.instances.path(),
        "alpha",
        &policy_with(InstalledAction::Quarantine),
        Some(&signed),
    )
    .unwrap();
    assert_eq!(outcome.decision, InstalledAction::Quarantine);

    let error = instance::start(fixture.instances.path(), "alpha", None).unwrap_err();
    assert_eq!(error.code, "instance.quarantined");
    assert!(
        !sentinel(&fixture, "alpha").exists(),
        "quarantine must block execution"
    );

    let inspection = instance::inspect(fixture.instances.path(), "alpha").unwrap();
    let trust = inspection.trust.expect("trust status");
    assert_eq!(trust.decision, InstalledAction::Quarantine);
    assert!(trust.reason.contains("revoked"));

    let error = instance::profile(fixture.instances.path(), "alpha", &["list".into()]).unwrap_err();
    assert_eq!(error.code, "instance.quarantined");
    assert!(!sentinel(&fixture, "alpha").exists());

    let still = instance::inspect(fixture.instances.path(), "alpha").unwrap();
    assert_eq!(still.trust.unwrap().decision, InstalledAction::Quarantine);
}

#[test]
fn continue_keeps_installed_execution_and_restrict_blocks_new_starts() {
    let fixture = installed_fixture();
    create_instance(&fixture, "alpha");
    let payload = AdvisoryPayload {
        schema: 1,
        generation: 1,
        issued_at: now_ms(),
        expires_at: None,
        revocations: vec![revocation(&fixture.digest)],
    };
    let signed = signed_advisory(&payload);

    let outcome = instance::apply_trust(
        fixture.instances.path(),
        "alpha",
        &policy_with(InstalledAction::Continue),
        Some(&signed),
    )
    .unwrap();
    assert_eq!(outcome.decision, InstalledAction::Continue);
    let _ = instance::start(fixture.instances.path(), "alpha", None);
    assert!(
        sentinel(&fixture, "alpha").exists(),
        "continue policy must leave installed execution available"
    );
    let _ = std::fs::remove_file(sentinel(&fixture, "alpha"));

    let fixture = installed_fixture();
    create_instance(&fixture, "beta");
    let payload = AdvisoryPayload {
        schema: 1,
        generation: 1,
        issued_at: now_ms(),
        expires_at: None,
        revocations: vec![revocation(&fixture.digest)],
    };
    let signed = signed_advisory(&payload);
    instance::apply_trust(
        fixture.instances.path(),
        "beta",
        &policy_with(InstalledAction::Restrict),
        Some(&signed),
    )
    .unwrap();
    let error = instance::start(fixture.instances.path(), "beta", None).unwrap_err();
    assert_eq!(error.code, "instance.trust_restricted");
    assert!(!sentinel(&fixture, "alpha").exists());
}

#[test]
fn quarantine_requires_an_explicit_operator_decision_with_fresh_advisory() {
    let fixture = installed_fixture();
    create_instance(&fixture, "alpha");
    let revoked = AdvisoryPayload {
        schema: 1,
        generation: 3,
        issued_at: now_ms(),
        expires_at: None,
        revocations: vec![revocation(&fixture.digest)],
    };
    instance::apply_trust(
        fixture.instances.path(),
        "alpha",
        &policy_with(InstalledAction::Quarantine),
        Some(&signed_advisory(&revoked)),
    )
    .unwrap();
    assert_eq!(
        instance::inspect(fixture.instances.path(), "alpha")
            .unwrap()
            .trust
            .unwrap()
            .decision,
        InstalledAction::Quarantine
    );

    // Reapplying the same trusted advisory under the same quarantine policy
    // keeps quarantine. It is not a new operator decision.
    let outcome = instance::apply_trust(
        fixture.instances.path(),
        "alpha",
        &policy_with(InstalledAction::Quarantine),
        Some(&signed_advisory(&revoked)),
    )
    .unwrap();
    assert_eq!(outcome.decision, InstalledAction::Quarantine);
    assert_eq!(
        instance::inspect(fixture.instances.path(), "alpha")
            .unwrap()
            .trust
            .unwrap()
            .decision,
        InstalledAction::Quarantine
    );

    let clear = AdvisoryPayload {
        schema: 1,
        generation: 4,
        issued_at: now_ms(),
        expires_at: None,
        revocations: Vec::new(),
    };
    let outcome = instance::apply_trust(
        fixture.instances.path(),
        "alpha",
        &policy_with(InstalledAction::Continue),
        Some(&signed_advisory(&clear)),
    )
    .unwrap();
    assert_eq!(outcome.decision, InstalledAction::Continue);
    let _ = instance::start(fixture.instances.path(), "alpha", None);
    assert!(sentinel(&fixture, "alpha").exists());
}

#[test]
fn tampered_advisory_cannot_change_an_instance_decision() {
    let fixture = installed_fixture();
    create_instance(&fixture, "alpha");
    let payload = AdvisoryPayload {
        schema: 1,
        generation: 1,
        issued_at: now_ms(),
        expires_at: None,
        revocations: vec![revocation(&fixture.digest)],
    };
    let mut tampered = signed_advisory(&payload);
    let last = tampered.len() - 1;
    tampered[last] ^= 0xff;

    let error = instance::apply_trust(
        fixture.instances.path(),
        "alpha",
        &policy_with(InstalledAction::Quarantine),
        Some(&tampered),
    )
    .unwrap_err();
    assert_eq!(error.code, "install.advisory_invalid");
    let inspection = instance::inspect(fixture.instances.path(), "alpha").unwrap();
    assert!(inspection.trust.is_none());
}

#[test]
fn missing_required_advisory_uses_the_recorded_offline_action() {
    let fixture = installed_fixture();
    create_instance(&fixture, "alpha");
    let mut policy = policy_with(InstalledAction::Restrict);
    policy.offline.installed_without_advisory = InstalledAction::Restrict;
    let outcome = instance::apply_trust(fixture.instances.path(), "alpha", &policy, None).unwrap();
    assert_eq!(outcome.decision, InstalledAction::Restrict);
    assert!(outcome.reason.contains("unavailable"));
    let error = instance::start(fixture.instances.path(), "alpha", None).unwrap_err();
    assert_eq!(error.code, "instance.trust_restricted");
    assert!(!sentinel(&fixture, "alpha").exists());
}

#[test]
fn cli_inspect_reports_the_instance_trust_decision_and_reason() {
    let fixture = installed_fixture();
    create_instance(&fixture, "alpha");
    let payload = AdvisoryPayload {
        schema: 1,
        generation: 2,
        issued_at: now_ms(),
        expires_at: None,
        revocations: vec![revocation(&fixture.digest)],
    };
    instance::apply_trust(
        fixture.instances.path(),
        "alpha",
        &policy_with(InstalledAction::Quarantine),
        Some(&signed_advisory(&payload)),
    )
    .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gb-instance"))
        .args([
            "inspect",
            "--root",
            fixture.instances.path().to_str().unwrap(),
            "--instance",
            "alpha",
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
    assert_eq!(report["trust"]["decision"], "quarantine");
    assert_eq!(report["trust"]["advisory_generation"], 2);
    assert!(report["trust"]["reason"]
        .as_str()
        .unwrap()
        .contains("revoked"));
    assert_eq!(report["signer"], "publisher");
}

#[test]
fn two_instances_take_independent_decisions_for_the_same_release() {
    let fixture = installed_fixture();
    create_instance(&fixture, "alpha");
    create_instance(&fixture, "beta");
    let payload = AdvisoryPayload {
        schema: 1,
        generation: 1,
        issued_at: now_ms(),
        expires_at: None,
        revocations: vec![revocation(&fixture.digest)],
    };
    let signed = signed_advisory(&payload);
    instance::apply_trust(
        fixture.instances.path(),
        "alpha",
        &policy_with(InstalledAction::Quarantine),
        Some(&signed),
    )
    .unwrap();
    instance::apply_trust(
        fixture.instances.path(),
        "beta",
        &policy_with(InstalledAction::Continue),
        Some(&signed),
    )
    .unwrap();

    assert_eq!(
        instance::start(fixture.instances.path(), "alpha", None)
            .unwrap_err()
            .code,
        "instance.quarantined"
    );
    assert!(!sentinel(&fixture, "alpha").exists());
    let _ = instance::start(fixture.instances.path(), "beta", None);
    assert!(sentinel(&fixture, "beta").exists());
}
