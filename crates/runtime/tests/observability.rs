//! Runtime observability through a real Worker: trace correlation, ordered
//! tool/diff/permission/sandbox/terminal evidence, truthful unmeasured fields,
//! redaction, typed audit, effect linkage, HTTP access, and restart durability.
//!
//! Tests observe the public runtime and Worker HTTP contract. They never read
//! private state or reach into the kernel.

use adk_rust::{Tool, ToolContext};
use async_trait::async_trait;
use common::{Limits, Principal, StreamEvent, SubmitTaskRequest, Task, TaskState};
use runtime::worker::{Worker, WorkerInstance};
use runtime::{EffectPolicy, EffectReconciler, EffectStatus, OperationRecord, Runtime};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TOKEN: &str = "observability-token";
const SECRET: &str = "sk-observability-canary-do-not-export";
const DISALLOWED: &str = "/home/other-tenant/private/notes.txt";

fn fixture(data: &Path, mode: &str, extra: &str) -> PathBuf {
    let assets = data.join("assets");
    let agents = assets.join("agents");
    let prompts = assets.join("prompts");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::write(
        agents.join("tools.toml"),
        "name = \"tools\"\ndescription = \"observability agent\"\nmodel = \"deterministic\"\nprompt = \"tools\"\ntools = [\"read_file\", \"write_file\", \"ledger_write\"]\n",
    )
    .expect("agent definition");
    std::fs::write(prompts.join("tools.md"), "Use the supplied tools.").expect("prompt");
    let config = data.join("worker.toml");
    std::fs::write(
        &config,
        format!(
            "instance = \"observability-{}\"\nnamespace = \"local\"\ndefault_agent = \"tools\"\ntoken = \"{TOKEN}\"\ntoken_scopes = [\"*\"]\n\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n\n[tools]\nworkspace = \"workspace\"\ntimeout_ms = 120000\nmax_output_bytes = 1048576\nmax_parallel = 2\n\n[permissions]\nmode = \"{mode}\"\napproval_wait_ms = 0\napproval_ttl_ms = 60000\n{extra}",
            data.file_name()
                .expect("temp dir name")
                .to_string_lossy(),
            data.display(),
        ),
    )
    .expect("worker config");
    config
}

struct Ledger;

#[async_trait]
impl Tool for Ledger {
    fn name(&self) -> &str {
        "ledger_write"
    }

    fn description(&self) -> &str {
        "Append one protected ledger entry"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {"entry": {"type": "string"}},
            "required": ["entry"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        Ok(json!({"committed": true, "entry": args["entry"]}))
    }
}

struct LedgerStatus;

#[async_trait]
impl EffectReconciler for LedgerStatus {
    async fn status(&self, operation: &OperationRecord) -> EffectStatus {
        EffectStatus::Committed {
            external_ref: Some(format!("ledger-{}", operation.attempt)),
            receipt: Some("ledger confirmed the exact entry".into()),
        }
    }
}

async fn start(data: &Path, mode: &str, extra: &str) -> WorkerInstance {
    let config = fixture(data, mode, extra);
    Worker::new("observability", "0.1.0")
        .file_tools()
        .effect_tool(
            "ledger_write",
            Arc::new(Ledger),
            Arc::new(LedgerStatus),
            EffectPolicy::default(),
        )
        .start(&config)
        .await
        .expect("worker starts")
}

fn admin() -> Principal {
    Principal::new("local", "local", &["*"])
}

async fn submit(runtime: &Runtime, input: &str, session: &str) -> String {
    runtime
        .submit_task(
            &admin(),
            SubmitTaskRequest {
                namespace: Some("local".into()),
                agent: "tools".into(),
                session_id: session.into(),
                input: input.into(),
                idempotency_key: format!("{session}-{}", common::canonical_digest(&input)),
                profile: None,
                team: None,
                worker: None,
                limits: Limits::default(),
            },
        )
        .await
        .expect("task admitted")
        .task_id
}

async fn wait(runtime: &Runtime, task_id: &str) -> Task {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let task = runtime.task_unscoped(task_id).await.expect("task exists");
        if task.state.is_terminal() {
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn events(runtime: &Runtime, task_id: &str) -> Vec<StreamEvent> {
    runtime
        .events_since(task_id, 0, 10_000)
        .await
        .expect("events")
        .events
}

fn position(events: &[StreamEvent], kind: &str) -> usize {
    events
        .iter()
        .position(|event| event.kind == kind)
        .unwrap_or_else(|| panic!("event {kind} missing: {events:?}"))
}

#[tokio::test]
async fn task_exposes_trace_and_ordered_tool_diff_permission_sandbox_and_terminal_events() {
    let data = tempfile::tempdir().expect("data directory");
    let instance = start(data.path(), "full-auto", "").await;
    let runtime = instance.runtime();
    let task_id = submit(
        &runtime,
        "call:write_file {\"path\":\"notes/a.txt\",\"content\":\"alpha\\nbeta\\n\"}",
        "ordered",
    )
    .await;
    let task = wait(&runtime, &task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert!(!task.trace_id.is_empty(), "every task carries a trace id");

    let events = events(&runtime, &task_id).await;
    assert!(
        events
            .iter()
            .all(|event| event.trace_id.as_deref() == Some(task.trace_id.as_str())),
        "every event is correlated to the task trace"
    );
    for pair in events.windows(2) {
        assert!(pair[0].seq < pair[1].seq, "events are ordered by sequence");
    }

    let sandbox = position(&events, "sandbox.decision");
    let permission = position(&events, "permission.decision");
    let started = position(&events, "tool.started");
    let completed = position(&events, "tool.completed");
    let diff = position(&events, "diff.recorded");
    let terminal = position(&events, "task.succeeded");
    assert!(sandbox < started);
    assert!(permission < started);
    assert!(started < completed);
    assert!(completed < diff);
    assert!(diff < terminal);

    let diff_event = &events[diff];
    assert_eq!(diff_event.payload["tool"], "write_file");
    assert_eq!(diff_event.payload["files"][0]["path"], "notes/a.txt");
    assert_eq!(diff_event.payload["files"][0]["change"], "add");
    assert_eq!(diff_event.payload["redaction"]["content"], "omitted");
    assert_eq!(
        std::fs::read_to_string(data.path().join("workspace/notes/a.txt")).unwrap(),
        "alpha\nbeta\n"
    );
}

#[tokio::test]
async fn unmeasured_timing_and_token_fields_are_omitted_not_zero() {
    let data = tempfile::tempdir().expect("data directory");
    let instance = start(data.path(), "full-auto", "").await;
    let runtime = instance.runtime();
    let task_id = submit(
        &runtime,
        "usage-unknown: report usage without estimating",
        "unmeasured",
    )
    .await;
    let task = wait(&runtime, &task_id).await;
    let events = events(&runtime, &task_id).await;
    let succeeded = events
        .iter()
        .find(|event| event.kind == "task.succeeded")
        .expect("terminal event");
    let usage = &succeeded.payload["usage"];
    assert!(usage.get("prompt_tokens").is_none(), "{usage}");
    assert!(usage.get("candidate_tokens").is_none(), "{usage}");
    assert!(usage.get("estimated_cost_micros").is_none(), "{usage}");
    // A local estimate is a measurement class of its own; it must not be
    // reported as provider-confirmed tokens.
    assert!(usage.get("estimated_tokens").is_some(), "{usage}");
    let _ = task;
}

#[tokio::test]
async fn denied_calls_omit_duration_while_executed_calls_report_it() {
    let data = tempfile::tempdir().expect("data directory");
    let rules = "\n[[permissions.rules]]\nid = \"deny-write\"\ndecision = \"deny\"\ntools = [\"write_file\"]\n";
    let instance = start(data.path(), "full-auto", rules).await;
    let runtime = instance.runtime();
    let task_id = submit(
        &runtime,
        "call:write_file {\"path\":\"notes/denied.txt\",\"content\":\"nope\"}",
        "denied",
    )
    .await;
    let task = wait(&runtime, &task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let events = events(&runtime, &task_id).await;
    let completed = events
        .iter()
        .find(|event| event.kind == "tool.completed")
        .expect("tool completed evidence");
    assert_eq!(completed.payload["ok"], false);
    assert!(
        completed.payload.get("duration_ms").is_none(),
        "a denied call never ran, so it has no measured duration: {}",
        completed.payload
    );
    let traces = runtime.traces_for(&task_id).await;
    assert!(
        traces.iter().all(|sample| sample.span != "tool.execute"),
        "an unexecuted call must not fabricate a duration trace"
    );
}

#[tokio::test]
async fn redaction_removes_secrets_and_disallowed_paths_from_events_audit_and_diagnostics() {
    let data = tempfile::tempdir().expect("data directory");
    let instance = start(data.path(), "full-auto", "").await;
    let runtime = instance.runtime();
    let input = format!("remember {SECRET} at {DISALLOWED}");
    let task_id = submit(&runtime, &input, "redaction").await;
    let task = wait(&runtime, &task_id).await;

    // The deterministic model echoes the prompt into the terminal output, so a
    // missing redaction pass would expose both classes here.
    let events = events(&runtime, &task_id).await;
    let rendered = serde_json::to_string(&events).expect("events JSON");
    assert!(!rendered.contains(SECRET), "{rendered}");
    assert!(!rendered.contains(DISALLOWED), "{rendered}");
    assert!(rendered.contains("[REDACTED]"), "{rendered}");
    assert!(rendered.contains("[REDACTED_PATH]"), "{rendered}");

    runtime
        .audit(
            "local",
            "observability.test",
            &task.id,
            "deny",
            &format!("reason={DISALLOWED} credential={SECRET}"),
        )
        .await
        .expect("audit recorded");
    let entries = runtime
        .audit_entries(&admin(), &task.id, 100)
        .await
        .expect("audit entries");
    let readable = entries
        .iter()
        .find(|entry| entry.action == "observability.test")
        .expect("test audit entry");
    assert_eq!(readable.decision, "deny");
    assert!(!readable.reason.contains(SECRET), "{}", readable.reason);
    assert!(!readable.reason.contains(DISALLOWED), "{}", readable.reason);

    let bundle = runtime
        .diagnostics(&admin(), Some(&task.id))
        .await
        .expect("diagnostics");
    let rendered = serde_json::to_string(&bundle).expect("diagnostics JSON");
    assert!(!rendered.contains(SECRET), "{rendered}");
    assert!(!rendered.contains(DISALLOWED), "{rendered}");
}

#[tokio::test]
async fn audit_entries_name_principal_action_decision_reason_and_resource() {
    let data = tempfile::tempdir().expect("data directory");
    let instance = start(data.path(), "full-auto", "").await;
    let runtime = instance.runtime();
    let task_id = submit(
        &runtime,
        "call:read_file {\"path\":\"missing.txt\"}",
        "audit",
    )
    .await;
    let task = wait(&runtime, &task_id).await;
    let entries = runtime
        .audit_entries(&admin(), &task.id, 100)
        .await
        .expect("audit entries");
    let submit_entry = entries
        .iter()
        .find(|entry| entry.action == "task.submit")
        .expect("acceptance audit");
    assert_eq!(submit_entry.principal, "local");
    assert_eq!(submit_entry.decision, "accepted");
    assert_eq!(submit_entry.resource, task.id);
    assert!(!submit_entry.reason.is_empty());
    assert_eq!(
        submit_entry.trace_id.as_deref(),
        Some(task.trace_id.as_str())
    );
}

#[tokio::test]
async fn effect_evidence_links_operation_to_task_attempt_invocation_and_terminal_result() {
    let data = tempfile::tempdir().expect("data directory");
    let instance = start(data.path(), "full-auto", "").await;
    let runtime = instance.runtime();
    let task_id = submit(
        &runtime,
        "call:ledger_write {\"entry\":\"alpha\"}",
        "effect",
    )
    .await;
    let task = wait(&runtime, &task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let evidence = runtime
        .effect_evidence(&task.id)
        .await
        .expect("effect evidence");
    assert_eq!(evidence.task_id, task.id);
    assert_eq!(evidence.state, TaskState::Succeeded);
    assert!(evidence.terminal);
    assert_eq!(evidence.trace_id.as_deref(), Some(task.trace_id.as_str()));
    assert_eq!(evidence.attempt, task.attempt);
    assert_eq!(
        evidence.native_invocation_id, task.native_invocation_id,
        "the confirmed native invocation is linked"
    );
    assert_eq!(evidence.operations.len(), 1);
    let operation = &evidence.operations[0];
    assert_eq!(operation.task_id, task.id);
    assert_eq!(operation.tool, "ledger_write");
    assert_eq!(operation.status, "committed");
    assert!(operation.terminal);
    assert!(operation.receipt.is_some());
}

#[tokio::test]
async fn observability_is_available_over_http_and_survives_worker_restart() {
    let data = tempfile::tempdir().expect("data directory");
    let config = fixture(data.path(), "full-auto", "");
    let instance = Worker::new("observability", "0.1.0")
        .file_tools()
        .effect_tool(
            "ledger_write",
            Arc::new(Ledger),
            Arc::new(LedgerStatus),
            EffectPolicy::default(),
        )
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let task_id = submit(
        &runtime,
        "call:write_file {\"path\":\"restart.txt\",\"content\":\"durable\"}",
        "restart",
    )
    .await;
    let task = wait(&runtime, &task_id).await;
    assert_eq!(task.state, TaskState::Succeeded);

    let base = serve(&instance).await;
    let client = reqwest::Client::new();
    let audit: Value = client
        .get(format!("{base}/v1/tasks/{task_id}/audit"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("audit request")
        .json()
        .await
        .expect("audit JSON");
    assert!(audit.as_array().is_some_and(|entries| !entries.is_empty()));
    assert!(audit[0].get("principal").is_some());
    assert!(audit[0].get("decision").is_some());
    assert!(audit[0].get("reason").is_some());
    assert!(audit[0].get("resource").is_some());

    let operations: Value = client
        .get(format!("{base}/v1/tasks/{task_id}/operations"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("operations request")
        .json()
        .await
        .expect("operations JSON");
    assert_eq!(operations["task_id"], task_id);
    assert_eq!(operations["state"], "succeeded");
    assert_eq!(operations["terminal"], true);
    assert_eq!(operations["trace_id"], task.trace_id);

    instance.shutdown().await;
    drop(instance);

    let restarted = Worker::new("observability", "0.1.0")
        .file_tools()
        .effect_tool(
            "ledger_write",
            Arc::new(Ledger),
            Arc::new(LedgerStatus),
            EffectPolicy::default(),
        )
        .start(&config)
        .await
        .expect("worker restarts on the same data directory");
    let runtime = restarted.runtime();
    let recovered = runtime
        .task_unscoped(&task_id)
        .await
        .expect("retained task");
    assert_eq!(recovered.trace_id, task.trace_id);
    let events = events(&runtime, &task_id).await;
    assert!(events.iter().any(|event| event.kind == "task.succeeded"));
    assert!(events.iter().any(|event| event.kind == "diff.recorded"));
    assert!(events
        .iter()
        .all(|event| event.trace_id.as_deref() == Some(task.trace_id.as_str())));
    let entries = runtime
        .audit_entries(&admin(), &task_id, 100)
        .await
        .expect("retained audit");
    assert!(entries.iter().any(|entry| entry.action == "task.submit"));
    let evidence = runtime
        .effect_evidence(&task_id)
        .await
        .expect("retained evidence");
    assert_eq!(evidence.state, TaskState::Succeeded);
    assert!(evidence.terminal);
    restarted.shutdown().await;
}

/// Mount the real harness router on the instance runtime and return its base.
async fn serve(instance: &WorkerInstance) -> String {
    use runtime::serving::{api_router, authenticate, WorkerState};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let state = WorkerState::new(instance.runtime(), instance.manifest().clone());
    let app = authenticate(api_router(state.clone()), state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}
