//! Turn-boundary file checkpoints, typed diff preview, rewind/undo/restore,
//! restart durability, and retention through a real Worker runtime.
//!
//! The tests observe the public runtime and HTTP-facing API only. They assert
//! file state, task events, checkpoint manifests, and retention reports.

use async_trait::async_trait;
use common::{Limits, Principal, StreamEvent, SubmitTaskRequest, Task, TaskState};
use runtime::worker::{ToolDescriptor, ToolEffect, ToolRisk, Worker};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct PatchPair {
    workspace: PathBuf,
}

#[async_trait]
impl adk_rust::Tool for PatchPair {
    fn name(&self) -> &str {
        "patch_pair"
    }

    fn description(&self) -> &str {
        "Write the same content to two workspace files"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "one": {"type": "string"},
                "two": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["one", "two", "content"]
        }))
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn adk_rust::ToolContext>,
        args: Value,
    ) -> adk_rust::Result<Value> {
        let content = args["content"].as_str().unwrap_or_default();
        for key in ["one", "two"] {
            let path = args[key].as_str().unwrap_or_default();
            let target = self.workspace.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| adk_rust::AdkError::tool(error.to_string()))?;
            }
            std::fs::write(&target, content)
                .map_err(|error| adk_rust::AdkError::tool(error.to_string()))?;
        }
        Ok(json!({"ok": true, "files": 2}))
    }
}

fn pair_paths(args: &Value) -> Vec<String> {
    ["one", "two"]
        .iter()
        .filter_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

fn fixture(data: &Path, retention: &str) -> PathBuf {
    let assets = data.join("assets");
    let agents = assets.join("agents");
    let prompts = assets.join("prompts");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::write(
        agents.join("patcher.toml"),
        "name = \"patcher\"\ndescription = \"checkpoint patcher\"\nmodel = \"deterministic\"\nprompt = \"patcher\"\ntools = [\"patch_pair\"]\n",
    )
    .expect("agent definition");
    std::fs::write(prompts.join("patcher.md"), "Use the patch tool.").expect("prompt");
    let config = data.join("worker.toml");
    std::fs::write(
        &config,
        format!(
            "instance = \"checkpoints-{}\"\nnamespace = \"local\"\ndefault_agent = \"patcher\"\ntoken = \"dev-token\"\ntoken_scopes = [\"*\"]\n\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n\n[tools]\nworkspace = \"workspace\"\ntimeout_ms = 120000\nmax_output_bytes = 1048576\nmax_parallel = 2\n\n[permissions]\nmode = \"full-auto\"\n\n[retention]\n{retention}",
            data.file_name().unwrap().to_string_lossy(),
            data.display(),
        ),
    )
    .expect("worker config");
    config
}

fn descriptor() -> ToolDescriptor {
    ToolDescriptor::new("workspace:write")
        .effect(ToolEffect::FileWrite)
        .risk(ToolRisk::High)
        .paths(pair_paths)
}

async fn start(config: &Path) -> runtime::worker::WorkerInstance {
    let data_dir = config.parent().expect("config parent").to_path_buf();
    let workspace = data_dir.join("workspace");
    Worker::new("checkpoints", "0.1.0")
        .tool_with_descriptor(
            "patch_pair",
            Arc::new(PatchPair {
                workspace: workspace.clone(),
            }),
            descriptor(),
            ["workspace:write"],
        )
        .start(config)
        .await
        .expect("worker starts")
}

fn admin() -> Principal {
    Principal::new("local", "local", &["*"])
}

async fn submit(runtime: &runtime::Runtime, input: &str, key: &str) -> String {
    runtime
        .submit_task(
            &admin(),
            SubmitTaskRequest {
                namespace: Some("local".into()),
                agent: "patcher".into(),
                session_id: format!("checkpoint-session-{key}"),
                input: input.into(),
                idempotency_key: key.into(),
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

async fn run(runtime: &runtime::Runtime, input: &str, key: &str) -> Task {
    let id = submit(runtime, input, key).await;
    wait(runtime, &id).await
}

async fn events(runtime: &runtime::Runtime, task_id: &str) -> Vec<StreamEvent> {
    runtime
        .events_since(task_id, 0, 10_000)
        .await
        .expect("events")
        .events
}

fn patch(content: &str) -> String {
    format!(
        "call:patch_pair {{\"one\":\"notes/one.txt\",\"two\":\"notes/two.txt\",\"content\":\"{content}\"}}"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn mutating_tool_creates_checkpoint_diff_and_rewind() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), "checkpoints_days = 30\n");
    let instance = start(&config).await;
    let runtime = instance.runtime();
    let workspace = data.path().join("workspace");

    let task = run(&runtime, &patch("alpha\\nbeta\\n"), "turn-1").await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("notes/one.txt")).unwrap(),
        "alpha\nbeta\n"
    );

    let checkpoints = runtime
        .checkpoints_for_task(&admin(), &task.id)
        .await
        .expect("checkpoints");
    assert_eq!(checkpoints.len(), 1, "{checkpoints:?}");
    let checkpoint = &checkpoints[0];
    assert_eq!(checkpoint.status, "closed");
    assert_eq!(checkpoint.task_id, task.id);
    assert_eq!(checkpoint.files.len(), 2);
    let mut paths = checkpoint
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(paths, vec!["notes/one.txt", "notes/two.txt"]);
    assert!(checkpoint
        .files
        .iter()
        .all(|file| file.change == "added" && file.after_digest.is_some()));

    let diff = runtime
        .checkpoint_diff(&admin(), &checkpoint.id)
        .await
        .expect("diff");
    assert_eq!(diff.files.len(), 2);
    assert_eq!(diff.additions, 4);
    for file in &diff.files {
        assert_eq!(file.change, "added");
        assert!(!file.path.contains(data.path().to_str().unwrap()));
        assert!(!file.hunks.is_empty());
    }

    // File-set rewind restores one selected path.
    let outcome = runtime
        .restore_checkpoint(
            &admin(),
            &checkpoint.id,
            &common::CheckpointRestoreRequest {
                mode: "rewind".into(),
                files: Some(vec!["notes/one.txt".into()]),
            },
        )
        .await
        .expect("partial rewind");
    assert_eq!(outcome.changed, vec!["notes/one.txt".to_string()]);
    assert!(!workspace.join("notes/one.txt").exists());
    assert!(workspace.join("notes/two.txt").exists());

    // Rewinding the whole turn removes both files and converges on retry.
    let outcome = runtime
        .restore_checkpoint(
            &admin(),
            &checkpoint.id,
            &common::CheckpointRestoreRequest {
                mode: "undo".into(),
                files: None,
            },
        )
        .await
        .expect("turn rewind");
    assert!(!outcome.already_applied);
    assert!(!workspace.join("notes/one.txt").exists());
    assert!(!workspace.join("notes/two.txt").exists());
    let outcome = runtime
        .restore_checkpoint(
            &admin(),
            &checkpoint.id,
            &common::CheckpointRestoreRequest {
                mode: "rewind".into(),
                files: None,
            },
        )
        .await
        .expect("idempotent rewind");
    assert!(outcome.already_applied);
    assert!(outcome.changed.is_empty());

    // Restore re-applies the post-turn state.
    runtime
        .restore_checkpoint(
            &admin(),
            &checkpoint.id,
            &common::CheckpointRestoreRequest {
                mode: "restore".into(),
                files: None,
            },
        )
        .await
        .expect("post-state restore");
    assert_eq!(
        std::fs::read_to_string(workspace.join("notes/two.txt")).unwrap(),
        "alpha\nbeta\n"
    );

    let kinds = events(&runtime, &task.id)
        .await
        .into_iter()
        .map(|event| event.kind)
        .collect::<Vec<_>>();
    assert!(
        kinds.iter().any(|kind| kind == "checkpoint.created"),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "checkpoint.committed"),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "checkpoint.restored"),
        "{kinds:?}"
    );

    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupted_snapshot_fails_closed_before_writing() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), "checkpoints_days = 30\n");
    let instance = start(&config).await;
    let runtime = instance.runtime();
    let workspace = data.path().join("workspace");
    let _ = run(&runtime, &patch("original\\n"), "turn-corrupt-1").await;
    let task = run(&runtime, &patch("changed\\n"), "turn-corrupt-2").await;
    let checkpoint = runtime
        .checkpoints_for_task(&admin(), &task.id)
        .await
        .expect("checkpoints")
        .remove(0);
    let digest = checkpoint.files[0]
        .before_digest
        .as_deref()
        .expect("digest")
        .to_string();

    let hex = digest.trim_start_matches("sha256:");
    let blob = runtime
        .checkpoint_store()
        .blob_dir()
        .join(&hex[..2])
        .join(&hex[2..]);
    std::fs::write(&blob, b"corrupted").expect("corrupt blob");

    let error = runtime
        .restore_checkpoint(
            &admin(),
            &checkpoint.id,
            &common::CheckpointRestoreRequest {
                mode: "rewind".into(),
                files: None,
            },
        )
        .await
        .expect_err("corruption fails closed");
    assert_eq!(error.code, "checkpoint.corrupt");
    assert_eq!(
        std::fs::read_to_string(workspace.join("notes/one.txt")).unwrap(),
        "changed\n",
        "a failed verification must not write the workspace"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checkpoints_survive_restart_and_join_retention() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), "checkpoints_days = 1\n");
    let instance = start(&config).await;
    let runtime = instance.runtime();
    let workspace = data.path().join("workspace");
    let task = run(&runtime, &patch("alpha\\n"), "turn-restart").await;
    let checkpoint = runtime
        .checkpoints_for_task(&admin(), &task.id)
        .await
        .expect("checkpoints")
        .remove(0);
    let blob_count = count_blobs(&data.path().join("checkpoints/blobs"));
    assert!(blob_count > 0);
    instance.shutdown().await;

    // A fresh process reads the same durable manifest and blobs.
    let instance = start(&config).await;
    let runtime = instance.runtime();
    let restored = runtime
        .checkpoints_for_task(&admin(), &task.id)
        .await
        .expect("checkpoints after restart");
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].id, checkpoint.id);
    runtime
        .restore_checkpoint(
            &admin(),
            &checkpoint.id,
            &common::CheckpointRestoreRequest {
                mode: "rewind".into(),
                files: None,
            },
        )
        .await
        .expect("restart restore");
    assert!(!workspace.join("notes/one.txt").exists());

    // Age the manifest beyond the checkpoint window and run retention.
    let old = common::now_ms() - 3 * 86_400_000;
    runtime
        .store()
        .write(|state| {
            let record = state
                .checkpoints
                .get_mut(&checkpoint.id)
                .expect("checkpoint record");
            record.updated_at = old;
            Ok(())
        })
        .await
        .expect("age checkpoint");
    let report = runtime
        .apply_retention(&admin())
        .await
        .expect("retention applies");
    assert!(report.completed, "{report:?}");
    assert!(
        report.removed.contains_key("checkpoint_files"),
        "{report:?}"
    );
    assert!(runtime
        .checkpoints_for_task(&admin(), &task.id)
        .await
        .expect("checkpoints")
        .is_empty());
    assert_eq!(count_blobs(&data.path().join("checkpoints/blobs")), 0);
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checkpoint_routes_expose_typed_diff_and_restore() {
    use runtime::serving::{api_router, authenticate, WorkerState};

    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), "checkpoints_days = 30\n");
    let instance = start(&config).await;
    let runtime = instance.runtime();
    let workspace = data.path().join("workspace");
    let task = run(&runtime, &patch("alpha\\n"), "turn-http").await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let state = WorkerState::new(
        runtime.clone(),
        common::PackageManifest {
            name: "checkpoints".into(),
            version: "0.1.0".into(),
            protocol: common::PROTOCOL_VERSION.into(),
            runtime: "0.1.0".into(),
            adk: "2.3.0".into(),
            capabilities: Vec::new(),
            required_permissions: Vec::new(),
            features: Vec::new(),
            launcher_modes: Vec::new(),
            hooks: Vec::new(),
        },
    );
    let app = authenticate(api_router(state.clone()), state);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base = format!("http://{address}");
    let client = reqwest::Client::new();

    let checkpoints: Value = client
        .get(format!("{base}/v1/tasks/{}/checkpoints", task.id))
        .bearer_auth("dev-token")
        .send()
        .await
        .expect("checkpoints respond")
        .json()
        .await
        .expect("checkpoints JSON");
    let checkpoint = checkpoints
        .as_array()
        .expect("array")
        .first()
        .expect("checkpoint")
        .clone();
    let id = checkpoint["id"].as_str().expect("id").to_string();
    assert_eq!(checkpoint["files"].as_array().expect("files").len(), 2);

    let diff: Value = client
        .get(format!("{base}/v1/checkpoints/{id}/diff"))
        .bearer_auth("dev-token")
        .send()
        .await
        .expect("diff responds")
        .json()
        .await
        .expect("diff JSON");
    assert_eq!(diff["files"].as_array().expect("files").len(), 2);
    assert_eq!(diff["additions"], 2);

    let restored: Value = client
        .post(format!("{base}/v1/checkpoints/{id}/restore"))
        .bearer_auth("dev-token")
        .json(&json!({"mode": "rewind", "files": ["notes/one.txt"]}))
        .send()
        .await
        .expect("restore responds")
        .json()
        .await
        .expect("restore JSON");
    assert_eq!(restored["changed"][0], "notes/one.txt");
    assert!(!workspace.join("notes/one.txt").exists());
    assert!(workspace.join("notes/two.txt").exists());

    server.abort();
    instance.shutdown().await;
}

fn count_blobs(directory: &Path) -> usize {
    if !directory.exists() {
        return 0;
    }
    std::fs::read_dir(directory)
        .expect("blob dir")
        .map(|shard| {
            let shard = shard.expect("shard");
            if shard.path().is_dir() {
                std::fs::read_dir(shard.path())
                    .map(|files| files.count())
                    .unwrap_or(0)
            } else {
                0
            }
        })
        .sum()
}
