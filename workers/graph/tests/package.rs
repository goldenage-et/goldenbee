//! Package inventory and shared-runtime graph reuse evidence.

use common::{Limits, Principal, SubmitTaskRequest, TaskState};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

fn config(data: &Path, instance: &str) -> std::path::PathBuf {
    let text = format!(
        "instance = \"{instance}\"\nnamespace = \"local\"\ndefault_agent = \"branching\"\ntoken = \"dev-token\"\ntoken_scopes = [\"*\"]\n\n[storage]\ndirectory = \"{}\"\n\n[settings]\nscenario = \"branching\"\n",
        data.display()
    );
    let path = data.join("worker.toml");
    std::fs::write(&path, text).expect("worker config");
    path
}

#[test]
fn package_manifest_advertises_native_workflow_and_protected_tool() {
    let manifest = graph::manifest();
    assert_eq!(manifest.name, "graph");
    assert_eq!(manifest.capabilities.len(), 2);
    let workflow = manifest
        .capabilities
        .iter()
        .find(|capability| capability.id == "branching")
        .expect("native workflow capability");
    assert_eq!(workflow.kind, "workflow");
    let tool = manifest
        .capabilities
        .iter()
        .find(|capability| capability.id == "ledger_write")
        .expect("protected effect tool capability");
    assert_eq!(tool.kind, "tool");
    assert!(manifest.features.iter().any(|feature| feature == "graph"));
    assert_eq!(manifest.launcher_modes, ["dev", "serve"]);
}

/// A second package definition uses the same shared-runtime graph support and
/// the same native checkpoint adapter. Its checkpoint identity names the second
/// package and its own instance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn different_package_reuses_shared_runtime_graph_support() {
    let data = tempfile::tempdir().expect("temporary data directory");
    let instance_id = instance_name(data.path());
    let config = config(data.path(), &instance_id);
    let worker = runtime::worker::Worker::new("graph-second", "0.0.1")
        .required_permissions(["task:submit", "session:write"])
        .graph("branching", 1, graph::build_graph);
    let package = worker.start(&config).await.expect("second package starts");
    let runtime = package.runtime();
    let principal = Principal::new("local", "local", &["*"]);
    let submitted = runtime
        .submit_task(
            &principal,
            SubmitTaskRequest {
                namespace: None,
                agent: "branching".into(),
                session_id: "second-session".into(),
                input: "go left".into(),
                idempotency_key: "second-key".into(),
                profile: None,
                team: None,
                worker: None,
                limits: Limits::default(),
            },
        )
        .await
        .expect("graph task is admitted");
    let id = submitted.task_id;

    let mut task = runtime
        .task(&principal, &id)
        .await
        .expect("task is visible");
    for _ in 0..500 {
        if task.state.is_terminal() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        task = runtime
            .task(&principal, &id)
            .await
            .expect("task is visible");
    }
    assert!(task.state.is_terminal(), "task did not finish: {task:?}");
    assert_eq!(task.state, TaskState::Succeeded);
    assert_eq!(task.output.as_deref(), Some("left:complete"));
    assert_eq!(task.native_thread_id.as_deref(), Some("second-session"));
    assert_eq!(task.profile, "default");
    assert_eq!(task.profile_revision, 1);

    let path = data.path().join("native-graph.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = sqlx::sqlite::SqlitePool::connect(&url)
        .await
        .expect("checkpoint store opens");
    let metadata: (String,) =
        sqlx::query_as("SELECT metadata FROM graph_checkpoints ORDER BY created_at DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("checkpoint exists");
    pool.close().await;
    let metadata: Value = serde_json::from_str(&metadata.0).expect("metadata is JSON");
    assert_eq!(metadata["goldenbee.package"], "graph-second");
    assert_eq!(metadata["goldenbee.instance"], instance_id);

    package.shutdown().await;
}

fn instance_name(data: &Path) -> String {
    format!(
        "graph-second-instance-{}",
        data.file_name().unwrap().to_string_lossy()
    )
}
