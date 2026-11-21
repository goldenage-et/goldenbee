//! Generic file/search tool parity through the real Worker runtime.
//!
//! These tests submit durable tasks to a started Worker and observe external
//! behavior: files on disk, task events, audit records, cancellation, and
//! capability inventory. They do not call ADK tools or kernel internals.

use adk_rust::Tool;
use async_trait::async_trait;
use common::{Limits, StreamEvent, SubmitTaskRequest, Task, TaskState};
use runtime::worker::{BuiltinTool, Worker};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn fixture(data: &Path, tools: &[&str], tools_config: &str) -> PathBuf {
    let assets = data.join("assets");
    let agents = assets.join("agents");
    let prompts = assets.join("prompts");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::write(
        agents.join("tools.toml"),
        format!(
            "name = \"tools\"\ndescription = \"file tool parity agent\"\nmodel = \"deterministic\"\nprompt = \"tools\"\ntools = [{}]\n",
            tools
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
            "instance = \"parity-{}\"\nnamespace = \"local\"\ndefault_agent = \"tools\"\ntoken = \"dev-token\"\ntoken_scopes = [\"*\"]\n\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n\n[tools]\n{}",
            data.file_name().unwrap().to_string_lossy(),
            data.display(),
            tools_config
        ),
    )
    .expect("worker config");
    config
}

fn default_tools_config() -> &'static str {
    "workspace = \"workspace\"\ntimeout_ms = 120000\nmax_output_bytes = 1048576\nmax_parallel = 2\n"
}

fn all_tools() -> Vec<&'static str> {
    vec![
        "read_file",
        "write_file",
        "edit_file",
        "multi_edit",
        "apply_patch",
        "glob",
        "grep",
    ]
}

async fn submit(runtime: &runtime::Runtime, input: &str, session: &str) -> String {
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
    let admin = runtime_principal();
    let submitted = runtime
        .submit_task(&admin, request)
        .await
        .expect("task admitted");
    submitted.task_id
}

fn runtime_principal() -> common::Principal {
    common::Principal::new("local", "local", &["*"])
}

async fn wait(runtime: &runtime::Runtime, task_id: &str) -> Task {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let task = runtime.task_unscoped(task_id).await.expect("task exists");
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

async fn run(runtime: &runtime::Runtime, input: &str, session: &str) -> Task {
    let task_id = submit(runtime, input, session).await;
    wait(runtime, &task_id).await
}

fn output(task: &Task) -> String {
    task.output.clone().unwrap_or_default()
}

fn result_json(text: &str) -> Value {
    let start = text
        .find("mcp result ")
        .map(|index| index + 11)
        .unwrap_or(0);
    serde_json::from_str(text[start..].trim()).unwrap_or(Value::Null)
}

#[tokio::test(flavor = "multi_thread")]
async fn opt_in_inventory_is_stable_and_disabled_tools_are_not_advertised() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &all_tools(), default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .file_tools()
        .start(&config)
        .await
        .expect("worker starts");
    let manifest = instance.manifest();
    for expected in all_tools() {
        let capability = manifest
            .capabilities
            .iter()
            .find(|capability| capability.id == expected)
            .unwrap_or_else(|| panic!("missing {expected} in inventory"));
        assert_eq!(capability.kind, "tool");
    }
    assert!(manifest
        .capabilities
        .iter()
        .filter(|capability| capability.id == "write_file")
        .flat_map(|capability| capability.permissions.iter())
        .any(|permission| permission == "workspace:write"));
    instance.shutdown().await;

    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &["read_file"], default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .file_tool(BuiltinTool::ReadFile)
        .start(&config)
        .await
        .expect("worker starts");
    assert!(!instance
        .manifest()
        .capabilities
        .iter()
        .any(|capability| capability.id == "write_file"));
    let runtime = instance.runtime();
    let task = run(
        &runtime,
        "call:write_file {\"path\":\"disabled.txt\",\"content\":\"nope\"}",
        "gating",
    )
    .await;
    assert!(!data.path().join("workspace/disabled.txt").exists());
    let events = events(&runtime, &task.id).await;
    assert!(!events
        .iter()
        .any(|event| { event.kind == "tool.completed" && event.payload["tool"] == "write_file" }));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn adk_file_and_search_tools_execute_against_the_configured_workspace() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &all_tools(), default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .file_tools()
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let workspace = data.path().join("workspace");

    let task = run(
        &runtime,
        "mcp:write_file {\"path\":\"notes/a.txt\",\"content\":\"alpha\\nbeta\\n\"}",
        "files",
    )
    .await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("notes/a.txt")).unwrap(),
        "alpha\nbeta\n"
    );

    let task = run(
        &runtime,
        "mcp:read_file {\"path\":\"notes/a.txt\"}",
        "files",
    )
    .await;
    let body = output(&task);
    assert!(body.contains("alpha"), "{body}");
    assert!(body.contains("beta"), "{body}");
    assert_eq!(result_json(&body)["total_lines"], 2);

    let task = run(
        &runtime,
        "mcp:edit_file {\"path\":\"notes/a.txt\",\"old_string\":\"beta\",\"new_string\":\"BETA\"}",
        "files",
    )
    .await;
    assert!(output(&task).contains("replacements"), "{}", output(&task));

    let before = std::fs::read_to_string(workspace.join("notes/a.txt")).unwrap();
    let task = run(
        &runtime,
        "mcp:multi_edit {\"path\":\"notes/a.txt\",\"edits\":[{\"old_string\":\"alpha\",\"new_string\":\"ALPHA\"},{\"old_string\":\"missing\",\"new_string\":\"x\"}]}",
        "files",
    )
    .await;
    assert!(output(&task).contains("edit_no_match"), "{}", output(&task));
    assert_eq!(
        std::fs::read_to_string(workspace.join("notes/a.txt")).unwrap(),
        before,
        "a failed multi_edit must not write a partial result"
    );

    std::fs::write(data.path().join("host.txt"), "HOST-CANARY").unwrap();
    let task = run(
        &runtime,
        "mcp:read_file {\"path\":\"../host.txt\"}",
        "files",
    )
    .await;
    assert!(
        output(&task).contains("tool.path_denied"),
        "{}",
        output(&task)
    );
    assert!(!output(&task).contains("HOST-CANARY"));

    let task = run(
        &runtime,
        "mcp:multi_edit {\"path\":\"notes/a.txt\",\"edits\":[{\"old_string\":\"alpha\",\"new_string\":\"ALPHA\"},{\"old_string\":\"BETA\",\"new_string\":\"GAMMA\"}]}",
        "files",
    )
    .await;
    assert!(output(&task).contains("\"edits\":2"), "{}", output(&task));
    assert_eq!(
        std::fs::read_to_string(workspace.join("notes/a.txt")).unwrap(),
        "ALPHA\nGAMMA\n"
    );

    let patch = "*** Begin Patch\n*** Update File: notes/a.txt\n@@\n-ALPHA\n+DELTA\n*** Add File: notes/b.txt\n+created\n*** End Patch";
    let task = run(
        &runtime,
        &format!("mcp:apply_patch {}", json!({"patch": patch})),
        "files",
    )
    .await;
    assert!(output(&task).contains("Applied patch"), "{}", output(&task));
    assert_eq!(
        std::fs::read_to_string(workspace.join("notes/a.txt")).unwrap(),
        "DELTA\nGAMMA\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("notes/b.txt")).unwrap(),
        "created\n"
    );

    let task = run(&runtime, "mcp:glob {\"pattern\":\"notes/*.txt\"}", "files").await;
    let glob = result_json(&output(&task));
    assert_eq!(glob["count"], 2, "{glob}");
    assert!(glob["matches"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .any(|path| path == "notes/b.txt"));

    let task = run(&runtime, "mcp:grep {\"pattern\":\"DELTA\"}", "files").await;
    let grep = result_json(&output(&task));
    assert_eq!(grep["count"], 1, "{grep}");
    assert!(grep["matches"][0]
        .as_str()
        .unwrap_or_default()
        .contains("notes/a.txt"));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mutating_tools_commit_effect_evidence_and_redacted_audit_before_the_outcome() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &all_tools(), default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .file_tools()
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let task = run(
        &runtime,
        "mcp:write_file {\"path\":\"secret.txt\",\"content\":\"SUPER-SECRET-CONTENT\"}",
        "evidence",
    )
    .await;
    assert_eq!(task.state, TaskState::Succeeded);
    let events = events(&runtime, &task.id).await;
    let started = events
        .iter()
        .find(|event| event.kind == "tool.started")
        .expect("tool started event");
    assert_eq!(started.payload["tool"], "write_file");
    assert_eq!(started.payload["permission"]["decision"], "allow");
    assert_eq!(started.payload["sandbox"]["profile"], "workspace-write");
    assert_eq!(started.payload["redaction"]["arguments"], "digest");
    let effect = events
        .iter()
        .find(|event| event.kind == "tool.effect")
        .expect("effect evidence");
    assert_eq!(effect.payload["kind"], "file_write");
    assert_eq!(effect.payload["paths"][0], "secret.txt");
    assert_eq!(effect.payload["ok"], true);
    assert_eq!(effect.payload["diff"]["files"][0]["change"], "add");
    let completed = events
        .iter()
        .find(|event| event.kind == "tool.completed")
        .expect("tool completed event");
    assert_eq!(completed.payload["tool"], "write_file");
    assert!(completed.payload["duration_ms"].is_u64());
    let terminal = events
        .iter()
        .find(|event| event.kind == "task.succeeded")
        .expect("terminal event");
    assert!(effect.seq < terminal.seq, "effect must precede the outcome");
    assert!(
        completed.seq < terminal.seq,
        "completion must precede the outcome"
    );

    let audit = runtime.audit_records(&task.id).await;
    let entry = audit
        .iter()
        .find(|record| record.action == "tool.effect")
        .expect("redacted audit entry");
    assert_eq!(entry.outcome, "committed");
    assert!(entry.detail.contains("secret.txt"));
    assert!(!entry.detail.contains("SUPER-SECRET-CONTENT"));
    let serialized = serde_json::to_string(&events).unwrap();
    assert!(!serialized.contains("SUPER-SECRET-CONTENT"));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_schema_and_event_shapes_are_stable_across_worker_restart() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &all_tools(), default_tools_config());
    let mut schema_digests = Vec::new();
    let mut event_keys: Vec<Vec<String>> = Vec::new();
    for attempt in 0..2 {
        let instance = Worker::new("parity", "0.1.0")
            .file_tools()
            .start(&config)
            .await
            .expect("worker starts");
        let runtime = instance.runtime();
        let task = run(
            &runtime,
            &format!(
                "mcp:write_file {{\"path\":\"restart-{attempt}.txt\",\"content\":\"stable\"}}"
            ),
            &format!("restart-{attempt}"),
        )
        .await;
        assert_eq!(task.state, TaskState::Succeeded);
        let events = events(&runtime, &task.id).await;
        let started = events
            .iter()
            .find(|event| event.kind == "tool.started")
            .expect("started event");
        schema_digests.push(started.payload["schema_digest"].clone());
        let mut keys: Vec<String> = started
            .payload
            .as_object()
            .expect("payload object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        event_keys.push(keys);
        instance.shutdown().await;
    }
    assert_eq!(schema_digests[0], schema_digests[1]);
    assert_eq!(event_keys[0], event_keys[1]);
}

#[tokio::test(flavor = "multi_thread")]
async fn read_output_is_bounded_with_truncation_metadata() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(
        data.path(),
        &all_tools(),
        "workspace = \"workspace\"\ntimeout_ms = 120000\nmax_output_bytes = 768\nmax_parallel = 2\n",
    );
    let instance = Worker::new("parity", "0.1.0")
        .file_tools()
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let content = "x".repeat(8192);
    let task = run(
        &runtime,
        &format!(
            "mcp:write_file {}",
            json!({"path": "large.txt", "content": content})
        ),
        "bounds",
    )
    .await;
    assert_eq!(task.state, TaskState::Succeeded);
    let task = run(&runtime, "mcp:read_file {\"path\":\"large.txt\"}", "bounds").await;
    let body = output(&task);
    assert!(body.contains("\"truncated\":true"), "{body}");
    assert!(body.contains("\"original_bytes\""), "{body}");
    assert!(body.len() < 4096, "result was not bounded: {}", body.len());
    assert_eq!(
        std::fs::read_to_string(data.path().join("workspace/large.txt"))
            .unwrap()
            .len(),
        8192
    );
    instance.shutdown().await;
}

struct SlowTool;

#[async_trait]
impl Tool for SlowTool {
    fn name(&self) -> &str {
        "slow_tool"
    }

    fn description(&self) -> &str {
        "Sleeps so the runtime deadline and cancellation can be observed"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({"type": "object", "properties": {}}))
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn adk_rust::ToolContext>,
        _args: Value,
    ) -> adk_rust::Result<Value> {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(json!({"ok": true}))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_timeout_is_a_typed_result_and_the_task_continues() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(
        data.path(),
        &["slow_tool"],
        "workspace = \"workspace\"\ntimeout_ms = 100\nmax_output_bytes = 1048576\nmax_parallel = 2\n",
    );
    let instance = Worker::new("parity", "0.1.0")
        .file_tools()
        .tool("slow_tool", Arc::new(SlowTool) as Arc<dyn Tool>)
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let started = Instant::now();
    let task = run(&runtime, "mcp:slow_tool {}", "timeout").await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "timeout did not bound the call"
    );
    assert!(output(&task).contains("tool.timeout"), "{}", output(&task));
    let events = events(&runtime, &task.id).await;
    assert!(events.iter().any(|event| {
        event.kind == "tool.completed" && event.payload["error_code"] == "tool.timeout"
    }));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn task_cancellation_stops_a_running_tool() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(
        data.path(),
        &["slow_tool"],
        "workspace = \"workspace\"\ntimeout_ms = 60000\nmax_output_bytes = 1048576\nmax_parallel = 2\n",
    );
    let instance = Worker::new("parity", "0.1.0")
        .file_tools()
        .tool("slow_tool", Arc::new(SlowTool) as Arc<dyn Tool>)
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let task_id = submit(&runtime, "mcp:slow_tool {}", "cancel").await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let events = events(&runtime, &task_id).await;
        if events
            .iter()
            .any(|event| event.kind == "tool.started" && event.payload["tool"] == "slow_tool")
        {
            break;
        }
        assert!(Instant::now() < deadline, "the tool never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let principal = runtime_principal();
    runtime
        .cancel_task(&principal, &task_id)
        .await
        .expect("cancel accepted");
    let task = wait(&runtime, &task_id).await;
    assert_eq!(task.state, TaskState::Cancelled, "{task:?}");
    assert!(
        Instant::now() < deadline + Duration::from_secs(5),
        "cancellation did not stop the turn"
    );
    instance.shutdown().await;
}
