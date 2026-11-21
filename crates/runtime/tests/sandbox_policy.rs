//! Sandbox profiles and network policy through a real Worker runtime.
//!
//! These tests submit durable tasks to a started Worker and observe external
//! behavior: files on disk, task events, audit records, and the tool result the
//! deterministic model sees. They do not call sandbox internals.

use adk_rust::{Tool, ToolContext};
use async_trait::async_trait;
use common::{Limits, StreamEvent, SubmitTaskRequest, Task, TaskState};
use runtime::worker::{ToolDescriptor, ToolRisk, Worker, WorkerInstance};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TOOLS: &[&str] = &["read_file", "write_file", "web_probe"];

fn fixture(data: &Path, sandbox: &str) -> PathBuf {
    let assets = data.join("assets");
    let agents = assets.join("agents");
    let prompts = assets.join("prompts");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::write(
        agents.join("tools.toml"),
        format!(
            "name = \"tools\"\ndescription = \"sandbox agent\"\nmodel = \"deterministic\"\nprompt = \"tools\"\ntools = [{}]\n",
            TOOLS
                .iter()
                .map(|tool| format!("\"{tool}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
    .expect("agent definition");
    std::fs::write(prompts.join("tools.md"), "Use the supplied tools.").expect("prompt");
    let config = data.join("worker.toml");
    std::fs::write(
        &config,
        format!(
            "instance = \"sandbox-{}\"\nnamespace = \"local\"\ndefault_agent = \"tools\"\ntoken = \"dev-token\"\ntoken_scopes = [\"*\"]\n\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n\n[tools]\nworkspace = \"workspace\"\ntimeout_ms = 120000\nmax_output_bytes = 1048576\nmax_parallel = 2\n\n[permissions]\nmode = \"full-auto\"\napproval_wait_ms = 0\napproval_ttl_ms = 60000\n\n[sandbox]\n{}",
            data.file_name()
                .expect("temp dir name")
                .to_string_lossy(),
            data.display(),
            sandbox
        ),
    )
    .expect("worker config");
    config
}

struct NetworkProbe;

#[async_trait]
impl Tool for NetworkProbe {
    fn name(&self) -> &str {
        "web_probe"
    }

    fn description(&self) -> &str {
        "Test network capability used to observe sandbox network policy"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {"url": {"type": "string"}},
            "required": ["url"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_rust::Result<Value> {
        Ok(json!({"called": true, "body": "network probe ran"}))
    }
}

fn network_tool(worker: Worker) -> Worker {
    worker.tool_with_descriptor(
        "web_probe",
        Arc::new(NetworkProbe),
        ToolDescriptor::new("network:read")
            .risk(ToolRisk::Medium)
            .network()
            .domain(|args| {
                args.get("url")
                    .and_then(Value::as_str)
                    .and_then(|url| url.split("://").nth(1))
                    .map(|rest| rest.split('/').next().unwrap_or_default().to_string())
            }),
        ["network:read"],
    )
}

async fn start(data: &Path, sandbox: &str) -> WorkerInstance {
    let config = fixture(data, sandbox);
    network_tool(Worker::new("sandbox", "0.1.0").file_tools())
        .start(&config)
        .await
        .expect("worker starts")
}

fn admin() -> common::Principal {
    common::Principal::new("local", "local", &["*"])
}

async fn run(runtime: &runtime::Runtime, input: &str, session: &str) -> Task {
    let request = SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: "tools".into(),
        session_id: session.into(),
        input: input.into(),
        idempotency_key: format!("{session}-{}", common::canonical_digest(&input)),
        profile: None,
        team: None,
        worker: None,
        limits: Limits::default(),
    };
    let task_id = runtime
        .submit_task(&admin(), request)
        .await
        .expect("task admitted")
        .task_id;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let task = runtime.task_unscoped(&task_id).await.expect("task exists");
        if task.state.is_terminal() {
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn events(runtime: &runtime::Runtime, task_id: &str) -> Vec<StreamEvent> {
    runtime
        .events_since(task_id, 0, 10_000)
        .await
        .expect("events")
        .events
}

async fn sandbox_event(runtime: &runtime::Runtime, task_id: &str) -> StreamEvent {
    events(runtime, task_id)
        .await
        .into_iter()
        .find(|event| event.kind == "sandbox.decision")
        .expect("sandbox decision event")
}

fn output(task: &Task) -> String {
    task.output.clone().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_blocks_a_write_and_keeps_reads_through_the_worker() {
    let data = tempfile::tempdir().expect("data");
    let workspace = data.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::write(workspace.join("existing.txt"), "READABLE").expect("seed");
    let instance = start(data.path(), "profile = \"read-only\"\n").await;
    let runtime = instance.runtime();

    let denied = run(
        &runtime,
        "call:write_file {\"path\":\"denied.txt\",\"content\":\"MUST-NOT-WRITE\"}",
        "read-only",
    )
    .await;
    assert_eq!(denied.state, TaskState::Succeeded, "{denied:?}");
    assert!(
        output(&denied).contains("sandbox.read_only"),
        "{}",
        output(&denied)
    );
    assert!(!workspace.join("denied.txt").exists());
    let event = sandbox_event(&runtime, &denied.id).await;
    assert_eq!(event.payload["decision"], "deny");
    assert_eq!(event.payload["code"], "sandbox.read_only");
    assert_eq!(event.payload["profile"], "read-only");
    // The denied mutation leaves failed effect evidence, never a committed
    // effect.
    let effects: Vec<_> = events(&runtime, &denied.id)
        .await
        .into_iter()
        .filter(|event| event.kind == "tool.effect")
        .collect();
    assert!(!effects.is_empty(), "missing failed effect evidence");
    assert!(effects.iter().all(|event| event.payload["ok"] == false));
    let audit = runtime.audit_records(&denied.id).await;
    let entry = audit
        .iter()
        .find(|record| record.action == "sandbox.decision")
        .expect("sandbox audit entry");
    assert_eq!(entry.outcome, "deny");
    assert!(
        entry.detail.contains("profile=read-only"),
        "{}",
        entry.detail
    );
    assert!(entry.detail.contains("code=sandbox.read_only"));
    assert!(!entry.detail.contains("MUST-NOT-WRITE"));

    let read = run(
        &runtime,
        "call:read_file {\"path\":\"existing.txt\"}",
        "read-only-read",
    )
    .await;
    assert!(output(&read).contains("READABLE"), "{}", output(&read));
    assert!(!output(&read).contains("sandbox.read_only"));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_write_protects_metadata_and_honors_the_root_allowlist() {
    let data = tempfile::tempdir().expect("data");
    let workspace = data.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".git")).expect("git dir");
    std::fs::write(workspace.join(".git/config"), "ORIGINAL").expect("git config");
    std::fs::create_dir_all(workspace.join("notes")).expect("notes");
    std::fs::create_dir_all(workspace.join("other")).expect("other");
    let instance = start(
        data.path(),
        "profile = \"workspace-write\"\nwritable_roots = [\"notes\"]\n",
    )
    .await;
    let runtime = instance.runtime();

    let protected = run(
        &runtime,
        "call:write_file {\"path\":\".git/config\",\"content\":\"OVERWRITE\"}",
        "protected",
    )
    .await;
    assert!(
        output(&protected).contains("sandbox.protected_path"),
        "{}",
        output(&protected)
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join(".git/config")).unwrap(),
        "ORIGINAL"
    );

    let outside = run(
        &runtime,
        "call:write_file {\"path\":\"other/a.txt\",\"content\":\"no\"}",
        "outside",
    )
    .await;
    assert!(
        output(&outside).contains("sandbox.path_denied"),
        "{}",
        output(&outside)
    );
    assert!(!workspace.join("other/a.txt").exists());

    let allowed = run(
        &runtime,
        "call:write_file {\"path\":\"notes/a.txt\",\"content\":\"yes\"}",
        "allowed",
    )
    .await;
    assert_eq!(allowed.state, TaskState::Succeeded);
    assert_eq!(
        std::fs::read_to_string(workspace.join("notes/a.txt")).unwrap(),
        "yes"
    );
    instance.shutdown().await;

    // A second instance grants the protected path explicitly.
    let granted = start(
        data.path(),
        "profile = \"workspace-write\"\nprotected_grants = [\".git\"]\n",
    )
    .await;
    let runtime = granted.runtime();
    let written = run(
        &runtime,
        "call:write_file {\"path\":\".git/config\",\"content\":\"GRANTED\"}",
        "granted",
    )
    .await;
    assert_eq!(written.state, TaskState::Succeeded, "{written:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.join(".git/config")).unwrap(),
        "GRANTED"
    );
    granted.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn network_disabled_denies_a_network_tool_with_a_stable_error() {
    let data = tempfile::tempdir().expect("data");
    let instance = start(data.path(), "profile = \"workspace-write\"\n").await;
    let runtime = instance.runtime();

    let task = run(
        &runtime,
        "call:web_probe {\"url\":\"https://example.com/v1\"}",
        "network",
    )
    .await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert!(
        output(&task).contains("sandbox.network_disabled"),
        "{}",
        output(&task)
    );
    assert!(
        !output(&task).contains("network probe ran"),
        "the network tool executed: {}",
        output(&task)
    );
    let event = sandbox_event(&runtime, &task.id).await;
    assert_eq!(event.payload["decision"], "deny");
    assert_eq!(event.payload["code"], "sandbox.network_disabled");
    assert_eq!(event.payload["network"]["enabled"], false);
    let audit = runtime.audit_records(&task.id).await;
    assert!(audit.iter().any(|record| {
        record.action == "sandbox.decision"
            && record.outcome == "deny"
            && record.detail.contains("domain=example.com")
    }));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn network_allowlist_denies_unlisted_hosts_when_network_is_enabled() {
    let data = tempfile::tempdir().expect("data");
    let instance = start(
        data.path(),
        "profile = \"workspace-write\"\n\n[sandbox.network]\nenabled = true\nallow_hosts = [\"api.example.com\"]\n",
    )
    .await;
    let runtime = instance.runtime();

    let denied = run(
        &runtime,
        "call:web_probe {\"url\":\"https://evil.test/v1\"}",
        "network-allow",
    )
    .await;
    assert!(
        output(&denied).contains("sandbox.network_denied"),
        "{}",
        output(&denied)
    );
    assert!(!output(&denied).contains("network probe ran"));

    let allowed = run(
        &runtime,
        "call:web_probe {\"url\":\"https://api.example.com/v1\"}",
        "network-allow",
    )
    .await;
    assert!(
        output(&allowed).contains("network probe ran"),
        "{}",
        output(&allowed)
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn full_access_records_the_profile_and_every_protected_path_exception() {
    let data = tempfile::tempdir().expect("data");
    let workspace = data.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".git")).expect("git dir");
    std::fs::write(workspace.join(".git/config"), "ORIGINAL").expect("git config");
    let instance = start(data.path(), "profile = \"full-access\"\n").await;
    let runtime = instance.runtime();

    let task = run(
        &runtime,
        "call:write_file {\"path\":\".git/config\",\"content\":\"FULL\"}",
        "full-access",
    )
    .await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.join(".git/config")).unwrap(),
        "FULL"
    );
    let event = sandbox_event(&runtime, &task.id).await;
    assert_eq!(event.payload["decision"], "allow");
    assert_eq!(event.payload["profile"], "full-access");
    assert_eq!(event.payload["exceptions"][0]["kind"], "protected_path");
    let audit = runtime.audit_records(&task.id).await;
    assert!(audit.iter().any(|record| {
        record.action == "sandbox.decision"
            && record.outcome == "exception"
            && record.detail.contains("profile=full-access")
    }));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_sandbox_policy_fails_before_the_worker_serves() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), "command_max_bytes = 0\n");
    let error = match network_tool(Worker::new("sandbox", "0.1.0").file_tools())
        .start(&config)
        .await
    {
        Ok(_) => panic!("invalid sandbox policy is rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, "config.sandbox");
}
