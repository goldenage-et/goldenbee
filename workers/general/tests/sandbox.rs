//! General package binding and end-to-end checks for the sandboxed command tool.

use general::sandbox_tool;
use std::process::Command;

mod fixture;

fn run(input: &str, data_dir: &std::path::Path, extra: &[(&str, &str)]) -> serde_json::Value {
    run_with_sandbox(input, data_dir, extra, None)
}

fn run_with_sandbox(
    input: &str,
    data_dir: &std::path::Path,
    extra: &[(&str, &str)],
    sandbox: Option<&str>,
) -> serde_json::Value {
    let model = extra
        .iter()
        .find(|(key, _)| *key == "GOLDENBEE_MODEL")
        .map(|(_, value)| (*value).to_string())
        .unwrap_or_else(|| "deterministic".into());
    let fixture = fixture::write(
        data_dir,
        fixture::Options {
            instance: "general-sandbox".into(),
            token: Some("dev-token".into()),
            model,
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    if let Some(sandbox) = sandbox {
        let mut text = std::fs::read_to_string(&fixture.config).expect("worker config");
        text.push_str(&format!("\n[sandbox]\n{sandbox}"));
        std::fs::write(&fixture.config, text).expect("sandbox config");
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_general"));
    command
        .args([
            "run",
            "--config",
            fixture.config.to_str().unwrap(),
            "--input",
            input,
            "--json",
        ])
        .env("GOLDENBEE_DATA_DIR", data_dir)
        .env("GOLDENBEE_SANDBOX_DIR", data_dir.join("workspace"));
    for (key, value) in extra {
        if *key == "GOLDENBEE_MODEL" {
            continue;
        }
        command.env(key, value);
    }
    let output = command.output().expect("general runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("structured evidence")
}

#[test]
fn exec_command_tool_keeps_native_adk_command_semantics() {
    let root = tempfile::tempdir().unwrap();
    let tool = sandbox_tool(root.path());
    use adk_rust::Tool;
    assert_eq!(tool.name(), "exec_command");
    let schema = tool.parameters_schema().expect("parameters schema");
    assert_eq!(schema["required"], serde_json::json!(["command"]));
    assert!(schema["properties"]["command"].is_object());
    assert!(schema["properties"]["working_dir"].is_object());
}

#[test]
fn native_agent_runs_the_bound_command_tool() {
    let data_dir = tempfile::tempdir().unwrap();
    let evidence = run(
        "sandbox:printf sandbox-result",
        data_dir.path(),
        &[("GOLDENBEE_MODEL", "deterministic")],
    );
    assert_eq!(evidence["state"], "succeeded");
    let output = evidence["output"].as_str().unwrap_or_default();
    assert!(output.contains("sandbox result"), "{output}");
    assert!(output.contains("sandbox-result"), "{output}");
}

#[test]
fn native_agent_cannot_read_a_forbidden_host_path() {
    let data_dir = tempfile::tempdir().unwrap();
    let host_secret = data_dir.path().join("host-secret.txt");
    std::fs::write(&host_secret, "HOST-FILE-CANARY").unwrap();
    let evidence = run(
        &format!("sandbox:cat {}", host_secret.display()),
        data_dir.path(),
        &[],
    );
    let output = evidence["output"].as_str().unwrap_or_default();
    assert!(!output.contains("HOST-FILE-CANARY"), "{output}");
    assert!(
        output.contains("No such file") || output.contains("not found"),
        "{output}"
    );
}

#[test]
fn operator_read_only_profile_blocks_a_workspace_write() {
    let data_dir = tempfile::tempdir().unwrap();
    let workspace = data_dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("existing.txt"), "READABLE").unwrap();
    let evidence = run_with_sandbox(
        "sandbox:cat existing.txt; printf 'MUTATED' > denied.txt; echo write=$?",
        data_dir.path(),
        &[("GOLDENBEE_MODEL", "deterministic")],
        Some("profile = \"read-only\"\n"),
    );
    assert_eq!(evidence["state"], "succeeded");
    let output = evidence["output"].as_str().unwrap_or_default();
    assert!(output.contains("READABLE"), "{output}");
    assert!(
        !output.contains("write=0"),
        "read-only sandbox allowed a write: {output}"
    );
    assert!(
        !workspace.join("denied.txt").exists(),
        "read-only sandbox wrote the workspace"
    );
}

#[test]
fn operator_network_disabled_blocks_a_process_network_attempt() {
    let data_dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let evidence = run_with_sandbox(
        &format!("/bin/bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}'; echo network=$?"),
        data_dir.path(),
        &[("GOLDENBEE_MODEL", "deterministic")],
        Some("profile = \"workspace-write\"\n\n[sandbox.network]\nenabled = false\n"),
    );
    let output = evidence["output"].as_str().unwrap_or_default();
    assert!(
        !output.contains("network=0"),
        "network was reachable under a disabled policy: {output}"
    );
    assert!(
        listener.accept().is_err(),
        "host listener received a sandbox connection"
    );
}

#[test]
fn operator_command_ceiling_blocks_an_unlisted_command() {
    let data_dir = tempfile::tempdir().unwrap();
    let workspace = data_dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let evidence = run_with_sandbox(
        "sandbox:touch denied.txt",
        data_dir.path(),
        &[("GOLDENBEE_MODEL", "deterministic")],
        Some("profile = \"workspace-write\"\ncommand_allow = [\"printf \"]\n"),
    );
    let output = evidence["output"].as_str().unwrap_or_default();
    assert!(
        output.contains("sandbox_command_denied"),
        "command ceiling was not enforced: {output}"
    );
    assert!(!workspace.join("denied.txt").exists());
}

#[test]
fn unavailable_sandbox_fails_closed_inside_the_native_agent() {
    let data_dir = tempfile::tempdir().unwrap();
    let canary = data_dir.path().join("host-fallback-canary");
    let evidence = run(
        &format!("sandbox:touch {}", canary.display()),
        data_dir.path(),
        &[("GOLDENBEE_SANDBOX_BWRAP", "/nonexistent/t32-bwrap")],
    );
    let output = evidence["output"].as_str().unwrap_or_default();
    assert!(output.contains("sandbox_unavailable"), "{output}");
    assert!(
        !canary.exists(),
        "missing boundary silently fell back to host execution"
    );
}
