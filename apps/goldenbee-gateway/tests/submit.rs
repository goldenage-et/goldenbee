//! Durable Gateway dispatch intent, reconciliation, and uncertainty.
//!
//! Each test counts real Worker tasks and native invocations so a retry that
//! duplicates work fails visibly.

mod support;

use serde_json::Value;
use support::{Options, Stack, GATEWAY_TOKEN};

fn error_body(response: reqwest::blocking::Response) -> (reqwest::StatusCode, Value) {
    let status = response.status();
    let body = response.json::<Value>().expect("error body is JSON");
    (status, body)
}

fn event_kinds(page: &Value) -> Vec<String> {
    page["events"]
        .as_array()
        .expect("events array")
        .iter()
        .map(|event| event["kind"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// A Gateway crash after the Worker accepted but before the Gateway mapping
/// committed converges on the same Worker task after restart. The retry is the
/// original key, not a fresh uncorrelated submission.
#[test]
fn gateway_restart_after_remote_acceptance_converges_on_one_worker_task() {
    let faults = tempfile::tempdir().expect("fault directory");
    let mut stack = Stack::start_with_options(Options {
        gateway_faults: Some(faults.path().to_path_buf()),
        ..Options::default()
    });
    std::fs::write(faults.path().join("after-acceptance"), b"once")
        .expect("fault marker is writable");

    let _ = stack.try_submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "accepted before mapping",
        "post-accept-session",
        "post-accept-key",
    );
    stack.wait_for_gateway_fault_exit();
    assert_eq!(
        stack.worker_a.tasks().len(),
        1,
        "the Worker accepted before the Gateway stopped"
    );
    stack.restart_gateway();

    let accepted: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "accepted before mapping",
            "post-accept-session",
            "post-accept-key",
        )
        .json()
        .expect("reconciled submission is JSON");
    assert_eq!(accepted["replayed"], true);
    let execution_id = accepted["task_id"].as_str().expect("execution id");
    assert!(execution_id.starts_with("exec-"), "{accepted}");

    let tasks = stack.worker_a.tasks();
    assert_eq!(tasks.len(), 1, "the retry must not create another task");
    assert_eq!(tasks[0].attempt, 1);

    let again: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "accepted before mapping",
            "post-accept-session",
            "post-accept-key",
        )
        .json()
        .expect("second replay is JSON");
    assert_eq!(again["task_id"], execution_id);
    assert_eq!(again["replayed"], true);
    assert_eq!(stack.worker_a.tasks().len(), 1);
}

/// A Gateway crash after intent persistence but before any remote request
/// re-uses the same external identity and dispatches exactly once after restart.
#[test]
fn gateway_restart_before_dispatch_reuses_durable_intent() {
    let faults = tempfile::tempdir().expect("fault directory");
    let mut stack = Stack::start_with_options(Options {
        gateway_faults: Some(faults.path().to_path_buf()),
        ..Options::default()
    });
    std::fs::write(faults.path().join("after-intent"), b"once").expect("fault marker");

    let _ = stack.try_submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "intent before dispatch",
        "before-dispatch-session",
        "before-dispatch-key",
    );
    stack.wait_for_gateway_fault_exit();
    assert!(
        stack.worker_a.tasks().is_empty(),
        "no Worker request may occur before the injected crash"
    );
    stack.restart_gateway();

    let accepted: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "intent before dispatch",
            "before-dispatch-session",
            "before-dispatch-key",
        )
        .json()
        .expect("reconciled submission is JSON");
    let execution_id = accepted["task_id"].as_str().expect("execution id");
    assert!(execution_id.starts_with("exec-"), "{accepted}");
    assert_eq!(accepted["replayed"], false);

    let tasks = stack.worker_a.tasks();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].attempt, 1);

    let retry: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "intent before dispatch",
            "before-dispatch-session",
            "before-dispatch-key",
        )
        .json()
        .expect("replay is JSON");
    assert_eq!(retry["task_id"], execution_id);
    assert_eq!(retry["replayed"], true);
    assert_eq!(stack.worker_a.tasks().len(), 1);
}

/// A failed intent commit is not acceptance: no external identity is usable and
/// no remote dispatch happens. The same key then admits exactly one task.
#[test]
fn dispatch_persistence_failure_never_fabricates_acceptance() {
    let faults = tempfile::tempdir().expect("fault directory");
    let stack = Stack::start_with_options(Options {
        gateway_faults: Some(faults.path().to_path_buf()),
        ..Options::default()
    });
    std::fs::write(faults.path().join("persist-failure"), b"once").expect("fault marker");

    let (status, error) = error_body(stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "persist fault",
        "persist-session",
        "persist-key",
    ));
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["code"], "storage.unavailable");
    assert!(error["retryable"].as_bool().unwrap_or(false));
    assert!(
        stack.worker_a.tasks().is_empty(),
        "intent persistence failure must not dispatch"
    );

    let accepted: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "persist fault",
            "persist-session",
            "persist-key",
        )
        .json()
        .expect("retry is JSON");
    assert_eq!(accepted["replayed"], false);
    assert_eq!(stack.worker_a.tasks().len(), 1);
}

/// A changed payload under one key conflicts at the Gateway before any remote
/// dispatch, while the Worker's accepted request stays untouched.
#[test]
fn changed_payload_conflicts_at_gateway_without_duplicate_dispatch() {
    let stack = Stack::start();
    let first: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "original payload",
            "conflict-session",
            "conflict-key",
        )
        .json()
        .expect("first submission is JSON");
    let execution_id = first["task_id"].as_str().expect("execution id").to_string();
    let routed = stack.wait_task(GATEWAY_TOKEN, &execution_id);
    let worker_task_id = routed["task"]["id"].as_str().expect("worker task id");

    let (status, conflict) = error_body(stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "changed payload",
        "conflict-session",
        "conflict-key",
    ));
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "task.idempotency_conflict");
    assert_eq!(conflict["details"]["execution_id"], execution_id);
    assert!(!conflict["recovery"].as_str().unwrap_or_default().is_empty());

    let tasks = stack.worker_a.tasks();
    assert_eq!(tasks.len(), 1, "Gateway conflict must not dispatch again");
    assert_eq!(tasks[0].id, worker_task_id);
    assert_eq!(tasks[0].input, "original payload");
    assert_eq!(tasks[0].attempt, 1);
}

/// Idempotency and mappings are scoped by authenticated namespace and caller.
/// Another namespace using the same key cannot read or inherit this execution.
#[test]
fn mappings_are_unreadable_across_namespaces() {
    let stack = Stack::start();
    let first: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "shared payload",
            "namespace-session",
            "namespace-key",
        )
        .json()
        .expect("first submission is JSON");
    let first_id = first["task_id"].as_str().expect("execution id");
    stack.wait_task(GATEWAY_TOKEN, first_id);

    let foreign_response = stack.submit(
        support::OTHER_CALLER_TOKEN,
        "worker-b",
        "general",
        "shared payload",
        "namespace-session",
        "namespace-key",
    );
    let foreign_status = foreign_response.status();
    let foreign: Value = foreign_response.json().expect("foreign submission is JSON");
    assert!(
        foreign_status.is_success(),
        "foreign submission failed: {foreign_status} {foreign}"
    );
    let foreign_id = foreign["task_id"].as_str().expect("execution id");
    assert_ne!(foreign_id, first_id);
    assert_eq!(stack.worker_a.tasks().len(), 1);
    assert_eq!(
        stack.worker_b.tasks_with("tenant-b-operator").len(),
        1,
        "the foreign namespace has its own Worker task"
    );
    stack.wait_task(support::OTHER_CALLER_TOKEN, foreign_id);

    let client = Stack::client();
    let denied = client
        .get(format!("{}/v1/tasks/{first_id}", stack.gateway_base))
        .bearer_auth(support::OTHER_CALLER_TOKEN)
        .send()
        .expect("foreign read responds");
    assert_eq!(denied.status(), reqwest::StatusCode::NOT_FOUND);
    let body: Value = denied.json().expect("denial is JSON");
    assert_eq!(body["code"], "route.task.not_found");

    let reverse = client
        .get(format!("{}/v1/tasks/{foreign_id}", stack.gateway_base))
        .bearer_auth(GATEWAY_TOKEN)
        .send()
        .expect("reverse read responds");
    assert_eq!(reverse.status(), reqwest::StatusCode::NOT_FOUND);
}

/// An unreachable Worker changes only the Gateway connectivity projection. The
/// stored Worker outcome stays exactly what the Worker recorded.
#[test]
fn unreachable_worker_does_not_rewrite_task_outcome() {
    let mut stack = Stack::start_with_options(Options {
        unpinned_worker_a: true,
        ..Options::default()
    });
    let accepted: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "survive connectivity",
            "connectivity-session",
            "connectivity-key",
        )
        .json()
        .expect("submission is JSON");
    let execution_id = accepted["task_id"].as_str().expect("execution id");
    let online = stack.wait_task(GATEWAY_TOKEN, execution_id);
    assert_eq!(online["connectivity"], "online");
    assert_eq!(online["task"]["state"], "succeeded");
    let worker_task_id = online["task"]["id"]
        .as_str()
        .expect("worker task id")
        .to_string();
    let output = online["task"]["output"].clone();

    stack.stop_worker_a();
    let offline = Stack::client()
        .get(format!("{}/v1/tasks/{execution_id}", stack.gateway_base))
        .bearer_auth(GATEWAY_TOKEN)
        .send()
        .expect("offline read responds");
    assert_eq!(offline.status(), reqwest::StatusCode::OK);
    let offline: Value = offline.json().expect("offline projection is JSON");
    assert_eq!(offline["connectivity"], "unreachable");
    assert_eq!(offline["task"]["state"], "succeeded");
    assert_eq!(offline["task"]["output"], output);
    assert_eq!(offline["task"]["id"], worker_task_id);

    stack.worker_a.restart();
    stack.restart_gateway();
    let direct = stack.worker_a.wait_direct(&worker_task_id);
    assert_eq!(direct.state, common::TaskState::Succeeded);
    assert_eq!(direct.output, output.as_str().map(str::to_string));

    let recovered = stack.wait_task(GATEWAY_TOKEN, execution_id);
    assert_eq!(recovered["connectivity"], "online");
    assert_eq!(recovered["task"]["state"], "succeeded");
    assert_eq!(recovered["task"]["output"], output);
}

/// The Gateway keeps only dispatch mapping and a projection; the Worker's store
/// is the authority for the native invocation count.
#[test]
fn accepted_mapping_is_stable_and_has_one_native_invocation() {
    let stack = Stack::start();
    let accepted: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "stable mapping",
            "mapping-session",
            "mapping-key",
        )
        .json()
        .expect("submission is JSON");
    let execution_id = accepted["task_id"]
        .as_str()
        .expect("execution id")
        .to_string();
    let routed = stack.wait_task(GATEWAY_TOKEN, &execution_id);
    let worker_task_id = routed["task"]["id"].as_str().expect("worker task id");
    let invocation = routed["task"]["native_invocation_id"]
        .as_str()
        .expect("native invocation")
        .to_string();

    let replay: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "stable mapping",
            "mapping-session",
            "mapping-key",
        )
        .json()
        .expect("replay is JSON");
    assert_eq!(replay["task_id"], execution_id);
    assert_eq!(replay["replayed"], true);

    let tasks = stack.worker_a.tasks();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, worker_task_id);
    assert_eq!(
        tasks[0].native_invocation_id.as_deref(),
        Some(invocation.as_str())
    );
}

/// The Gateway keeps its own external execution identity and reconciles the
/// retry through the original idempotency key, which returns the one native
/// invocation the Worker already recorded.
#[test]
fn lost_acknowledgment_is_reconciled_without_duplicate_native_execution() {
    let faults = tempfile::tempdir().expect("fault directory");
    let mut stack = Stack::start_with_options(Options {
        worker_a_faults: Some(faults.path().to_path_buf()),
        unpinned_worker_a: true,
        ..Options::default()
    });
    std::fs::write(faults.path().join("before-acknowledge"), b"once")
        .expect("fault marker is writable");

    let (status, unknown) = error_body(stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "lost ack input",
        "lost-ack-session",
        "lost-ack-key",
    ));
    assert_eq!(status, reqwest::StatusCode::ACCEPTED, "{unknown}");
    assert_eq!(unknown["code"], "task.unknown_submission");
    assert_eq!(unknown["retryable"], true);
    assert!(!unknown["recovery"].as_str().unwrap_or_default().is_empty());
    let execution_id = unknown["details"]["execution_id"]
        .as_str()
        .expect("uncertain submission exposes its external execution id")
        .to_string();
    assert!(execution_id.starts_with("exec-"), "{execution_id}");
    stack.worker_a.wait_for_fault_exit();
    stack.worker_a.restart();
    // A Gateway restart reloads the same durable intent and re-runs preflight
    // against the new Worker process identity.
    stack.restart_gateway();

    let response = stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "lost ack input",
        "lost-ack-session",
        "lost-ack-key",
    );
    let retry_status = response.status();
    let accepted: Value = response.json().expect("reconciled submission is JSON");
    assert!(
        retry_status.is_success(),
        "retry failed: {retry_status} {accepted}"
    );
    assert_eq!(accepted["task_id"], execution_id);
    assert_eq!(accepted["replayed"], true);
    assert_eq!(accepted["worker"], "worker-a");

    let tasks = stack.worker_a.tasks();
    assert_eq!(tasks.len(), 1, "the Worker must own exactly one task");
    let task = &tasks[0];
    assert_eq!(task.attempt, 1, "native execution is not repeated");
    assert!(
        task.native_invocation_id
            .as_deref()
            .is_some_and(|id| id.starts_with("inv-")),
        "the one native invocation is recorded"
    );
    let kinds = event_kinds(&stack.worker_a.events(&task.id));
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "task.running").count(),
        1,
        "one native dispatch: {kinds:?}"
    );

    let routed = stack.wait_task(GATEWAY_TOKEN, &execution_id);
    assert_eq!(routed["task"]["state"], "succeeded");
    assert_eq!(routed["task"]["id"], task.id);
}
