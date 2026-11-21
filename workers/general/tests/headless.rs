//! Headless run protocol through the real Worker executable.
//!
//! These tests assert external CLI behavior only: structured output, ordered
//! JSONL frames, input source handling, and stable process exit classes.

use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

mod fixture;
use fixture::Options;

fn command(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_general"));
    command.args(["run", "--config", config.to_str().expect("config path")]);
    command
}

fn run(config: &Path, args: &[&str]) -> Output {
    command(config)
        .args(args)
        .output()
        .expect("headless run executes")
}

fn run_with_stdin(config: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = command(config)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("headless run starts");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("stdin write");
    child.wait_with_output().expect("headless run completes")
}

fn directory() -> tempfile::TempDir {
    tempfile::tempdir().expect("temporary directory")
}

#[test]
fn json_run_returns_one_structured_result_and_exit_zero() {
    let directory = directory();
    let fixture = fixture::write(directory.path(), Options::default());
    let output = run(&fixture.config, &["--input", "hello", "--json"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(0));
    let result: Value = serde_json::from_slice(&output.stdout).expect("structured result");
    assert_eq!(result["schema_version"], common::RUN_PROTOCOL_SCHEMA);
    assert_eq!(result["state"], "succeeded");
    assert_eq!(result["exit_code"], 0);
    assert!(
        result["output"]
            .as_str()
            .is_some_and(|output| output.contains("hello")),
        "{result}"
    );
    assert!(result["task_id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(result["session_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
}

#[test]
fn jsonl_run_starts_with_task_and_ends_with_terminal_frame() {
    let directory = directory();
    let fixture = fixture::write(directory.path(), Options::default());
    let output = run(&fixture.config, &["--input", "hello", "--jsonl"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .expect("utf-8 frames")
        .lines()
        .map(|line| serde_json::from_str(line).expect("frame JSON"))
        .collect();
    assert!(!lines.is_empty());
    assert_eq!(lines.first().unwrap()["type"], "task_started");
    assert_eq!(lines.last().unwrap()["type"], "task_completed");
    assert_eq!(lines.last().unwrap()["exit_code"], 0);
    assert_eq!(lines.last().unwrap()["state"], "succeeded");
    assert!(lines.iter().any(|frame| frame["type"] == "turn_started"));
    assert!(lines
        .windows(2)
        .all(|pair| pair[0]["task_id"] == pair[1]["task_id"]));
}

#[test]
fn stdin_is_read_once_and_bounded_before_execution() {
    let directory = directory();
    let fixture = fixture::write(directory.path(), Options::default());
    let output = run_with_stdin(&fixture.config, &["--input", "-", "--json"], "from-stdin");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).expect("structured result");
    assert!(
        result["output"]
            .as_str()
            .is_some_and(|output| output.contains("from-stdin")),
        "{result}"
    );

    let second = run_with_stdin(&fixture.config, &["--input", "-", "--json"], "again");
    assert!(
        second.status.success(),
        "a second run reads stdin independently"
    );

    let missing = directory.path().join("secret-input.txt");
    let output = run(
        &fixture.config,
        &["--input-file", missing.to_str().expect("path"), "--json"],
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("validation.run_source"), "{stderr}");
    assert!(!stderr.contains("secret-input.txt"), "{stderr}");
    assert!(
        !stderr.contains(directory.path().to_str().unwrap()),
        "{stderr}"
    );
}

#[test]
fn missing_input_is_a_usage_failure() {
    let directory = directory();
    let fixture = fixture::write(directory.path(), Options::default());
    let output = run(&fixture.config, &["--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stdout).is_empty());
}

#[test]
fn json_and_jsonl_cannot_be_combined() {
    let directory = directory();
    let fixture = fixture::write(directory.path(), Options::default());
    let output = run(&fixture.config, &["--input", "hello", "--json", "--jsonl"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cli.option_conflict"));
}

#[test]
fn an_unauthorized_principal_exits_with_the_authorization_class() {
    let directory = directory();
    let options = Options {
        token_scopes: vec!["session:read".into()],
        ..Options::default()
    };
    let fixture = fixture::write(directory.path(), options);
    let output = run(&fixture.config, &["--input", "hello", "--json"]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("scope."), "{stderr}");
}

#[test]
fn resume_of_an_unknown_session_fails_before_a_task_starts() {
    let directory = directory();
    let fixture = fixture::write(directory.path(), Options::default());
    let output = run(
        &fixture.config,
        &["--input", "hello", "--resume", "missing-session", "--json"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("session.not_found"));
}

#[test]
fn continue_without_a_session_is_a_usage_failure() {
    let directory = directory();
    let fixture = fixture::write(directory.path(), Options::default());
    let output = run(
        &fixture.config,
        &["--input", "hello", "--continue", "--json"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("session.not_found"));
}

#[test]
fn app_server_run_route_returns_the_same_structured_result() {
    use std::io::{BufRead, BufReader};
    use std::process::Child;

    let directory = directory();
    let fixture = fixture::write(directory.path(), Options::default());
    let mut child: Child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            fixture.config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("worker serves");
    let stdout = child.stdout.take().expect("stdout");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("listener announces");
    let base = line
        .split("listening on ")
        .nth(1)
        .expect("address announcement")
        .trim()
        .to_string();

    let response = reqwest::blocking::Client::new()
        .post(format!("{base}/v1/runs"))
        .bearer_auth("dev-token")
        .json(&serde_json::json!({"input": {"text": "hello"}}))
        .send()
        .expect("run responds");
    let status = response.status();
    let body = response.text().expect("run body");
    let _ = child.kill();
    let _ = child.wait();
    assert!(status.is_success(), "{status} {body}");
    let result: Value = serde_json::from_str(&body).expect("structured result");
    assert_eq!(result["state"], "succeeded");
    assert_eq!(result["exit_code"], 0);
    assert!(result["task_id"].as_str().is_some_and(|id| !id.is_empty()));
}
