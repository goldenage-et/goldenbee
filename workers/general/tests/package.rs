//! Package registration and end-to-end execution checks for the General Worker.

use std::process::Command;

mod fixture;

#[test]
fn manifest_matches_compiled_registrations() {
    let manifest = general::manifest();
    let ids: Vec<_> = manifest
        .capabilities
        .iter()
        .map(|capability| capability.id.as_str())
        .collect();
    assert!(ids.contains(&"manual"));
    assert!(ids.contains(&"exec_command"));
    assert!(!ids.contains(&"general"));
    assert!(!ids.contains(&"echo"));
}

#[test]
fn manifest_declares_installed_functionality_only() {
    let manifest = general::manifest();
    let ids: Vec<_> = manifest
        .capabilities
        .iter()
        .map(|capability| capability.id.clone())
        .collect();
    assert_eq!(
        ids,
        vec![
            "manual",
            "exec_command",
            "save_artifact",
            "browser",
            "observe_knowledge",
            "recall_knowledge",
        ]
    );
    assert!(!ids.iter().any(|id| id == "coding" || id == "knowledge"));
}

#[test]
fn cli_runs_a_file_defined_agent_through_shared_runtime() {
    let data = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "general-cli".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let output = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "run",
            "--config",
            fixture.config.to_str().unwrap(),
            "--input",
            "hello package",
            "--json",
        ])
        .output()
        .expect("general runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let evidence: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("structured evidence");
    assert_eq!(evidence["state"], "succeeded");
    assert!(evidence["output"]
        .as_str()
        .unwrap_or_default()
        .contains("hello package"));
    assert!(evidence["native_invocation_id"].as_str().is_some());
    assert!(evidence["asset_revision"].as_u64().unwrap_or_default() >= 1);
}

#[test]
fn capabilities_distinguish_installed_from_configured() {
    let data = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "general-caps".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let output = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "capabilities",
            "--config",
            fixture.config.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("general runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    let installed = value["installed"]["capabilities"]
        .as_array()
        .expect("installed");
    assert!(installed.iter().any(|entry| entry["id"] == "exec_command"));
    let configured = value["configured"].as_array().expect("configured");
    assert!(configured.iter().any(|entry| entry["id"] == "general"));
    assert!(configured.iter().any(|entry| entry["id"] == "manual"));
}
