//! End-to-end installation of the real Knowledge Worker binary and an
//! installed-binary retrieval run against operator assets outside the install.

mod fixture;

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
        package: knowledge::manifest(),
        source_digest: bytes_digest(source),
        binary_digest: bytes_digest(binary),
        target: TrustPolicy::host_target(),
        source_revision: String::new(),
        protocol_min: String::new(),
        protocol_max: String::new(),
        signer: "knowledge-publisher".into(),
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
        trusted_signers: BTreeMap::from([("knowledge-publisher".into(), public_key())]),
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
async fn installs_the_real_knowledge_package_and_runs_an_installed_query() {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_knowledge")).unwrap();
    let source = serde_json::to_vec(&serde_json::json!({
        "revision": "git:knowledge-fixture",
        "tree": bytes_digest(b"knowledge source tree"),
    }))
    .unwrap();
    let metadata = metadata(&binary, &source);
    let package = metadata.package.clone();
    assert!(package.features.contains(&"rag".to_string()));
    let artifact = tempfile::tempdir().unwrap();
    let digest = bytes_digest(&encode_metadata(&metadata).unwrap());
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(signer().sign(digest.as_bytes()).to_bytes());
    write_layout(artifact.path(), &metadata, &binary, &source, &signature).unwrap();

    let cache = tempfile::tempdir().unwrap();
    let installer = Installer::new(cache.path(), policy(&package));
    let inspection = installer.inspect(artifact.path()).unwrap();
    assert_eq!(inspection.metadata.package.name, "knowledge");
    assert_eq!(inspection.metadata.binary_digest, bytes_digest(&binary));

    let outcome = installer.install(artifact.path()).await.unwrap();
    assert!(matches!(outcome, InstallOutcome::Installed(_)));
    let release = outcome.release();
    assert_eq!(release.package, "knowledge");
    assert_eq!(release.version, package.version);
    assert_eq!(
        release.registered,
        vec![
            "knowledge_ask".to_string(),
            "knowledge_ingest".to_string(),
            "knowledge_remove".to_string()
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

    let version = std::process::Command::new(&release.binary)
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());

    let converged = installer.install(artifact.path()).await.unwrap();
    assert!(matches!(converged, InstallOutcome::Converged(_)));

    // The installed executable loads operator configuration and assets from
    // outside the install cache and performs real retrieval.
    let config_root = tempfile::tempdir().unwrap();
    let fixture = fixture::write(config_root.path(), fixture::Options::default());
    let document = fixture.sources.join("installed.md");
    std::fs::write(
        &document,
        b"installed canary KNOWLEDGE-INSTALLED-31 from operator assets.\n",
    )
    .unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let ingest = fixture::run_binary(
        &release.binary,
        &fixture.config,
        elsewhere.path(),
        &format!("knowledge-ingest:{}", document.display()),
    );
    assert!(
        ingest.status.success(),
        "installed ingest failed: {}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    let asked = fixture::run_binary(
        &release.binary,
        &fixture.config,
        elsewhere.path(),
        "knowledge-ask:KNOWLEDGE-INSTALLED-31",
    );
    assert!(
        asked.status.success(),
        "installed ask failed: {}",
        String::from_utf8_lossy(&asked.stderr)
    );
    let payload = fixture::knowledge_payload(&asked);
    assert_eq!(payload["results"], 1, "{payload}");
    assert!(payload["citations"][0]["text"]
        .as_str()
        .unwrap()
        .contains("KNOWLEDGE-INSTALLED-31"));
}
