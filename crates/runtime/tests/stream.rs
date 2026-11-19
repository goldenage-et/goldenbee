//! In-process public SSE acceptance for bounded fan-out and durable approval
//! replay. These tests drive the real Axum route and a real HTTP connection.

use common::{ApprovalDecision, Limits, PackageManifest, Principal, SubmitTaskRequest, TaskState};
use runtime::serving::{api_router, authenticate, WorkerState};
use runtime::{ExecutionOutcome, Executor, Runtime, RuntimeConfig};
use std::path::Path;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "stream-runtime-token";

struct Burst {
    delay_ms: u64,
    events: usize,
}

#[async_trait::async_trait]
impl Executor for Burst {
    async fn execute(
        &self,
        runtime: Runtime,
        task: common::Task,
        _cancel: CancellationToken,
    ) -> ExecutionOutcome {
        tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        for index in 0..self.events {
            let _ = runtime
                .append_event(
                    &task.id,
                    "model.delta",
                    serde_json::json!({"text": index.to_string()}),
                    false,
                )
                .await;
        }
        ExecutionOutcome::success("burst complete")
    }

    async fn cancel(&self, _task_id: &str) -> bool {
        false
    }
}

struct Sleeper;

#[async_trait::async_trait]
impl Executor for Sleeper {
    async fn execute(
        &self,
        _runtime: Runtime,
        _task: common::Task,
        cancel: CancellationToken,
    ) -> ExecutionOutcome {
        tokio::select! {
            _ = cancel.cancelled() => ExecutionOutcome::cancelled(),
            _ = tokio::time::sleep(Duration::from_secs(30)) => ExecutionOutcome::cancelled(),
        }
    }

    async fn cancel(&self, _task_id: &str) -> bool {
        false
    }
}

fn config(directory: &Path) -> RuntimeConfig {
    let mut config = RuntimeConfig::local(directory);
    config.tokens.clear();
    config
        .tokens
        .insert(TOKEN.into(), Principal::new("local", "local", &["*"]));
    config
}

fn manifest() -> PackageManifest {
    PackageManifest {
        name: "stream-test".into(),
        version: "0.1.0".into(),
        protocol: common::PROTOCOL_VERSION.into(),
        runtime: "0.1.0".into(),
        adk: "2.3.0".into(),
        capabilities: vec![common::CapabilityDescriptor {
            id: "general".into(),
            kind: "agent".into(),
            description: "stream test agent".into(),
            permissions: Vec::new(),
        }],
        required_permissions: Vec::new(),
        features: Vec::new(),
        launcher_modes: Vec::new(),
        hooks: Vec::new(),
    }
}

async fn serve(runtime: Runtime) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let manifest = manifest();
    let state = WorkerState::new(runtime, manifest);
    let app = authenticate(api_router(state.clone()), state);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}"), handle)
}

async fn submit(runtime: &Runtime, key: &str) -> String {
    runtime
        .submit_task(
            &Principal::new("local", "local", &["*"]),
            SubmitTaskRequest {
                namespace: Some("local".into()),
                agent: "general".into(),
                session_id: format!("stream-{key}"),
                input: "stream".into(),
                idempotency_key: key.into(),
                profile: None,
                team: None,
                worker: None,
                limits: Limits::default(),
            },
        )
        .await
        .expect("submit")
        .task_id
}

async fn open(base: &str, task_id: &str, after: u64) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{base}/v1/tasks/{task_id}/events/stream?after={after}"
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("stream opens")
}

async fn read_until(response: &mut reqwest::Response, needle: &str, seconds: u64) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let mut body = Vec::new();
    loop {
        if String::from_utf8_lossy(&body).contains(needle) {
            return String::from_utf8_lossy(&body).into_owned();
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "stream never produced {needle}");
        match tokio::time::timeout(remaining, response.chunk()).await {
            Ok(Ok(Some(chunk))) => body.extend_from_slice(&chunk),
            Ok(Ok(None)) => break,
            Ok(Err(error)) => panic!("stream failed: {error}"),
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&body).into_owned()
}

async fn wait_state(runtime: &Runtime, task_id: &str, wanted: TaskState) -> common::Task {
    for _ in 0..300 {
        let task = runtime.task_unscoped(task_id).await.expect("task");
        if task.state == wanted {
            return task;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("task never reached {wanted:?}");
}

#[tokio::test]
async fn lagged_subscriber_gets_typed_notice_and_never_stalls_execution() {
    let directory = tempfile::tempdir().expect("data directory");
    let mut config = config(directory.path());
    config.stream_buffer = 1;
    config.max_streams = 4;
    let runtime = Runtime::open(config).await.expect("runtime");
    runtime
        .set_executor(std::sync::Arc::new(Burst {
            delay_ms: 300,
            events: 2_000,
        }))
        .await;
    let (base, server) = serve(runtime.clone()).await;
    let task_id = submit(&runtime, "lagged").await;

    let mut response = open(&base, &task_id, 0).await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    // Do not read while the executor emits more events than the one-slot
    // broadcast buffer can hold.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let body = read_until(&mut response, "stream.lagged", 10).await;
    assert!(body.contains("\"kind\":\"stream.lagged\""), "{body}");
    assert!(body.contains("\"recovery\""), "{body}");

    // Execution is independent of the slow viewer.
    let task = wait_state(&runtime, &task_id, TaskState::Succeeded).await;
    assert_eq!(task.output.as_deref(), Some("burst complete"));
    drop(response);
    server.abort();
}

#[tokio::test]
async fn approval_transitions_replay_over_sse_after_runtime_restart() {
    let directory = tempfile::tempdir().expect("data directory");
    let config = config(directory.path());
    let runtime = Runtime::open(config.clone()).await.expect("runtime");
    runtime.set_executor(std::sync::Arc::new(Sleeper)).await;
    let (base, server) = serve(runtime.clone()).await;
    let task_id = submit(&runtime, "approval").await;
    wait_state(&runtime, &task_id, TaskState::Running).await;

    let principal = Principal::new("local", "local", &["*"]);
    let approval = runtime
        .request_approval(&task_id, "write", "receipt.txt", "digest-1", "low", 60_000)
        .await
        .expect("approval requested");
    let approval_id = approval.id.clone();

    // First viewer observes the request, then disconnects.
    let mut first = open(&base, &task_id, 0).await;
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let observed = read_until(&mut first, "approval.requested", 10).await;
    assert!(observed.contains("\"durable\":true"), "{observed}");
    drop(first);

    // Restart: stop serving, drop the runtime, reopen the same durable store.
    server.abort();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let runtime = Runtime::open(config).await.expect("reopened runtime");
    let (base, server) = serve(runtime.clone()).await;

    let mut replay = open(&base, &task_id, 0).await;
    let replayed = read_until(&mut replay, "approval.requested", 10).await;
    assert!(
        replayed.matches("\"kind\":\"approval.requested\"").count() == 1,
        "approval request must replay exactly once: {replayed}"
    );

    runtime
        .decide_approval(
            &principal,
            &approval_id,
            ApprovalDecision {
                decision: "approved".into(),
                digest: "digest-1".into(),
            },
        )
        .await
        .expect("approval decided");

    let mut decided = open(&base, &task_id, 0).await;
    let body = read_until(&mut decided, "approval.approved", 10).await;
    assert_eq!(
        body.matches("\"kind\":\"approval.approved\"").count(),
        1,
        "approval decision must be delivered exactly once: {body}"
    );
    assert!(body.contains("\"kind\":\"approval.requested\""), "{body}");
    drop(decided);
    server.abort();
}
