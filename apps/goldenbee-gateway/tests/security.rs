//! Ticket 47 cross-process security acceptance through the public Gateway and
//! Worker surfaces.
//!
//! These tests start real `gateway`, `general`, and (for failure drills) the
//! same Worker fixtures used by the per-feature suites. They assert external
//! outcomes only: task and artifact identities, denial codes, observed Worker
//! task counts, and redacted error bodies. Per-feature tests remain
//! authoritative for their detailed contracts; this suite proves the combined
//! invariants across process boundaries.

mod support;

use common::delegation::{issue_delegation, DelegationClaims};
use common::{Artifact, Limits, SubmitTaskRequest};
use serde_json::Value;
use support::{
    Options, Stack, DELEGATION_SECRET, GATEWAY_TOKEN, OTHER_CALLER_TOKEN, WORKER_A_TOKEN,
};

fn client() -> reqwest::blocking::Client {
    Stack::client()
}

fn body(response: reqwest::blocking::Response) -> (reqwest::StatusCode, String) {
    let status = response.status();
    (status, response.text().unwrap_or_default())
}

fn json_body(response: reqwest::blocking::Response) -> (reqwest::StatusCode, Value) {
    let (status, text) = body(response);
    (
        status,
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text})),
    )
}

fn report(step: &str, detail: impl std::fmt::Display) {
    // Deterministic acceptance trace; run with `--nocapture` when collecting
    // evidence. It carries counts and identities, never credentials.
    println!("security-47: {step}: {detail}");
}

fn assert_redacted(text: &str, canaries: &[&str]) {
    for canary in canaries {
        assert!(
            !text.contains(canary),
            "public evidence leaked a protected value: {canary}"
        );
    }
}

fn delegate(instance: &str, namespace: &str, capability: &str, expires_at: i64) -> String {
    issue_delegation(
        DELEGATION_SECRET.as_bytes(),
        &DelegationClaims {
            issuer: "ticket-47-security".into(),
            audience: instance.into(),
            principal: "foreign-caller".into(),
            namespace: namespace.into(),
            scopes: vec!["task:submit".into(), "task:read".into()],
            capabilities: vec![capability.into()],
            expires_at,
            depth: 0,
            max_depth: 2,
            limits: Limits::default(),
            token_id: format!("ticket47-{instance}-{expires_at}"),
            team: None,
            path: Vec::new(),
        },
    )
    .expect("delegation issues")
}

fn direct_task(worker: &support::Worker, token: Option<&str>) -> reqwest::blocking::Response {
    let request = SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: "general".into(),
        session_id: "security-direct".into(),
        input: "direct hostile request".into(),
        idempotency_key: "security-direct".into(),
        profile: None,
        team: None,
        worker: None,
        limits: Limits::default(),
    };
    let mut builder = client()
        .post(format!("{}/v1/tasks", worker.base))
        .json(&request);
    if let Some(token) = token {
        builder = builder.bearer_auth(token);
    }
    builder.send().expect("direct Worker submit responds")
}

fn worker_ids(stack: &Stack, token: &str) -> Vec<String> {
    let workers: Vec<Value> = client()
        .get(format!("{}/v1/workers", stack.gateway_base))
        .bearer_auth(token)
        .send()
        .expect("discovery responds")
        .json()
        .expect("discovery is JSON");
    workers
        .into_iter()
        .filter_map(|worker| worker["id"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn namespaces_cannot_confuse_tasks_artifacts_approvals_discovery_or_delegated_identity() {
    let stack = Stack::start();
    let accepted: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "artifact:namespace-a-secret",
            "namespace-a-session",
            "namespace-a-key",
        )
        .json()
        .expect("owning submission is JSON");
    let execution = accepted["task_id"].as_str().expect("execution id");
    assert!(execution.starts_with("exec-"), "{accepted}");
    let projection = stack.wait_task(GATEWAY_TOKEN, execution);
    assert_eq!(projection["task"]["state"], "succeeded", "{projection}");
    let worker_task = projection["task"]["id"]
        .as_str()
        .expect("Worker task id")
        .to_string();

    let artifacts: Vec<Artifact> = client()
        .get(format!(
            "{}/v1/tasks/{execution}/artifacts",
            stack.gateway_base
        ))
        .bearer_auth(GATEWAY_TOKEN)
        .send()
        .expect("artifact list responds")
        .json()
        .expect("artifact list is JSON");
    assert_eq!(artifacts.len(), 1, "{artifacts:?}");
    let artifact = &artifacts[0];
    assert_eq!(artifact.namespace, "local");
    assert_eq!(artifact.owner, "caller-1");
    assert_eq!(artifact.task_id, worker_task);
    let (status, content) = body(
        client()
            .get(format!(
                "{}/v1/tasks/{execution}/artifacts/{}/content",
                stack.gateway_base, artifact.id
            ))
            .bearer_auth(GATEWAY_TOKEN)
            .send()
            .expect("artifact content responds"),
    );
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(content.contains("namespace-a-secret"), "{content}");
    report(
        "owning namespace",
        format!(
            "execution={execution} worker_task={worker_task} artifacts={} digest={}",
            artifacts.len(),
            artifact.digest
        ),
    );

    // Another application namespace receives the same external denial on every
    // task-scoped surface. It must not receive content, a handle, or a decision.
    let paths = [
        format!("/v1/tasks/{execution}"),
        format!("/v1/tasks/{execution}/events"),
        format!("/v1/tasks/{execution}/approvals?pending=true"),
        format!("/v1/tasks/{execution}/artifacts"),
        format!("/v1/tasks/{execution}/artifacts/{}/content", artifact.id),
    ];
    for path in &paths {
        let (status, text) = body(
            client()
                .get(format!("{}{path}", stack.gateway_base))
                .bearer_auth(OTHER_CALLER_TOKEN)
                .send()
                .expect("foreign read responds"),
        );
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{path}: {text}");
        assert!(text.contains("route.task.not_found"), "{path}: {text}");
        assert_redacted(
            &text,
            &[
                "namespace-a-secret",
                GATEWAY_TOKEN,
                OTHER_CALLER_TOKEN,
                WORKER_A_TOKEN,
                DELEGATION_SECRET,
            ],
        );
    }
    let (status, text) = body(
        client()
            .post(format!(
                "{}/v1/tasks/{execution}/cancel",
                stack.gateway_base
            ))
            .bearer_auth(OTHER_CALLER_TOKEN)
            .send()
            .expect("foreign cancel responds"),
    );
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{text}");
    let stream = client()
        .get(format!(
            "{}/v1/tasks/{execution}/events/stream",
            stack.gateway_base
        ))
        .bearer_auth(OTHER_CALLER_TOKEN)
        .send()
        .expect("foreign stream responds");
    assert_eq!(stream.status(), reqwest::StatusCode::NOT_FOUND);

    // A same-named session and idempotency key in the other namespace is a
    // separate task with its own identity.
    let foreign: Value = stack
        .submit(
            OTHER_CALLER_TOKEN,
            "worker-b",
            "general",
            "artifact:tenant-b-secret",
            "namespace-a-session",
            "namespace-a-key",
        )
        .json()
        .expect("foreign submission is JSON");
    let foreign_execution = foreign["task_id"].as_str().expect("foreign execution id");
    assert_ne!(foreign_execution, execution);
    let foreign_projection = stack.wait_task(OTHER_CALLER_TOKEN, foreign_execution);
    assert_eq!(foreign_projection["task"]["state"], "succeeded");
    assert_eq!(stack.worker_a.tasks().len(), 1);
    assert_eq!(stack.worker_b.tasks_with("tenant-b-operator").len(), 1);
    let (status, text) = body(
        client()
            .get(format!(
                "{}/v1/tasks/{foreign_execution}",
                stack.gateway_base
            ))
            .bearer_auth(GATEWAY_TOKEN)
            .send()
            .expect("reverse read responds"),
    );
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{text}");
    assert_redacted(&text, &["tenant-b-secret"]);

    // Discovery is caller-scoped: the two namespaces never see each other's
    // Worker identities.
    let local = worker_ids(&stack, GATEWAY_TOKEN);
    let tenant = worker_ids(&stack, OTHER_CALLER_TOKEN);
    assert!(local.contains(&"worker-a".to_string()), "{local:?}");
    assert!(!local.contains(&"worker-b".to_string()), "{local:?}");
    assert!(tenant.contains(&"worker-b".to_string()), "{tenant:?}");
    assert!(!tenant.contains(&"worker-a".to_string()), "{tenant:?}");

    // A caller cannot widen routing by naming a Worker outside its allow-list.
    let (status, denied) = json_body(stack.submit(
        OTHER_CALLER_TOKEN,
        "worker-a",
        "general",
        "cross namespace dispatch",
        "namespace-b-session",
        "namespace-b-key",
    ));
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "{denied}");
    assert_eq!(denied["code"], "scope.worker_denied", "{denied}");
    assert_eq!(stack.worker_a.tasks().len(), 1, "denial must not dispatch");

    // Delegated identity is audience, namespace, scope, and expiry checked by
    // the Worker itself even when a caller bypasses the Gateway transport.
    let wrong_audience = delegate(
        "worker-elsewhere",
        "local",
        "general",
        common::now_ms() + 60_000,
    );
    let (status, text) = body(direct_task(&stack.worker_a, Some(&wrong_audience)));
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED, "{text}");
    assert!(text.contains("delegation.wrong_audience"), "{text}");
    let wrong_namespace = delegate(
        &stack.worker_a.instance,
        "tenant-b",
        "general",
        common::now_ms() + 60_000,
    );
    let (status, text) = body(direct_task(&stack.worker_a, Some(&wrong_namespace)));
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "{text}");
    assert!(text.contains("scope.namespace_denied"), "{text}");
    assert_eq!(
        stack.worker_a.tasks().len(),
        1,
        "foreign identity checks must not create a task"
    );

    let public = format!(
        "{}{}{}{}",
        stack.task(GATEWAY_TOKEN, execution),
        stack.events(GATEWAY_TOKEN, execution),
        serde_json::to_string(&artifacts).unwrap(),
        denied
    );
    assert_redacted(
        &public,
        &[
            GATEWAY_TOKEN,
            OTHER_CALLER_TOKEN,
            WORKER_A_TOKEN,
            DELEGATION_SECRET,
        ],
    );
}

#[test]
fn hostile_raw_requests_are_denied_by_gateway_and_worker_enforcement() {
    let stack = Stack::start();
    let request = serde_json::json!({
        "worker": "worker-a",
        "agent": "general",
        "session_id": "hostile",
        "input": "hostile",
        "idempotency_key": "hostile"
    });

    // No SDK or Client-side validation is involved: these are raw HTTP calls.
    let (status, text) = body(
        client()
            .post(format!("{}/v1/tasks", stack.gateway_base))
            .json(&request)
            .send()
            .expect("unauthenticated submit responds"),
    );
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED, "{text}");
    assert!(text.contains("auth.unauthenticated"), "{text}");

    let (status, text) = body(
        client()
            .post(format!("{}/v1/tasks", stack.gateway_base))
            .bearer_auth("not-a-caller-token")
            .json(&request)
            .send()
            .expect("unknown caller responds"),
    );
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED, "{text}");

    let (status, text) = body(
        client()
            .post(format!("{}/v1/tasks", stack.gateway_base))
            .bearer_auth(GATEWAY_TOKEN)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body("{this is not json")
            .send()
            .expect("malformed request responds"),
    );
    assert!(
        status.is_client_error(),
        "malformed JSON must be rejected: {status} {text}"
    );

    let (status, denied) = json_body(stack.submit(
        GATEWAY_TOKEN,
        "worker-b",
        "general",
        "forbidden target",
        "hostile-session",
        "hostile-target",
    ));
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "{denied}");
    assert_eq!(denied["code"], "scope.worker_denied", "{denied}");
    assert!(stack.worker_b.tasks().is_empty());

    // The Worker also rejects missing, unknown, and foreign local credentials.
    for token in [None, Some("not-a-worker-token"), Some(OTHER_CALLER_TOKEN)] {
        let (status, text) = body(direct_task(&stack.worker_a, token));
        assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED, "{text}");
        assert!(
            text.contains("auth.unauthenticated") || text.contains("delegation.invalid"),
            "{text}"
        );
    }

    let expired = delegate(
        &stack.worker_a.instance,
        "local",
        "general",
        common::now_ms() - 1_000,
    );
    let (status, text) = body(direct_task(&stack.worker_a, Some(&expired)));
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED, "{text}");
    assert!(text.contains("delegation.expired"), "{text}");

    let narrowed = delegate(
        &stack.worker_a.instance,
        "local",
        "echo",
        common::now_ms() + 60_000,
    );
    let (status, text) = body(direct_task(&stack.worker_a, Some(&narrowed)));
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "{text}");
    assert!(text.contains("scope.capability_denied"), "{text}");

    assert!(
        stack.worker_a.tasks().is_empty(),
        "every hostile request must be denied before a task is recorded"
    );
    assert!(stack.worker_b.tasks().is_empty());
    assert_redacted(
        &format!("{text}{denied}"),
        &[
            GATEWAY_TOKEN,
            OTHER_CALLER_TOKEN,
            WORKER_A_TOKEN,
            DELEGATION_SECRET,
        ],
    );
}

#[test]
fn partition_provider_expiry_and_storage_failures_are_explicit_and_bounded() {
    let faults = tempfile::tempdir().expect("fault directory");
    let mut stack = Stack::start_with_options(Options {
        gateway_faults: Some(faults.path().to_path_buf()),
        ..Options::default()
    });

    let normal: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "normal bounded work",
            "bounded-normal-session",
            "bounded-normal",
        )
        .json()
        .expect("normal submission is JSON");
    let normal_id = normal["task_id"].as_str().expect("normal execution id");
    let normal = stack.wait_task(GATEWAY_TOKEN, normal_id);
    assert_eq!(normal["task"]["state"], "succeeded");

    // Unreachable target: explicit, retryable, and no fallback dispatch.
    let (status, unavailable) = json_body(stack.submit(
        GATEWAY_TOKEN,
        "worker-down",
        "general",
        "partitioned target",
        "bounded-partition-session",
        "bounded-partition",
    ));
    assert_eq!(
        status,
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "{unavailable}"
    );
    assert_eq!(
        unavailable["code"], "route.worker_unavailable",
        "{unavailable}"
    );
    assert_eq!(unavailable["retryable"], true);

    // Provider failure affects only its task and leaves readiness unchanged.
    let failed: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "provider-fail",
            "bounded-provider-session",
            "bounded-provider",
        )
        .json()
        .expect("provider submission is JSON");
    let failed_id = failed["task_id"].as_str().expect("provider execution id");
    let failed = stack.wait_task(GATEWAY_TOKEN, failed_id);
    assert_eq!(failed["task"]["state"], "failed", "{failed}");
    assert_eq!(stack.worker_a.status()["phase"], "ready");
    let after_provider: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "work after provider failure",
            "bounded-provider-after-session",
            "bounded-provider-after",
        )
        .json()
        .expect("post-provider submission is JSON");
    let after_id = after_provider["task_id"]
        .as_str()
        .expect("post-provider id");
    assert_eq!(
        stack.wait_task(GATEWAY_TOKEN, after_id)["task"]["state"],
        "succeeded"
    );

    // Durable acceptance failure is not reported as accepted and does not
    // dispatch. The same key then admits exactly one task.
    let before = stack.worker_a.tasks().len();
    std::fs::write(faults.path().join("persist-failure"), b"once").expect("fault marker");
    let (status, storage) = json_body(stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "storage failure",
        "bounded-storage-session",
        "bounded-storage",
    ));
    assert_eq!(
        status,
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "{storage}"
    );
    assert_eq!(storage["code"], "storage.unavailable", "{storage}");
    assert_eq!(stack.worker_a.tasks().len(), before);
    let recovered: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "storage failure",
            "bounded-storage-session",
            "bounded-storage",
        )
        .json()
        .expect("storage retry is JSON");
    assert_eq!(recovered["replayed"], false);
    assert_eq!(stack.worker_a.tasks().len(), before + 1);

    // Expired delegated authority is denied without recording a task.
    let expired = delegate(
        &stack.worker_a.instance,
        "local",
        "general",
        common::now_ms() - 1_000,
    );
    let direct_before = stack.worker_a.tasks().len();
    let (status, text) = body(direct_task(&stack.worker_a, Some(&expired)));
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED, "{text}");
    assert!(text.contains("delegation.expired"), "{text}");
    assert_eq!(stack.worker_a.tasks().len(), direct_before);

    // A real partition of one Worker does not block unrelated authorized work
    // through another Worker.
    stack.stop_worker_a();
    let (status, text) = body(stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "partitioned worker",
        "bounded-down-session",
        "bounded-down",
    ));
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE, "{text}");
    let unrelated: Value = stack
        .submit(
            OTHER_CALLER_TOKEN,
            "worker-b",
            "general",
            "unrelated authorized work",
            "bounded-other-session",
            "bounded-other",
        )
        .json()
        .expect("unrelated submission is JSON");
    let unrelated_id = unrelated["task_id"].as_str().expect("unrelated id");
    assert_eq!(
        stack.wait_task(OTHER_CALLER_TOKEN, unrelated_id)["task"]["state"],
        "succeeded"
    );
    report(
        "failure bounds",
        format!(
            "worker_tasks={} provider_state={} storage_recovered={} unrelated_state=succeeded",
            before + 1,
            failed["task"]["state"],
            recovered["task_id"]
        ),
    );
}

#[test]
fn forced_gateway_termination_preserves_acceptance_identity_and_dispatch_count() {
    let faults = tempfile::tempdir().expect("fault directory");
    let mut stack = Stack::start_with_options(Options {
        gateway_faults: Some(faults.path().to_path_buf()),
        // Bound the unknown-outcome reconciliation window so the crash drill
        // stays deterministic instead of waiting for the production default.
        gateway_claim_ttl_ms: Some(250),
        ..Options::default()
    });
    std::fs::write(faults.path().join("after-acceptance"), b"once").expect("fault marker");

    let _ = stack.try_submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "accepted before mapping",
        "crash-accept-session",
        "crash-accept",
    );
    stack.wait_for_gateway_fault_exit();
    let accepted_tasks = stack.worker_a.tasks();
    assert_eq!(
        accepted_tasks.len(),
        1,
        "the Worker accepted before the Gateway stopped"
    );
    let accepted_worker_task = accepted_tasks[0].id.clone();
    assert_eq!(accepted_tasks[0].attempt, 1);

    stack.restart_gateway();
    let reconciled: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "accepted before mapping",
            "crash-accept-session",
            "crash-accept",
        )
        .json()
        .expect("reconciled submission is JSON");
    assert_eq!(reconciled["replayed"], true);
    let execution = reconciled["task_id"]
        .as_str()
        .expect("execution id")
        .to_string();
    assert!(execution.starts_with("exec-"), "{reconciled}");
    let projection = stack.wait_task(GATEWAY_TOKEN, &execution);
    assert_eq!(projection["task"]["id"], accepted_worker_task);
    assert_eq!(stack.worker_a.tasks().len(), 1);

    let replay: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "accepted before mapping",
            "crash-accept-session",
            "crash-accept",
        )
        .json()
        .expect("second replay is JSON");
    assert_eq!(replay["task_id"], execution);
    assert_eq!(stack.worker_a.tasks().len(), 1);

    // Crash after durable intent but before remote dispatch dispatches exactly
    // once after restart, with a fresh identity that is then stable.
    std::fs::write(faults.path().join("after-intent"), b"once").expect("fault marker");
    let _ = stack.try_submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "intent before dispatch",
        "crash-intent-session",
        "crash-intent",
    );
    stack.wait_for_gateway_fault_exit();
    assert_eq!(stack.worker_a.tasks().len(), 1);
    stack.restart_gateway();
    let fresh: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "intent before dispatch",
            "crash-intent-session",
            "crash-intent",
        )
        .json()
        .expect("fresh submission is JSON");
    let fresh_id = fresh["task_id"].as_str().expect("fresh execution id");
    assert_ne!(fresh_id, execution);
    let tasks = stack.worker_a.tasks();
    assert_eq!(tasks.len(), 2);
    assert!(tasks.iter().all(|task| task.attempt == 1));
    let mut ids = tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 2, "each accepted task has one identity");
    assert_eq!(
        stack.wait_task(GATEWAY_TOKEN, fresh_id)["task"]["id"],
        tasks
            .iter()
            .find(|task| task.id != accepted_worker_task)
            .expect("new Worker task")
            .id
    );
    report(
        "forced termination",
        format!(
            "execution={execution} accepted_worker_task={accepted_worker_task} worker_tasks={} attempts=1",
            tasks.len()
        ),
    );
}
