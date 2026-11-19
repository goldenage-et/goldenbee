//! End-to-end signed release acceptance for the real Coding binary.

use base64::Engine;
use common::{bytes_digest, ReleaseMetadata};
use ed25519_dalek::{Signer, SigningKey};
use install::{encode_metadata, write_layout, InstallOutcome, Installer, TrustPolicy};
use std::collections::BTreeMap;

fn signer() -> SigningKey {
    SigningKey::from_bytes(&[7_u8; 32])
}

fn public_key() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signer().verifying_key().as_bytes())
}

fn metadata(binary: &[u8], source: &[u8]) -> ReleaseMetadata {
    ReleaseMetadata {
        schema: common::RELEASE_METADATA_SCHEMA,
        package: coding::manifest(),
        source_digest: bytes_digest(source),
        binary_digest: bytes_digest(binary),
        target: TrustPolicy::host_target(),
        source_revision: String::new(),
        protocol_min: String::new(),
        protocol_max: String::new(),
        signer: "coding-publisher".into(),
        build: Default::default(),
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
        trusted_signers: BTreeMap::from([("coding-publisher".into(), public_key())]),
        targets: vec![TrustPolicy::host_target()],
        runtimes: vec![manifest.runtime.clone()],
        adks: vec![manifest.adk.clone()],
        protocols: vec![manifest.protocol.clone()],
        features: manifest.features.clone(),
        approved_permissions: permissions,
        sensitive_permissions: vec!["exec:command".into(), "workspace:write".into()],
        ..TrustPolicy::default()
    }
}

#[tokio::test]
async fn installs_the_real_coding_package_and_accepts_its_manifest() {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_coding")).unwrap();
    let source = serde_json::to_vec(&serde_json::json!({
        "revision": "git:coding-fixture",
        "tree": bytes_digest(b"coding source tree"),
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
    assert_eq!(inspection.metadata.package.name, "coding");
    assert!(inspection
        .metadata
        .package
        .features
        .contains(&"coding".to_string()));
    assert_eq!(inspection.metadata.binary_digest, bytes_digest(&binary));

    let outcome = installer.install(artifact.path()).await.unwrap();
    assert!(matches!(outcome, InstallOutcome::Installed(_)));
    let release = outcome.release();
    assert_eq!(release.package, "coding");
    assert_eq!(release.version, package.version);
    assert_eq!(
        release.registered,
        vec![
            "developer".to_string(),
            "exec_command".to_string(),
            "reviewer".to_string()
        ]
    );
    assert_eq!(
        release.launcher_modes,
        vec!["dev".to_string(), "serve".to_string()]
    );
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
