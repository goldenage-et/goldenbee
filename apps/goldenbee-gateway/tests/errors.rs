//! Stable Gateway errors for unsupported, unauthorized, and unavailable targets.

mod support;

use serde_json::{json, Value};
use support::{Stack, GATEWAY_TOKEN, OTHER_CALLER_TOKEN};

fn error(response: reqwest::blocking::Response) -> (reqwest::StatusCode, Value) {
    let status = response.status();
    let body = response.json::<Value>().expect("error body is JSON");
    (status, body)
}

#[test]
fn invalid_or_unauthorized_targets_return_stable_errors_without_fallback() {
    let stack = Stack::start();
    let client = Stack::client();

    let (status, body) = error(
        client
            .post(format!("{}/v1/tasks", stack.gateway_base))
            .json(&json!({
                "worker": "worker-a",
                "agent": "general",
                "session_id": "s",
                "input": "no token",
                "idempotency_key": "unauth"
            }))
            .send()
            .unwrap(),
    );
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "auth.unauthenticated");
    assert!(!body.to_string().contains(GATEWAY_TOKEN));

    let (status, body) = error(
        client
            .post(format!("{}/v1/tasks", stack.gateway_base))
            .bearer_auth(GATEWAY_TOKEN)
            .json(&json!({
                "agent": "general",
                "session_id": "s",
                "input": "missing target",
                "idempotency_key": "missing-target"
            }))
            .send()
            .unwrap(),
    );
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "route.worker_required");
    assert!(!body["recovery"].as_str().unwrap_or_default().is_empty());

    let (status, body) = error(stack.submit(
        GATEWAY_TOKEN,
        "worker-missing",
        "general",
        "unknown",
        "s",
        "unknown-worker",
    ));
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "route.worker.not_found");

    let (status, body) = error(stack.submit(
        GATEWAY_TOKEN,
        "worker-b",
        "general",
        "denied target",
        "s",
        "denied-worker",
    ));
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "scope.worker_denied");
    assert!(
        stack.worker_b.tasks().is_empty(),
        "denial must not fall back"
    );

    let (status, body) = error(stack.submit(
        GATEWAY_TOKEN,
        "worker-down",
        "general",
        "unreachable",
        "s",
        "unavailable-worker",
    ));
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "route.worker_unavailable");
    assert_eq!(body["retryable"], true);

    let (status, body) = error(stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "not-a-capability",
        "mismatch",
        "s",
        "capability-mismatch",
    ));
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "scope.capability_denied");
    assert!(
        stack.worker_a.tasks().is_empty(),
        "denial must not dispatch"
    );

    let (status, body) = error(
        client
            .get(format!("{}/v1/tasks/task_absent", stack.gateway_base))
            .bearer_auth(GATEWAY_TOKEN)
            .send()
            .unwrap(),
    );
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "route.task.not_found");

    // Another authenticated caller may not read a task routed for caller-1.
    let accepted: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "owner-only",
            "s",
            "owner-only",
        )
        .json()
        .unwrap();
    let task_id = accepted["task_id"].as_str().unwrap();
    let (status, body) = error(
        client
            .get(format!("{}/v1/tasks/{task_id}", stack.gateway_base))
            .bearer_auth(OTHER_CALLER_TOKEN)
            .send()
            .unwrap(),
    );
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "route.task.not_found");

    for token in [GATEWAY_TOKEN, OTHER_CALLER_TOKEN] {
        assert!(!body.to_string().contains(token));
    }
}

#[test]
fn configured_worker_instance_identity_is_confirmed_before_dispatch() {
    let stack = Stack::start_with_instance_mismatch();
    let (status, body) = error(stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "identity mismatch",
        "s",
        "identity-mismatch",
    ));
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "route.worker_unavailable");
    assert!(
        stack.worker_a.tasks().is_empty(),
        "an identity-mismatched endpoint must not receive the task"
    );

    let health: Value = Stack::client()
        .get(format!("{}/v1/health", stack.gateway_base))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(health["workers"], 3);
    assert_eq!(
        health["reachable"], 1,
        "only worker-b keeps a confirmed identity"
    );
}
