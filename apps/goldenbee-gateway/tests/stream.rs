//! Public Gateway SSE acceptance: reconnect across Gateway restart, typed
//! cursors, bounded fan-out, and redaction.

use serde_json::Value;

mod support;

use support::{Options, Stack, GATEWAY_TOKEN, OTHER_CALLER_TOKEN};

fn submit(stack: &Stack, token: &str, input: &str, key: &str) -> String {
    let response = stack.submit(token, "worker-a", "general", input, key, key);
    assert!(
        response.status().is_success(),
        "submit failed: {}",
        response.status()
    );
    let body: Value = response.json().expect("submit is JSON");
    body["task_id"].as_str().expect("task id").to_string()
}

fn error_code(response: reqwest::blocking::Response) -> String {
    response
        .json::<Value>()
        .ok()
        .and_then(|body| body["code"].as_str().map(str::to_string))
        .unwrap_or_default()
}

#[test]
fn gateway_restart_preserves_the_stream_and_never_resubmits() {
    let mut stack = Stack::start();
    let id = submit(
        &stack,
        GATEWAY_TOKEN,
        "delay:800 gateway reconnect",
        "gw-reconnect",
    );

    let mut first = stack.sse(GATEWAY_TOKEN, &id, 0);
    let mut cursor = 0;
    loop {
        let event = first.next_event().expect("first stream stays open");
        cursor = cursor.max(event.seq);
        if event.kind == "task.running" {
            break;
        }
    }
    drop(first);

    stack.restart_gateway();

    let mut second = stack.sse(GATEWAY_TOKEN, &id, cursor);
    let events = second.until_terminal();
    let succeeded = events
        .iter()
        .find(|event| event.kind == "task.succeeded")
        .expect("terminal outcome is recovered after Gateway restart");
    assert!(succeeded.payload["output"]
        .as_str()
        .is_some_and(|output| output.contains("gateway reconnect")));

    let projection = stack.task(GATEWAY_TOKEN, &id);
    assert_eq!(projection["task_id"].as_str(), Some(id.as_str()));
    assert_eq!(projection["task"]["state"], "succeeded");

    let worker_tasks = stack.worker_a.tasks();
    assert_eq!(worker_tasks.len(), 1, "reconnect submitted a second task");
    assert_eq!(
        worker_tasks[0].native_invocation_id,
        projection["task"]["native_invocation_id"]
            .as_str()
            .map(str::to_string),
        "reconnect created a second native invocation"
    );
}

#[test]
fn gateway_cursor_errors_are_typed_and_do_not_rewrite_connectivity() {
    let stack = Stack::start();
    let id = submit(&stack, GATEWAY_TOKEN, "delay:800 cursor", "gw-cursor");

    let client = Stack::client();
    let malformed = client
        .get(format!(
            "{}/v1/tasks/{id}/events/stream?after=abc",
            stack.gateway_base
        ))
        .bearer_auth(GATEWAY_TOKEN)
        .send()
        .unwrap();
    assert_eq!(malformed.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(error_code(malformed), "stream.cursor_invalid");

    let too_new = client
        .get(format!(
            "{}/v1/tasks/{id}/events/stream?after={}",
            stack.gateway_base,
            u64::MAX
        ))
        .bearer_auth(GATEWAY_TOKEN)
        .send()
        .unwrap();
    assert_eq!(too_new.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(error_code(too_new), "stream.cursor_invalid");

    let unauthorized = client
        .get(format!(
            "{}/v1/tasks/{id}/events/stream?after=0",
            stack.gateway_base
        ))
        .send()
        .unwrap();
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

    let foreign = client
        .get(format!(
            "{}/v1/tasks/{id}/events/stream?after=0",
            stack.gateway_base
        ))
        .bearer_auth(OTHER_CALLER_TOKEN)
        .send()
        .unwrap();
    assert_eq!(foreign.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(error_code(foreign), "route.task.not_found");

    // A client error is not an unreachable Worker.
    let projection = stack.task(GATEWAY_TOKEN, &id);
    assert_eq!(projection["connectivity"], "online");
}

#[test]
fn gateway_forwards_expired_retention_guidance() {
    let stack = Stack::start_with_worker_env(&[("GOLDENBEE_EVENT_RETENTION", "2".into())]);
    let id = submit(&stack, GATEWAY_TOKEN, "trimmed gateway", "gw-trimmed");
    stack.wait_task(GATEWAY_TOKEN, &id);

    let expired = Stack::client()
        .get(format!(
            "{}/v1/tasks/{id}/events/stream?after=0",
            stack.gateway_base
        ))
        .bearer_auth(GATEWAY_TOKEN)
        .send()
        .unwrap();
    assert_eq!(expired.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(error_code(expired), "stream.cursor_expired");
}

#[test]
fn gateway_secret_canary_is_redacted_over_public_sse() {
    let stack = Stack::start();
    let id = submit(
        &stack,
        GATEWAY_TOKEN,
        "delay:300 echo sk-gateway-secret",
        "gw-secret",
    );
    let events = stack.sse(GATEWAY_TOKEN, &id, 0).until_terminal();
    let encoded = serde_json::to_string(&events).expect("events serialize");
    assert!(
        !encoded.contains("sk-gateway-secret"),
        "secret leaked through the Gateway stream: {encoded}"
    );
    assert!(events.iter().any(|event| event.kind == "task.succeeded"));
}

#[test]
fn gateway_bounds_its_own_forwarded_streams() {
    let stack = Stack::start_with_options(Options {
        gateway_max_streams: Some(1),
        ..Options::default()
    });
    let id = submit(&stack, GATEWAY_TOKEN, "delay:1500 gateway held", "gw-held");
    let held = stack.stream_raw(GATEWAY_TOKEN, &id, 0);
    assert_eq!(held.status(), reqwest::StatusCode::OK);

    let second = stack.stream_raw(GATEWAY_TOKEN, &id, 0);
    assert_eq!(second.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_code(second), "stream.subscriber_limit");

    drop(held);
    let mut released = false;
    for _ in 0..100 {
        let response = stack.stream_raw(GATEWAY_TOKEN, &id, 0);
        if response.status() == reqwest::StatusCode::OK {
            released = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(released, "Gateway stream slot was not released");
}
