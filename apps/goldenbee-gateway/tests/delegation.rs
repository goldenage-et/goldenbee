//! Delegation narrowing: Gateway grants one capability; Worker enforces it.

mod support;

use common::delegation::{issue_delegation, DelegationClaims};
use common::{Limits, SubmitTaskRequest};
use serde_json::Value;
use support::{Stack, DELEGATION_SECRET, GATEWAY_TOKEN};

fn delegation(instance: &str, capability: &str) -> String {
    issue_delegation(
        DELEGATION_SECRET.as_bytes(),
        &DelegationClaims {
            issuer: "test-gateway".into(),
            audience: instance.into(),
            principal: "caller-1".into(),
            namespace: "local".into(),
            scopes: vec!["task:submit".into(), "task:read".into()],
            capabilities: vec![capability.into()],
            expires_at: common::now_ms() + 30_000,
            depth: 0,
            max_depth: 2,
            limits: Limits::default(),
            token_id: "delegation-test".into(),
            team: None,
            path: Vec::new(),
        },
    )
    .expect("test delegation issues")
}

fn direct(worker: &support::Worker, token: &str, agent: &str) -> reqwest::blocking::Response {
    support::Worker::client()
        .post(format!("{}/v1/tasks", worker.base))
        .bearer_auth(token)
        .json(&SubmitTaskRequest {
            namespace: None,
            agent: agent.into(),
            session_id: "delegated".into(),
            input: "delegated call".into(),
            idempotency_key: format!("delegated-{agent}"),
            profile: None,
            team: None,
            worker: None,
            limits: Limits::default(),
        })
        .send()
        .expect("delegated request responds")
}

#[test]
fn gateway_narrows_delegation_and_worker_enforces_it_independently() {
    let stack = Stack::start();

    let accepted: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "echo",
            "gateway delegated echo",
            "delegated-session",
            "delegated-gateway",
        )
        .json()
        .expect("gateway submission is JSON");
    assert_eq!(accepted["agent"], "echo");
    let task_id = accepted["task_id"].as_str().unwrap();
    let routed = stack.wait_task(GATEWAY_TOKEN, task_id);
    assert_eq!(routed["task"]["agent"], "echo");
    let worker_task_id = routed["task"]["id"].as_str().expect("worker task id");
    assert_eq!(
        stack.worker_a.task(worker_task_id).agent,
        "echo",
        "Worker stores only the capability the Gateway delegated"
    );

    // A valid signature for the right Worker that does not grant the requested
    // capability is rejected by the Worker itself, not only by the Gateway.
    let wrong_capability = direct(
        &stack.worker_a,
        &delegation(&stack.worker_a.instance, "echo"),
        "general",
    );
    assert_eq!(wrong_capability.status(), reqwest::StatusCode::FORBIDDEN);
    let body: Value = wrong_capability.json().unwrap();
    assert_eq!(body["code"], "scope.capability_denied");

    let wrong_audience = direct(
        &stack.worker_a,
        &delegation("worker-somewhere-else", "general"),
        "general",
    );
    assert_eq!(wrong_audience.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = wrong_audience.json().unwrap();
    assert_eq!(body["code"], "delegation.wrong_audience");

    assert!(!body.to_string().contains(DELEGATION_SECRET));
}
