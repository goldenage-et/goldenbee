//! Stored Gateway profiles through the real CLI executable.

use serde_json::Value;
use std::process::{Command, Output};

fn cli(home: &std::path::Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gb-client"))
        .env("GOLDENBEE_CLI_HOME", home)
        .args(arguments)
        .output()
        .expect("cli runs")
}

#[test]
fn profiles_add_list_detail_and_switch() {
    let home = tempfile::tempdir().unwrap();
    let added = cli(
        home.path(),
        &[
            "gw",
            "add",
            "local",
            "http://127.0.0.1:8088",
            "token-one-123456",
        ],
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    assert!(String::from_utf8_lossy(&added.stdout).contains("active"));

    let second = cli(
        home.path(),
        &[
            "gateway",
            "add",
            "team",
            "http://127.0.0.1:8099",
            "token-two-123456",
        ],
    );
    assert!(second.status.success());

    let listed = cli(home.path(), &["gw", "list", "--json"]);
    assert!(listed.status.success());
    let profiles: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(profiles.as_array().unwrap().len(), 2);
    assert_eq!(profiles[0]["name"], "local");
    assert_eq!(profiles[0]["active"], true);
    assert!(!profiles[0]["token"]
        .as_str()
        .unwrap()
        .contains("one-123456"));

    let switched = cli(home.path(), &["gw", "switch", "team"]);
    assert!(switched.status.success());
    let detail = cli(home.path(), &["gw", "detail", "team", "--json"]);
    assert!(detail.status.success());
    let detail: Value = serde_json::from_slice(&detail.stdout).unwrap();
    assert_eq!(detail["active"], true);
    assert_eq!(detail["url"], "http://127.0.0.1:8099");

    let missing = cli(home.path(), &["gw", "detail", "absent"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("cli.profile_not_found"));
}

#[test]
fn bare_cli_requires_an_active_profile() {
    let home = tempfile::tempdir().unwrap();
    let output = cli(home.path(), &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("gw add"));
}
