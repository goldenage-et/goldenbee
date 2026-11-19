//! Reproducible release assembly and verification.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn git(directory: &Path, args: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new("timeout")
        .args(["--kill-after=1s", "5s", "git", "-C"])
        .arg(directory)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn write_fixture(directory: &Path) -> std::path::PathBuf {
    let binary = directory.join("fixture-package");
    std::fs::write(
        &binary,
        "#!/bin/sh\n\
         if [ \"$1\" = probe ]; then\n\
           printf '%s' '{\"package\":\"fixture\",\"version\":\"1.2.3\",\"runtime\":\"0.1.0\",\"protocol\":\"1.0\",\"adk\":\"2.3.0\",\"registered\":[\"fixture-agent\",\"fixture-tool\"],\"capabilities\":[{\"id\":\"fixture-agent\",\"kind\":\"agent\",\"description\":\"fixture\",\"permissions\":[]},{\"id\":\"fixture-tool\",\"kind\":\"tool\",\"description\":\"fixture\",\"permissions\":[]}],\"features\":[\"agents\",\"runner\",\"sessions\",\"server\",\"cli\"],\"required_permissions\":[\"task:submit\"],\"hooks\":[\"startup\",\"before_task\",\"after_task\",\"shutdown\"],\"launcher_modes\":[\"dev\",\"serve\"],\"serve_ready\":true}'\n\
           exit 0\n\
         fi\n\
         exit 3\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();
    binary
}

fn spec(binary: &Path) -> serde_json::Value {
    let directory = binary.parent().unwrap();
    let source = directory.join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(
        source.join("README.md"),
        "Release fixture source, not native acceptance.\n",
    )
    .unwrap();
    git(&source, &["init", "-q"]);
    git(&source, &["add", "README.md"]);
    git(
        &source,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    let revision = String::from_utf8(git(&source, &["rev-parse", "HEAD"])).unwrap();
    let tree = git(&source, &["archive", "--format=tar", "HEAD"]);
    let bundle = directory.join("source.bundle");
    git(
        &source,
        &["bundle", "create", bundle.to_str().unwrap(), "HEAD"],
    );
    serde_json::json!({
        "schema": 1,
        "release": "2026.11.19-fixture",
        "signer": "fixture-release",
        "target": install::TrustPolicy::host_target(),
        "runtime": "0.1.0",
        "adk": "2.3.0",
        "protocol": "1.0",
        "protocol_min": "1.0",
        "protocol_max": "1.0",
        "source": {
            "revision": format!("git:{}", revision.trim()),
            "tree_digest": common::bytes_digest(&tree),
            "dirty": false
        },
        "source_bundle": bundle,
        "build": {
            "toolchain": "rust 1.100.0",
            "profile": "release",
            "command": "fixture shell script; not a compiled release",
            "reproducible": false
        },
        "packages": [{
            "name": "fixture",
            "version": "1.2.3",
            "binary": binary,
            "capabilities": [
                {"id": "fixture-agent", "kind": "agent", "description": "fixture", "permissions": []},
                {"id": "fixture-tool", "kind": "tool", "description": "fixture", "permissions": []}
            ],
            "required_permissions": ["task:submit"],
            "features": ["agents", "runner", "sessions", "server", "cli"],
            "launcher_modes": ["dev", "serve"],
            "hooks": ["startup", "before_task", "after_task", "shutdown"]
        }]
    })
}

#[test]
fn assembles_and_verifies_signed_release_with_source_identity() {
    let directory = tempfile::tempdir().unwrap();
    let binary = write_fixture(directory.path());
    let spec_path = directory.path().join("release.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&spec(&binary)).unwrap(),
    )
    .unwrap();
    let key_path = directory.path().join("release.key");
    let (seed, public) = install::release::keygen();
    std::fs::write(&key_path, seed).unwrap();
    let output = directory.path().join("dist");

    let report = install::release::assemble(&spec_path, &output, &key_path).unwrap();
    assert_eq!(report.release, "2026.11.19-fixture");
    assert_eq!(report.manifest.packages.len(), 1);
    assert_eq!(
        report.manifest.packages[0].capabilities,
        ["fixture-agent", "fixture-tool"]
    );
    assert!(report.manifest.source.revision.starts_with("git:"));
    assert_eq!(report.manifest.signer_public_key, public);
    assert!(output.join("SHA256SUMS").exists());
    assert!(output.join("SHA256SUMS.ed25519").exists());
    assert!(output.join("provenance.json").exists());
    assert!(output.join("release-manifest.json").exists());

    let verified = install::release::verify(&output, &public).unwrap();
    assert_eq!(verified, report.manifest);

    // Tampering with a package blob must fail verification before execution.
    let manifest = std::fs::read_to_string(output.join("release-manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let directory = manifest["packages"][0]["directory"].as_str().unwrap();
    let blobs = output.join(directory).join("blobs/sha256");
    let victim = std::fs::read_dir(&blobs)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.is_file())
        .unwrap();
    std::fs::write(victim, b"tampered").unwrap();
    let error = install::release::verify(&output, &public).unwrap_err();
    assert!(
        error.code == "release.checksum_mismatch"
            || error.code == "install.manifest_digest_mismatch"
            || error.code == "install.layout_invalid",
        "unexpected error: {}",
        error.code
    );
}

#[test]
fn refuses_metadata_that_disagrees_with_the_binary_probe() {
    let directory = tempfile::tempdir().unwrap();
    let binary = write_fixture(directory.path());
    let mut document = spec(&binary);
    document["packages"][0]["capabilities"] = serde_json::json!([
        {"id": "different", "kind": "agent", "description": "wrong", "permissions": []}
    ]);
    let spec_path = directory.path().join("release.json");
    std::fs::write(&spec_path, serde_json::to_vec(&document).unwrap()).unwrap();
    let key_path = directory.path().join("release.key");
    let (seed, _) = install::release::keygen();
    std::fs::write(&key_path, seed).unwrap();

    let error = install::release::assemble(&spec_path, &directory.path().join("dist"), &key_path)
        .unwrap_err();
    assert_eq!(error.code, "release.probe_mismatch");
}

#[test]
fn release_authentication_requires_an_independently_trusted_key() {
    let (directory, public) = assembled();
    let (_, other_key) = install::release::keygen();
    let error = install::release::verify(&directory.path().join("dist"), &other_key).unwrap_err();
    assert_eq!(error.code, "release.signature_invalid");
    install::release::verify(&directory.path().join("dist"), &public).unwrap();
}

fn assembled() -> (tempfile::TempDir, String) {
    let directory = tempfile::tempdir().unwrap();
    let binary = write_fixture(directory.path());
    let spec_path = directory.path().join("release.json");
    std::fs::write(&spec_path, serde_json::to_vec(&spec(&binary)).unwrap()).unwrap();
    let (seed, public) = install::release::keygen();
    let key = directory.path().join("test-only.key");
    std::fs::write(&key, seed).unwrap();
    install::release::assemble(&spec_path, &directory.path().join("dist"), &key).unwrap();
    (directory, public)
}

#[test]
fn omitted_manifest_cannot_escape_signed_inventory() {
    use base64::Engine;
    use ed25519_dalek::Signer;
    let (directory, public) = assembled();
    let root = directory.path().join("dist");
    let sums = std::fs::read_to_string(root.join("SHA256SUMS")).unwrap();
    let sums = sums
        .lines()
        .filter(|line| !line.ends_with("  release-manifest.json"))
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    std::fs::write(root.join("SHA256SUMS"), &sums).unwrap();
    let seed = std::fs::read_to_string(directory.path().join("test-only.key")).unwrap();
    let seed: [u8; 32] = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(seed)
        .unwrap()
        .try_into()
        .unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(key.sign(sums.as_bytes()).to_bytes());
    std::fs::write(root.join("SHA256SUMS.ed25519"), signature).unwrap();
    assert!(install::release::verify(&root, &public).is_err());
}

#[test]
fn existing_output_is_never_overwritten_and_symlinks_are_refused() {
    let (directory, public) = assembled();
    let root = directory.path().join("dist");
    let error = install::release::assemble(
        &directory.path().join("release.json"),
        &root,
        &directory.path().join("test-only.key"),
    )
    .unwrap_err();
    assert_eq!(error.code, "release.storage_unavailable");
    install::release::verify(&root, &public).unwrap();
    std::os::unix::fs::symlink("../test-only.key", root.join("secret-link")).unwrap();
    assert!(install::release::verify(&root, &public).is_err());
}

#[test]
fn unsigned_extra_file_is_rejected() {
    let (directory, public) = assembled();
    let root = directory.path().join("dist");
    std::fs::write(root.join("unsigned.txt"), b"unexpected").unwrap();
    assert!(install::release::verify(&root, &public).is_err());
}

#[test]
fn source_digest_and_release_paths_are_checked_before_writing() {
    for mutation in ["source", "path", "compatibility"] {
        let directory = tempfile::tempdir().unwrap();
        let binary = write_fixture(directory.path());
        let mut document = spec(&binary);
        match mutation {
            "source" => {
                document["source"]["tree_digest"] = common::bytes_digest(b"wrong source").into()
            }
            "path" => document["packages"][0]["name"] = "../../outside".into(),
            _ => document["adk"] = "0.0.0".into(),
        }
        let path = directory.path().join("release.json");
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let (seed, _) = install::release::keygen();
        let key = directory.path().join("test-only.key");
        std::fs::write(&key, seed).unwrap();
        let output = directory.path().join("dist");
        assert!(
            install::release::assemble(&path, &output, &key).is_err(),
            "{mutation}"
        );
        assert!(
            !output.exists(),
            "validation must precede publication: {mutation}"
        );
    }
}
