//! Gateway-originated task admission against one Worker Instance.
//!
//! Gateway proxies the Worker's finite capacity unchanged: excess work is
//! backpressured, queued work can be cancelled through the same route, and the
//! Worker's permit returns after terminal outcomes.

mod support;

use serde_json::Value;
use std::time::{Duration, Instant};
use support::{Stack, GATEWAY_TOKEN};

fn task_id(response: reqwest::blocking::Response) -> String {
    let status = response.status();
    let body = response.text().expect("submit body");
    assert!(
        status.is_success(),
        "gateway submit failed: {status} {body}"
    );
    let value: Value = serde_json::from_str(&body).expect("submit is JSON");
    value["task_id"].as_str().expect("task id").to_string()
}

fn worker_id(stack: &Stack, id: &str) -> String {
    stack.task(GATEWAY_TOKEN, id)["task"]["id"]
        .as_str()
        .expect("worker task id")
        .to_string()
}

fn wait_worker_state(stack: &Stack, id: &str, wanted: &str) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(15) {
        let task = stack.worker_a.task(id);
        let state = serde_json::to_value(task.state).expect("state serializes");
        if state.as_str() == Some(wanted) {
            return;
        }
        if task.state.is_terminal() {
            assert_eq!(state.as_str(), Some(wanted), "task finished early");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("worker task {id} did not reach {wanted}");
}

fn wait_worker_capacity(stack: &Stack, active: u64) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(15) {
        let status = stack.worker_a.status();
        if status["capacity"]["active"].as_u64() == Some(active) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "worker capacity did not reach active={active}: {}",
        stack.worker_a.status()
    );
}

#[test]
fn gateway_tasks_obey_worker_admission_and_queued_cancellation() {
    let stack = Stack::start_with_worker_env(&[
        ("GOLDENBEE_MAX_ACTIVE", "1".into()),
        ("GOLDENBEE_MAX_QUEUE", "1".into()),
        ("GOLDENBEE_PER_PRINCIPAL", "4".into()),
    ]);
    assert_eq!(stack.worker_a.status()["capacity"]["limit"], 1);

    let first = task_id(stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "delay:700 first",
        "gateway-queue-a",
        "gateway-1",
    ));
    wait_worker_capacity(&stack, 1);

    let second = task_id(stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "delay:100 second",
        "gateway-queue-b",
        "gateway-2",
    ));
    let second_worker = worker_id(&stack, &second);
    wait_worker_state(&stack, &second_worker, "queued");

    let excess = stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "delay:100 third",
        "gateway-queue-c",
        "gateway-3",
    );
    assert_eq!(excess.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let error: Value = excess.json().expect("backpressure is JSON");
    assert_eq!(error["code"], "capacity.queue_full");
    assert_eq!(error["retryable"], true);
    assert!(error["details"]["retry_after_ms"].is_string());

    let cancelled = stack.cancel(GATEWAY_TOKEN, &second);
    assert!(cancelled.status().is_success(), "gateway cancel failed");
    let cancelled: Value = cancelled.json().expect("cancel is JSON");
    assert_eq!(cancelled["task"]["state"], "cancelled");
    wait_worker_state(&stack, &second_worker, "cancelled");

    let first = stack.wait_task(GATEWAY_TOKEN, &first);
    assert_eq!(first["task"]["state"], "succeeded");
    wait_worker_capacity(&stack, 0);

    let after = task_id(stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "after gateway capacity",
        "gateway-queue-d",
        "gateway-4",
    ));
    let after = stack.wait_task(GATEWAY_TOKEN, &after);
    assert_eq!(after["task"]["state"], "succeeded");
    assert!(after["task"]["output"]
        .as_str()
        .is_some_and(|output| output.contains("after gateway capacity")));
    wait_worker_capacity(&stack, 0);
}
