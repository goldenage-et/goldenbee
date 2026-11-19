//! Ticket 22 Gateway-unavailable continuity evidence. A team execution reaches
//! its ordinary Master Worker through Gateway, then Gateway is killed. The
//! Master continues to coordinate its frozen roster over native A2A, evaluates
//! observed artifact evidence, and denies delegation outside the frozen
//! authority.

mod support;

use common::delegation::{issue_delegation, DelegationClaims};
use common::Limits;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use support::{Options, Stack, ADMIN_TOKEN, DELEGATION_SECRET};

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
            issuer: "ticket-22-test".into(),
            audience: instance.into(),
            principal: principal.into(),
            namespace: "local".into(),
            scopes: vec!["*".into()],
            capabilities: vec![capability.into()],
            expires_at: common::now_ms() + 120_000,
            depth: 0,
            max_depth: 3,
            limits: Limits::default(),
            token_id: format!("ticket22-{instance}-{}", common::now_ms()),
            team: None,
            path: Vec::new(),
        },
    )
    .expect("delegation issues")
}

fn a2a(
    worker: &support::Worker,
    token: &str,
    method: &str,
    params: Value,
) -> (reqwest::StatusCode, Value) {
    let response = Stack::client()
        .post(format!("{}/a2a", worker.base))
        .bearer_auth(token)
        .header("A2A-Version", "1.0")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
        .send()
        .expect("A2A responds");
    let status = response.status();
    let body = response.text().unwrap_or_default();
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body})),
    )
}

fn remote_tasks(worker: &support::Worker, token: &str) -> Vec<Value> {
    let (status, body) = a2a(worker, token, "ListTasks", json!({}));
    assert!(status.is_success(), "{body}");
    body["result"].as_array().cloned().unwrap_or_default()
}

fn child<'a>(plan: &'a Value, id: &str) -> &'a Value {
    plan["children"]
        .as_array()
        .expect("plan children")
        .iter()
        .find(|child| child["id"] == id)
        .unwrap_or_else(|| panic!("child '{id}': {plan}"))
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

#[test]
fn team_master_continues_and_verifies_after_gateway_is_gone() {
    let mut stack = Stack::start_with_options(Options {
        with_echo_worker: true,
        ..Options::default()
    });
    let (status, team) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams",
        json!({
            "name": "recovery",
            "master": "worker-a",
            "members": [
                member("worker-a", "general"),
                member("worker-b", "general"),
                member("worker-c", "echo")
            ],
            "limits": {"max_depth": 2, "max_fanout": 3}
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{team}");
    let (status, activated) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams/recovery/activate",
        json!({"version": 1}),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{activated}");
    let (status, accepted) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/tasks",
        json!({
            "team": "recovery",
            "team_version": 1,
            "agent": "general",
            "session_id": "gateway-gone-session",
            "input": "coordinate after gateway loss",
            "idempotency_key": "gateway-gone"
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{accepted}");
    let execution = accepted["task_id"].as_str().expect("execution id");
    let projection = stack.wait_task(ADMIN_TOKEN, execution);
    let master_task = projection["task"]["id"]
        .as_str()
        .expect("Master task id")
        .to_string();

    // Gateway is no longer a dependency of Master execution or recovery.
    stack.kill_gateway();

    let (status, created) = worker_post(
        &stack.worker_a,
        "/v1/plans",
        json!({
            "parent_task_id": master_task,
            "objective": "aggregate independent package evidence without Gateway",
            "criteria": [
                {"id": "general-output", "child": "general-child",
                 "evidence": {"kind": "output_contains", "text": "gateway independent general"}},
                {"id": "general-artifact", "child": "general-child",
                 "evidence": {"kind": "artifact_digest"}},
                {"id": "echo-output", "child": "echo-child",
                 "evidence": {"kind": "output_contains", "text": "gateway independent echo"}},
                {"id": "echo-artifact", "child": "echo-child",
                 "evidence": {"kind": "artifact_digest"}}
            ],
            "children": [
                {"id": "general-child", "member": "worker-b", "capability": "general",
                 "objective": "gateway independent general"},
                {"id": "echo-child", "member": "worker-c", "capability": "echo",
                 "objective": "gateway independent echo"}
            ]
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{created}");
    assert_eq!(created["team"], "recovery");
    assert_eq!(created["team_version"], 1);
    assert_eq!(created["provenance"]["instance"], stack.worker_a.instance);
    assert_eq!(created["provenance"]["package"], "general");
    assert_eq!(created["provenance"]["profile"], "default");
    let id = created["id"].as_str().unwrap().to_string();
    let (status, _) = worker_post(
        &stack.worker_a,
        &format!("/v1/plans/{id}/dispatch"),
        json!({"wait_ms": 15000}),
    );
    assert_eq!(status, reqwest::StatusCode::OK);
    let plan = wait_plan(&stack.worker_a, &id, |plan| {
        plan["state"] == "verified"
            && plan["children"]
                .as_array()
                .is_some_and(|children| children.iter().all(|child| child["state"] == "completed"))
    });
    assert_eq!(plan["state"], "verified", "{plan}");
    assert_eq!(plan["team_version"], 1);
    assert_eq!(child(&plan, "general-child")["member"], "worker-b");
    assert_eq!(child(&plan, "echo-child")["member"], "worker-c");

    // The independently built echo package executed the second child, and both
    // child tasks retain causal parent correlation.
    let worker_c = stack.worker_c.as_ref().expect("independent echo Worker");
    let (status, caps) = worker_get(worker_c, "/v1/capabilities");
    assert_eq!(status, reqwest::StatusCode::OK, "{caps}");
    assert_eq!(caps["package"], "echo", "{caps}");
    let general_token = delegation(&stack.worker_b.instance, "admin", "general");
    let echo_token = delegation(&worker_c.instance, "admin", "echo");
    let general_tasks = remote_tasks(&stack.worker_b, &general_token);
    let echo_tasks = remote_tasks(worker_c, &echo_token);
    assert_eq!(general_tasks.len(), 1);
    assert_eq!(echo_tasks.len(), 1);
    for (tasks, parent_instance) in [
        (&general_tasks, stack.worker_a.instance.as_str()),
        (&echo_tasks, stack.worker_a.instance.as_str()),
    ] {
        assert_eq!(
            tasks[0]["metadata"]["goldenbee.parent_task"],
            master_task.as_str()
        );
        assert_eq!(
            tasks[0]["metadata"]["goldenbee.parent_instance"],
            parent_instance
        );
        assert_eq!(tasks[0]["metadata"]["goldenbee.team"], "recovery");
        assert_eq!(tasks[0]["metadata"]["goldenbee.team_version"], 1);
    }
    assert!(child(&plan, "echo-child")["artifacts"][0]["digest"]
        .as_str()
        .is_some_and(|digest| digest.starts_with("sha256:")));

    // A current Gateway registry entry outside the frozen roster cannot be
    // selected, and a member cannot gain a capability it was not granted.
    let (status, hidden) = worker_post(
        &stack.worker_a,
        "/v1/plans",
        json!({
            "parent_task_id": master_task,
            "objective": "attempt delegation outside the frozen roster",
            "criteria": [
                {"id": "hidden", "child": "hidden-child",
                 "evidence": {"kind": "output_contains", "text": "hidden"}}
            ],
            "children": [
                {"id": "hidden-child", "member": "worker-down", "capability": "general",
                 "objective": "hidden"}
            ]
        }),
    );
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "{hidden}");
    assert_eq!(hidden["code"], "team.member_hidden", "{hidden}");

    let (status, escalation) = worker_post(
        &stack.worker_a,
        "/v1/plans",
        json!({
            "parent_task_id": master_task,
            "objective": "attempt capability escalation",
            "criteria": [
                {"id": "echo", "child": "general-child",
                 "evidence": {"kind": "output_contains", "text": "echo"}}
            ],
            "children": [
                {"id": "general-child", "member": "worker-b", "capability": "echo",
                 "objective": "echo"}
            ]
        }),
    );
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "{escalation}");
    assert_eq!(escalation["code"], "peer.capability_denied", "{escalation}");
}
