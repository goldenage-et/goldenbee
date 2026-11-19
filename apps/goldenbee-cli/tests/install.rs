//! Cross-process installation and dispatch checks.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_gb")
}

fn fake_binary(path: &Path, label: &str) {
    fs::write(path, format!("#!/bin/sh\nprintf '{label} %s\\n' \"$*\"\n"))
        .expect("write fake binary");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod fake binary");
}

fn run(args: &[&str], release: &Path, bin: &Path, config: &Path, data: &Path) -> String {
    let output = Command::new(binary())
        .args(args)
        .env("GOLDENBEE_BIN_DIR", bin)
        .env("GOLDENBEE_CONFIG_DIR", config)
        .env("GOLDENBEE_DATA_DIR", data)
        .env("GOLDENBEE_RELEASE_DIR", release)
        .output()
        .expect("gb runs");
    assert!(
        output.status.success(),
        "gb {:?} failed: {}{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn installs_components_and_dispatches_with_default_config() {
    let temp = TempDir::new().expect("tempdir");
    let release = temp.path().join("release");
    let bin = temp.path().join("bin");
    let config = temp.path().join("config");
    let data = temp.path().join("data");
    fs::create_dir_all(&release).expect("release dir");
    fake_binary(&release.join("gb-gateway"), "fake-gateway");
    fake_binary(&release.join("gb-composer"), "fake-composer");
    fake_binary(&release.join("gb-client"), "fake-client");

    let stdout = run(&["i", "all"], &release, &bin, &config, &data);
    assert!(stdout.contains("installed"));
    assert!(bin.join("gb-gateway").is_file());
    assert!(bin.join("gb-composer").is_file());
    assert!(!bin.join("gb-client").exists());

    let gateway: serde_json::Value =
        serde_json::from_slice(&fs::read(config.join("gateway.json")).expect("gateway config"))
            .expect("gateway json");
    let composer: serde_json::Value =
        serde_json::from_slice(&fs::read(config.join("composer.json")).expect("composer config"))
            .expect("composer json");
    let token = fs::read_to_string(config.join("local-token"))
        .expect("token")
        .trim()
        .to_string();
    assert_eq!(gateway["callers"][0]["token"], token);
    assert_eq!(composer["callers"][0]["token"], token);
    assert_eq!(gateway["hosts"][0]["token"], token);
    assert_eq!(
        fs::metadata(config.join("gateway.json"))
            .expect("gateway mode")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let gateway_args = run(&["gw", "status"], &release, &bin, &config, &data);
    assert!(gateway_args.contains("--config"));
    assert!(gateway_args.contains(&config.join("gateway.json").display().to_string()));

    let composer_args = run(&["cmp", "serve"], &release, &bin, &config, &data);
    assert!(composer_args.contains("--config"));
    assert!(composer_args.contains(&config.join("composer.json").display().to_string()));

    run(&["i", "client-cli"], &release, &bin, &config, &data);
    assert!(bin.join("gb-client").is_file());
}

#[test]
fn existing_config_and_binary_are_kept_without_force() {
    let temp = TempDir::new().expect("tempdir");
    let release = temp.path().join("release");
    let bin = temp.path().join("bin");
    let config = temp.path().join("config");
    let data = temp.path().join("data");
    fs::create_dir_all(&release).expect("release dir");
    fs::create_dir_all(&bin).expect("bin dir");
    fs::create_dir_all(&config).expect("config dir");
    fake_binary(&release.join("gb-gateway"), "fake-gateway");
    fs::write(bin.join("gb-gateway"), "existing").expect("existing binary");
    fs::write(config.join("gateway.json"), "{\"keep\":true}").expect("existing config");

    let stdout = run(&["i", "gateway"], &release, &bin, &config, &data);
    assert!(stdout.contains("kept existing"));
    assert_eq!(
        fs::read_to_string(bin.join("gb-gateway")).expect("binary"),
        "existing"
    );
    assert_eq!(
        fs::read_to_string(config.join("gateway.json")).expect("config"),
        "{\"keep\":true}"
    );

    run(&["i", "gateway", "--force"], &release, &bin, &config, &data);
    assert_ne!(
        fs::read_to_string(bin.join("gb-gateway")).expect("binary"),
        "existing"
    );
    assert!(fs::read_to_string(config.join("gateway.json"))
        .expect("config")
        .contains("callers"));
}
