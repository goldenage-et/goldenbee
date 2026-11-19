//! Client journey through the real CLI executable to Gateway and Workers.

#[path = "../../goldenbee-gateway/tests/support/mod.rs"]
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
fn cli_executable_reaches_explicit_worker_through_gateway() {
    let stack = Stack::start();

    let health = stack.cli(&["health", "--json"]);
    assert!(health.status.success());
    assert_eq!(stdout_json(&health)["status"], "live");

    let workers = stack.cli(&["workers", "--json"]);
    assert!(workers.status.success());
    let workers = stdout_json(&workers);
    assert!(workers
        .as_array()
        .is_some_and(|workers| workers.iter().any(|worker| worker["id"] == "worker-a")));

    let submitted = stack.cli(&[
        "submit",
        "--worker",
        "worker-a",
        "--agent",
        "general",
        "--input",
        "sdk-only cli journey",
        "--session",
        "cli-sdk-session",
        "--key",
        "cli-sdk-journey",
        "--json",
    ]);
    assert!(
        submitted.status.success(),
        "{}",
        String::from_utf8_lossy(&submitted.stderr)
    );
    let submitted = stdout_json(&submitted);
    assert_eq!(submitted["worker"], "worker-a");
    let task_id = submitted["task_id"].as_str().unwrap();

    let mut projection = Value::Null;
    for _ in 0..300 {
        let task = stack.cli(&["task", task_id, "--json"]);
        assert!(task.status.success());
        projection = stdout_json(&task);
        if is_terminal(&projection["task"]["state"]) {
            break;
        }
        sleep(Duration::from_millis(20));
    }
    assert_eq!(projection["task"]["state"], "succeeded");
    assert!(projection["task"]["output"]
        .as_str()
        .unwrap_or_default()
        .contains("sdk-only cli journey"));

    let events = stack.cli(&["events", task_id, "--json"]);
    assert!(events.status.success());
    assert!(stdout_json(&events)["events"]
        .as_array()
        .is_some_and(|events| !events.is_empty()));

    let cancelled = stack.cli(&["cancel", task_id, "--json"]);
    assert!(cancelled.status.success());
    assert!(!String::from_utf8_lossy(&cancelled.stdout).contains(GATEWAY_TOKEN));
}

#[test]
fn cli_interactive_profile_selects_worker_and_agent() {
    let stack = Stack::start();
    let home = tempfile::tempdir().unwrap();
    let added = std::process::Command::new(env!("CARGO_BIN_EXE_gb-client"))
        .env("GOLDENBEE_CLI_HOME", home.path())
        .args([
            "gw",
            "add",
            "interactive",
            &stack.gateway_base,
            GATEWAY_TOKEN,
        ])
        .output()
        .expect("profile command runs");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_gb-client"))
        .env("GOLDENBEE_CLI_HOME", home.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("interactive cli starts");
    std::io::Write::write_all(
        child.stdin.as_mut().expect("interactive stdin"),
        b"/worker\n1\n/agent echo\ninteractive echo\n/exit\n",
    )
    .expect("interactive input");
    let output = child.wait_with_output().expect("interactive cli exits");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("worker   worker-a"), "stdout: {stdout}");
    assert!(stdout.contains("agent    echo"), "stdout: {stdout}");
    assert!(stdout.contains("interactive echo"), "stdout: {stdout}");
}

#[test]
fn sdk_watch_replays_task_events() {
    let stack = Stack::start();
    let submitted = stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "watch journey",
        "watch-session",
        "watch-key",
    );
    assert!(submitted.status().is_success());
    let submitted: Value = submitted.json().unwrap();
    let task_id = submitted["task_id"].as_str().unwrap().to_string();
    let client = sdk::Client::new(&stack.gateway_base, GATEWAY_TOKEN);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let kinds = runtime.block_on(async {
        let mut watch = client.watch(&task_id, 0).await.expect("watch opens");
        let mut kinds = Vec::new();
        while let Some(event) = watch.next().await.expect("stream event") {
            kinds.push(event.kind);
        }
        kinds
    });
    drop(runtime);
    drop(stack);
    assert!(
        kinds.iter().any(|kind| kind == "task.succeeded"),
        "{kinds:?}"
    );
    assert!(kinds.iter().any(|kind| kind == "task.running"), "{kinds:?}");
}

#[test]
fn sdk_watch_from_cursor_recovers_completion_without_duplicates() {
    let stack = Stack::start();
    let submitted = stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "delay:400 cursor journey",
        "cursor-session",
        "cursor-key",
    );
    assert!(submitted.status().is_success());
    let submitted: Value = submitted.json().unwrap();
    let task_id = submitted["task_id"].as_str().unwrap().to_string();
    let client = sdk::Client::new(&stack.gateway_base, GATEWAY_TOKEN);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let (cursor, seen) = runtime.block_on(async {
        let mut watch = client.watch(&task_id, 0).await.expect("watch opens");
        let mut seen = Vec::new();
        loop {
            let event = watch.next().await.expect("event").expect("stream event");
            seen.push(event.seq);
            if event.kind == "task.running" {
                break;
            }
        }
        (watch.last_durable_seq(), seen)
    });

    for _ in 0..200 {
        let task = runtime.block_on(client.task(&task_id)).expect("task");
        if task.task.state.is_terminal() {
            break;
        }
        sleep(Duration::from_millis(20));
    }

    let (kinds, replayed) = runtime.block_on(async {
        let mut watch = client
            .watch_from(&task_id, cursor)
            .await
            .expect("watch reconnects");
        let mut kinds = Vec::new();
        let mut replayed = Vec::new();
        while let Some(event) = watch.next().await.expect("stream event") {
            kinds.push(event.kind);
            replayed.push(event.seq);
        }
        (kinds, replayed)
    });
    drop(runtime);

    assert!(
        kinds.iter().any(|kind| kind == "task.succeeded"),
        "{kinds:?}"
    );
    assert!(
        replayed.iter().all(|seq| !seen.contains(seq)),
        "reconnect replayed {seen:?} again as {replayed:?}"
    );
    let tasks = stack.worker_a.tasks();
    assert_eq!(tasks.len(), 1, "watching resubmitted the prompt");
}

#[test]
fn cli_watch_journey_replays_and_exits_without_resubmitting() {
    let stack = Stack::start();
    let submitted = stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "cli watch journey",
        "cli-watch-session",
        "cli-watch-key",
    );
    assert!(submitted.status().is_success());
    let submitted: Value = submitted.json().unwrap();
    let task_id = submitted["task_id"].as_str().unwrap();
    stack.wait_task(GATEWAY_TOKEN, task_id);

    let output = stack.cli(&["watch", task_id]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("task.succeeded"), "{stdout}");
    assert_eq!(stack.worker_a.tasks().len(), 1);
}
