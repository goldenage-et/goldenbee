//! Separate CLI process reaching Gateway only through the reusable SDK.

mod support;

use serde_json::Value;
use std::thread::sleep;
use std::time::Duration;
use support::{is_terminal, Stack, GATEWAY_TOKEN};

fn stdout_json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn cli_submits_inspects_events_and_cancels_through_the_sdk() {
    let stack = Stack::start();

    let health = stack.cli(&["health", "--json"]);
    assert!(
        health.status.success(),
        "{}",
        String::from_utf8_lossy(&health.stderr)
    );
    let health = stdout_json(&health);
    assert_eq!(health["status"], "live");
    assert_eq!(health["workers"], 3);
    assert_eq!(health["reachable"], 2);

    let workers = stack.cli(&["workers", "--json"]);
    assert!(workers.status.success());
    let workers = stdout_json(&workers);
    let ids: Vec<&str> = workers
        .as_array()
        .expect("worker list")
        .iter()
        .filter_map(|worker| worker["id"].as_str())
        .collect();
    assert!(ids.contains(&"worker-a"));
    assert!(
        !ids.contains(&"worker-b"),
        "CLI only sees Workers its caller is authorized for"
    );

    let submitted = stack.cli(&[
        "submit",
        "--worker",
        "worker-a",
        "--agent",
        "general",
        "--input",
        "cli journey through gateway",
        "--session",
        "cli-session",
        "--key",
        "cli-journey",
        "--json",
    ]);
    assert!(
        submitted.status.success(),
        "{}",
        String::from_utf8_lossy(&submitted.stderr)
    );
    assert!(!String::from_utf8_lossy(&submitted.stdout).contains(GATEWAY_TOKEN));
    assert!(!String::from_utf8_lossy(&submitted.stderr).contains(GATEWAY_TOKEN));
    let submitted: Value = stdout_json(&submitted);
    assert_eq!(submitted["worker"], "worker-a");
    assert_eq!(submitted["agent"], "general");
    let task_id = submitted["task_id"].as_str().unwrap().to_string();

    let mut last = Value::Null;
    for _ in 0..300 {
        let projection = stack.cli(&["task", &task_id, "--json"]);
        assert!(
            projection.status.success(),
            "{}",
            String::from_utf8_lossy(&projection.stderr)
        );
        last = stdout_json(&projection);
        if is_terminal(&last["task"]["state"]) {
            break;
        }
        sleep(Duration::from_millis(20));
    }
    assert_eq!(last["task"]["state"], "succeeded");
    assert!(last["task"]["output"]
        .as_str()
        .unwrap_or_default()
        .contains("cli journey through gateway"));
    assert_eq!(last["worker"], "worker-a");

    let events = stack.cli(&["events", &task_id, "--json"]);
    assert!(events.status.success());
    let events: Value = stdout_json(&events);
    assert!(events["events"]
        .as_array()
        .is_some_and(|events| !events.is_empty()));

    let cancelled = stack.cli(&["cancel", &task_id, "--json"]);
    assert!(
        cancelled.status.success(),
        "{}",
        String::from_utf8_lossy(&cancelled.stderr)
    );
    let cancelled: Value = stdout_json(&cancelled);
    assert!(matches!(
        cancelled["task"]["state"].as_str(),
        Some("succeeded" | "cancelled")
    ));
}

#[test]
fn cli_exposes_uncertain_submission_and_reuses_the_same_key() {
    let faults = tempfile::tempdir().expect("fault directory");
    let mut stack = Stack::start_with_options(support::Options {
        worker_a_faults: Some(faults.path().to_path_buf()),
        unpinned_worker_a: true,
        ..support::Options::default()
    });
    std::fs::write(faults.path().join("before-acknowledge"), b"once")
        .expect("fault marker is writable");

    let first = stack.cli(&[
        "submit",
        "--worker",
        "worker-a",
        "--agent",
        "general",
        "--input",
        "cli uncertain",
        "--session",
        "cli-uncertain-session",
        "--key",
        "cli-uncertain-key",
        "--json",
    ]);
    assert_eq!(first.status.code(), Some(5));
    assert!(first.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&first.stderr).to_string();
    assert!(stderr.contains("task.unknown_submission"), "{stderr}");
    assert!(stderr.contains("recovery:"), "{stderr}");
    let execution_id = stderr
        .lines()
        .find_map(|line| line.strip_prefix("execution_id: "))
        .expect("the CLI exposes the external execution id")
        .trim()
        .to_string();
    assert!(execution_id.starts_with("exec-"), "{execution_id}");
    assert!(!stderr.contains(GATEWAY_TOKEN));
    stack.worker_a.wait_for_fault_exit();

    stack.worker_a.restart();
    stack.restart_gateway();
    let retry = stack.cli(&[
        "submit",
        "--worker",
        "worker-a",
        "--agent",
        "general",
        "--input",
        "cli uncertain",
        "--session",
        "cli-uncertain-session",
        "--key",
        "cli-uncertain-key",
        "--json",
    ]);
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let accepted: Value = stdout_json(&retry);
    assert_eq!(accepted["task_id"], execution_id);
    assert_eq!(accepted["replayed"], true);
    assert_eq!(stack.worker_a.tasks().len(), 1);
    assert_eq!(stack.worker_a.tasks()[0].attempt, 1);
}

#[test]
fn cli_errors_are_stable_and_do_not_echo_secrets() {
    let stack = Stack::start();
    let missing = stack.cli(&["task", "task_absent", "--json"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("route.task.not_found"), "{stderr}");
    assert!(!stderr.contains(GATEWAY_TOKEN));

    let canary = "cli-secret-canary-do-not-log";
    let bad_option = stack.cli(&["--definitely-not-an-option", canary]);
    assert_eq!(bad_option.status.code(), Some(2));
    assert!(bad_option.stdout.is_empty());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&bad_option.stdout),
        String::from_utf8_lossy(&bad_option.stderr)
    );
    assert!(!combined.contains(canary));
    assert!(!combined.contains(GATEWAY_TOKEN));
}
