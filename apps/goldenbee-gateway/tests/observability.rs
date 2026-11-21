//! Ticket 48: correlate one distributed execution and drill actionable failure
//! evidence through public SDK and Worker contracts.

mod support;

use common::delegation::{issue_delegation, DelegationClaims};
use common::Limits;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use support::{Options, Stack, ADMIN_TOKEN, DELEGATION_SECRET};

const TRACE: &str = "0123456789abcdef0123456789abcdef";
const CANARY: &str = "sk-observability-canary-do-not-export";

fn post(stack: &Stack, token: &str, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
    let response = Stack::client()
        .post(format!("{}{path}", stack.gateway_base))
        .bearer_auth(token)
        .json(&body)
        .send()
        .expect("POST responds");
    let status = response.status();
    let body = response.text().unwrap_or_default();
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body})),
    )
}

fn submit_traced(stack: &Stack, token: &str, body: Value) -> (reqwest::StatusCode, Value) {
    let response = Stack::client()
        .post(format!("{}/v1/tasks", stack.gateway_base))
        .bearer_auth(token)
        .header("traceparent", format!("00-{TRACE}-0000000000000001-01"))
        .json(&body)
        .send()
        .expect("submit responds");
    let status = response.status();
    let body = response.text().unwrap_or_default();
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body})),
    )
}

fn worker_post(worker: &support::Worker, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
    let response = Stack::client()
        .post(format!("{}{path}", worker.base))
        .bearer_auth(&worker.token)
        .json(&body)
        .send()
        .expect("Worker POST responds");
    let status = response.status();
    let body = response.text().unwrap_or_default();
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body})),
    )
}

fn worker_get(worker: &support::Worker, path: &str) -> (reqwest::StatusCode, Value) {
    let response = Stack::client()
        .get(format!("{}{path}", worker.base))
        .bearer_auth(&worker.token)
        .send()
        .expect("Worker GET responds");
    let status = response.status();
    let body = response.text().unwrap_or_default();
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body})),
    )
}

fn wait_plan(worker: &support::Worker, id: &str, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (status, plan) = worker_get(worker, &format!("/v1/plans/{id}"));
        assert!(status.is_success(), "{plan}");
        if predicate(&plan) {
            return plan;
        }
        assert!(Instant::now() < deadline, "plan did not converge: {plan}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn member(worker: &str, capability: &str) -> Value {
    json!({
        "worker": worker,
        "role": if worker == "worker-a" { "master" } else { "specialist" },
        "capabilities": [capability],
        "max_depth": 2
    })
}

fn delegation(instance: &str, principal: &str, capability: &str) -> String {
    issue_delegation(
        DELEGATION_SECRET.as_bytes(),
        &DelegationClaims {
            issuer: "ticket-48-test".into(),
            audience: instance.into(),
            principal: principal.into(),
            namespace: "local".into(),
            scopes: vec!["*".into()],
            capabilities: vec![capability.into()],
            expires_at: common::now_ms() + 120_000,
            depth: 0,
            max_depth: 3,
            limits: Limits::default(),
            token_id: format!("ticket48-{instance}-{}", common::now_ms()),
            team: None,
            path: Vec::new(),
        },
    )
    .expect("delegation issues")
}

fn a2a(worker: &support::Worker, token: &str, method: &str) -> Value {
    let response = Stack::client()
        .post(format!("{}/a2a", worker.base))
        .bearer_auth(token)
        .header("A2A-Version", "1.0")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}}))
        .send()
        .expect("A2A responds");
    response.json::<Value>().expect("A2A JSON")
}

#[test]
fn distributed_execution_is_correlated_and_diagnostics_drill_unavailable_child() {
    let mut stack = Stack::start_with_options(Options {
        with_echo_worker: true,
        ..Options::default()
    });
    let (status, team) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams",
        json!({
            "name": "observe",
            "master": "worker-a",
            "members": [
                member("worker-a", "general"),
                member("worker-b", "general")
            ],
            "limits": {"max_depth": 2, "max_fanout": 2}
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{team}");
    let (status, activated) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams/observe/activate",
        json!({"version": 1}),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{activated}");

    let (status, accepted) = submit_traced(
        &stack,
        ADMIN_TOKEN,
        json!({
            "team": "observe",
            "team_version": 1,
            "agent": "general",
            "session_id": "observability-session",
            "input": format!("correlated execution {CANARY}"),
            "idempotency_key": "observability-team"
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{accepted}");
    let execution = accepted["task_id"].as_str().expect("execution");
    let projection = stack.wait_task(ADMIN_TOKEN, execution);
    let master_task = projection["task"]["id"]
        .as_str()
        .expect("master task")
        .to_string();
    assert_eq!(projection["task"]["trace_id"], TRACE);
    assert_eq!(projection["task"]["traffic"], "product");

    let (status, created) = worker_post(
        &stack.worker_a,
        "/v1/plans",
        json!({
            "parent_task_id": master_task,
            "objective": "correlated child execution",
            "criteria": [
                {"id": "child-output", "child": "child",
                 "evidence": {"kind": "output_contains", "text": "trace evidence"}},
                {"id": "child-artifact", "child": "child",
                 "evidence": {"kind": "artifact_digest"}}
            ],
            "children": [
                {"id": "child", "member": "worker-b", "capability": "general",
                 "objective": "artifact:trace evidence"}
            ]
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{created}");
    let plan_id = created["id"].as_str().unwrap().to_string();
    let (status, _) = worker_post(
        &stack.worker_a,
        &format!("/v1/plans/{plan_id}/dispatch"),
        json!({"wait_ms": 15000}),
    );
    assert_eq!(status, reqwest::StatusCode::OK);
    let plan = wait_plan(&stack.worker_a, &plan_id, |plan| {
        plan["state"] == "verified"
    });
    assert_eq!(plan["state"], "verified", "{plan}");
    assert!(plan["children"][0]["artifacts"][0]["digest"]
        .as_str()
        .is_some_and(|digest| digest.starts_with("sha256:")));

    // The child native A2A task retains the propagated trace and parent
    // correlation.
    let token = delegation(&stack.worker_b.instance, "admin", "general");
    let tasks = a2a(&stack.worker_b, &token, "ListTasks");
    let child = &tasks["result"][0];
    assert_eq!(child["metadata"]["goldenbee.trace_id"], TRACE);
    assert_eq!(child["metadata"]["goldenbee.parent_task"], master_task);

    // Retained task events carry the same trace at every step.
    let (status, page) = worker_get(
        &stack.worker_a,
        &format!("/v1/tasks/{master_task}/events?after=0&limit=100"),
    );
    assert_eq!(status, reqwest::StatusCode::OK);
    let events = page["events"].as_array().expect("events");
    assert!(!events.is_empty());
    assert!(
        events.iter().all(|event| event["trace_id"] == TRACE),
        "{page}"
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = sdk::Client::new(&stack.gateway_base, ADMIN_TOKEN);
    let bundle = runtime
        .block_on(client.diagnostics("worker-a", Some(&master_task)))
        .expect("SDK diagnostics");
    assert_eq!(bundle.correlation.trace_id.as_deref(), Some(TRACE));
    assert_eq!(
        bundle.correlation.task_id.as_deref(),
        Some(master_task.as_str())
    );
    assert_eq!(bundle.task.as_ref().unwrap().traffic, "product");
    assert!(bundle
        .audit
        .iter()
        .any(|record| record.action == "task.submit" && record.trace_id.as_deref() == Some(TRACE)));
    // Provider-confirmed counts stay separate from locally estimated and
    // explicitly unknown values; none is serialized as zero.
    assert!(bundle.metrics.iter().any(|metric| {
        metric.name == "tasks.usage.prompt_tokens"
            && metric.quality == common::MetricQuality::ProviderConfirmed
            && metric.value.is_some()
    }));
    assert!(bundle.metrics.iter().any(|metric| {
        metric.name == "tasks.usage.estimated_tokens"
            && metric.quality != common::MetricQuality::ProviderConfirmed
    }));
    for metric in &bundle.metrics {
        for key in metric.labels.keys() {
            assert!(
                common::is_bounded_label(key),
                "unbounded metric label {key}"
            );
        }
    }
    let diagnostic_text = serde_json::to_string(&bundle).unwrap();
    assert!(!diagnostic_text.contains(CANARY));
    assert!(!diagnostic_text.contains("observability-session"));

    // A tool-using execution keeps the same trace through native invocation,
    // retained tool events, and artifact metadata.
    let (status, accepted) = submit_traced(
        &stack,
        ADMIN_TOKEN,
        json!({
            "worker": "worker-a",
            "agent": "general",
            "session_id": "observability-artifact",
            "input": "artifact:trace evidence",
            "idempotency_key": "observability-artifact"
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{accepted}");
    let execution = accepted["task_id"].as_str().expect("artifact execution");
    let projection = stack.wait_task(ADMIN_TOKEN, execution);
    let worker_task = projection["task"]["id"].as_str().expect("worker task");
    assert_eq!(projection["task"]["trace_id"], TRACE);
    assert!(projection["task"]["native_invocation_id"].is_string());
    let (status, page) = worker_get(
        &stack.worker_a,
        &format!("/v1/tasks/{worker_task}/events?after=0&limit=200"),
    );
    assert_eq!(status, reqwest::StatusCode::OK);
    let events = page["events"].as_array().expect("artifact events");
    assert!(events
        .iter()
        .any(|event| event["kind"] == "tool.call" && event["trace_id"] == TRACE));
    let (status, artifacts) = worker_get(
        &stack.worker_a,
        &format!("/v1/tasks/{worker_task}/artifacts"),
    );
    assert_eq!(status, reqwest::StatusCode::OK);
    let artifact = artifacts
        .as_array()
        .and_then(|artifacts| artifacts.first())
        .expect("artifact metadata");
    assert_eq!(artifact["trace_id"], TRACE);

    // Stop the child, then drill the retained partial plan through the same
    // SDK diagnostic surface.
    stack.worker_b.stop();
    let (status, second) = worker_post(
        &stack.worker_a,
        "/v1/plans",
        json!({
            "parent_task_id": master_task,
            "objective": "unavailable child drill",
            "criteria": [
                {"id": "missing", "child": "child",
                 "evidence": {"kind": "output_contains", "text": "never"}}
            ],
            "children": [
                {"id": "child", "member": "worker-b", "capability": "general",
                 "objective": "unavailable"}
            ]
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{second}");
    let second_id = second["id"].as_str().unwrap().to_string();
    let (status, _) = worker_post(
        &stack.worker_a,
        &format!("/v1/plans/{second_id}/dispatch"),
        json!({"wait_ms": 2000}),
    );
    assert_eq!(status, reqwest::StatusCode::OK);
    let second_plan = wait_plan(&stack.worker_a, &second_id, |plan| {
        plan["children"][0]["state"] == "unknown"
            || plan["children"][0]["last_error"]["code"] == "peer.unreachable"
    });
    let bundle = runtime
        .block_on(client.diagnostics("worker-a", Some(&master_task)))
        .expect("SDK diagnostics after child loss");
    assert!(
        bundle
            .findings
            .iter()
            .any(|finding| finding.kind == "unreachable_child"),
        "plan={second_plan} task={:?} findings={:?}",
        bundle.task,
        bundle.findings
    );
}
