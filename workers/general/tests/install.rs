//! End-to-end installation of the real General Worker binary.
//!
//! The release is signed with ed25519 and inspected, smoke-tested, and cached
//! by the on-host installer. Both declared launcher modes are exercised by the
//! constrained smoke run.

use base64::Engine;
use common::{bytes_digest, ReleaseMetadata};
use ed25519_dalek::{Signer, SigningKey};
use install::{encode_metadata, write_layout, InstallOutcome, Installer, TrustPolicy};
use std::collections::BTreeMap;

fn signer() -> SigningKey {
    SigningKey::from_bytes(&[5_u8; 32])
}

fn public_key() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signer().verifying_key().as_bytes())
}

fn metadata(binary: &[u8], source: &[u8]) -> ReleaseMetadata {
    ReleaseMetadata {
        schema: common::RELEASE_METADATA_SCHEMA,
        package: general::manifest(),
        source_digest: bytes_digest(source),
        binary_digest: bytes_digest(binary),
        source_revision: "git:general-fixture".into(),
        target: TrustPolicy::host_target(),
        protocol_min: common::PROTOCOL_VERSION.into(),
        protocol_max: common::PROTOCOL_VERSION.into(),
        signer: "general-publisher".into(),
        build: common::BuildEvidence {
            toolchain: "rust 1.83".into(),
            profile: "release".into(),
            command: "gbb check -p general".into(),
            started_at: "2026-11-18T00:00:00Z".into(),
            finished_at: "2026-11-18T00:01:00Z".into(),
            reproducible: true,
            evidence_digest: bytes_digest(b"general build evidence"),
        },
        seed_assets: Vec::new(),
    }
}

fn policy(manifest: &common::PackageManifest) -> TrustPolicy {
    let mut permissions = manifest.required_permissions.clone();
    for capability in &manifest.capabilities {
        permissions.extend(capability.permissions.clone());
    }
    permissions.sort();
    permissions.dedup();
    TrustPolicy {
        trusted_signers: BTreeMap::from([("general-publisher".into(), public_key())]),
        targets: vec![TrustPolicy::host_target()],
        runtimes: vec![manifest.runtime.clone()],
        adks: vec![manifest.adk.clone()],
        protocols: vec![manifest.protocol.clone()],
        features: manifest.features.clone(),
        approved_permissions: permissions,
        sensitive_permissions: vec!["credential:read".into(), "exec:host".into()],
        ..TrustPolicy::default()
    }
}

#[tokio::test]
async fn installs_the_real_general_package_and_validates_launcher_modes() {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_general")).unwrap();
    let source = serde_json::to_vec(&serde_json::json!({
        "revision": "git:general-fixture",
        "tree": bytes_digest(b"general source tree"),
    }))
    .unwrap();
    let metadata = metadata(&binary, &source);
    let package = metadata.package.clone();
    let artifact = tempfile::tempdir().unwrap();
    let digest = bytes_digest(&encode_metadata(&metadata).unwrap());
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(signer().sign(digest.as_bytes()).to_bytes());
    write_layout(artifact.path(), &metadata, &binary, &source, &signature).unwrap();

    let cache = tempfile::tempdir().unwrap();
    let installer = Installer::new(cache.path(), policy(&package));
    let inspection = installer.inspect(artifact.path()).unwrap();
    assert_eq!(inspection.metadata.package.name, "general");
    assert_eq!(inspection.metadata.binary_digest, bytes_digest(&binary));

    let outcome = installer.install(artifact.path()).await.unwrap();
    assert!(matches!(outcome, InstallOutcome::Installed(_)));
    let release = outcome.release();
    assert_eq!(release.package, "general");
    assert_eq!(release.version, package.version);
    assert_eq!(
        release.registered,
        vec![
            "browser".to_string(),
            "exec_command".to_string(),
            "manual".to_string(),
            "observe_knowledge".to_string(),
            "recall_knowledge".to_string(),
            "save_artifact".to_string()
        ]
    );
    assert_eq!(
        release.launcher_modes,
        vec!["dev".to_string(), "serve".to_string()]
    );
    assert!(release.binary.exists());
    assert_eq!(
        common::bytes_digest(&std::fs::read(&release.binary).unwrap()),
        metadata.binary_digest
    );

    let output = std::process::Command::new(&release.binary)
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());

    let converged = installer.install(artifact.path()).await.unwrap();
    assert!(matches!(converged, InstallOutcome::Converged(_)));
}
