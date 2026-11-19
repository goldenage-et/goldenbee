//! Explicit Gateway routing across two real Worker processes.

mod support;

use serde_json::Value;
use support::{Stack, GATEWAY_TOKEN};

#[test]
fn explicit_target_reaches_only_the_named_worker() {
    let stack = Stack::start();
    let input = "route-only-worker-a";
    let response = stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        input,
        "session-route",
        "key-route",
    );
    let status = response.status();
    let accepted: Value = response.json().expect("gateway submission is JSON");
    assert_eq!(status, reqwest::StatusCode::OK, "{accepted}");
    assert_eq!(accepted["worker"], "worker-a");
    assert_eq!(accepted["worker_instance"], stack.worker_a.instance);
    assert_eq!(accepted["agent"], "general");
    let task_id = accepted["task_id"].as_str().expect("task id").to_string();

    let routed = stack.wait_task(GATEWAY_TOKEN, &task_id);
    assert_eq!(routed["task"]["state"], "succeeded");
    assert_eq!(routed["worker"], "worker-a");
    assert_eq!(routed["worker_instance"], stack.worker_a.instance);
    assert!(routed["task"]["output"]
        .as_str()
        .unwrap_or_default()
        .contains(input));

    let events = stack.events(GATEWAY_TOKEN, &task_id);
    assert!(events["events"]
        .as_array()
        .is_some_and(|events| !events.is_empty()));

    let worker_task_id = routed["task"]["id"]
        .as_str()
        .expect("worker task id")
        .to_string();
    let worker_a_ids: Vec<String> = stack
        .worker_a
        .tasks()
        .into_iter()
        .map(|task| task.id)
        .collect();
    assert!(
        worker_a_ids.contains(&worker_task_id),
        "worker-a must own the routed task"
    );
    let worker_b_ids: Vec<String> = stack
        .worker_b
        .tasks()
        .into_iter()
        .map(|task| task.id)
        .collect();
    assert!(
        !worker_b_ids.contains(&worker_task_id),
        "worker-b must never receive a task explicitly routed to worker-a"
    );
    assert!(
        worker_b_ids.is_empty(),
        "no fallback submission may appear on worker-b"
    );
}
