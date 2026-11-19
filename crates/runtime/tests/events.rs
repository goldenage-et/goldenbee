//! Ephemeral stream events stay out of durable state.

use common::{Limits, Principal, SubmitTaskRequest, TaskState};
use runtime::{ExecutionOutcome, Executor, Runtime};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct Streamer;

#[async_trait::async_trait]
impl Executor for Streamer {
    async fn execute(
        &self,
        runtime: Runtime,
        task: common::Task,
        _cancel: CancellationToken,
    ) -> ExecutionOutcome {
        for index in 0..50 {
            runtime
                .append_event(
                    &task.id,
                    "model.delta",
                    serde_json::json!({"chunk": index}),
                    false,
                )
                .await
                .expect("ephemeral append");
        }
        ExecutionOutcome::success("done")
    }

    async fn cancel(&self, _task_id: &str) -> bool {
        false
    }
}

#[tokio::test]
async fn ephemeral_events_are_broadcast_but_not_persisted() {
    let (runtime, directory) = Runtime::temporary().await.unwrap();
    runtime.set_executor(Arc::new(Streamer)).await;
    let principal = Principal::new("local", "local", &["*"]);
    let response = runtime
        .submit_task(
            &principal,
            SubmitTaskRequest {
                namespace: Some("local".into()),
                agent: "echo".into(),
                session_id: "stream".into(),
                input: "stream".into(),
                idempotency_key: "stream".into(),
                profile: None,
                team: None,
                worker: None,
                limits: Limits::default(),
            },
        )
        .await
        .unwrap();

    for _ in 0..200 {
        let task = runtime.task_unscoped(&response.task_id).await.unwrap();
        if task.state.is_terminal() {
            assert_eq!(task.state, TaskState::Succeeded);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let page = runtime
        .events_since(&response.task_id, 0, 1_000)
        .await
        .unwrap();
    assert!(
        page.events.iter().all(|event| event.kind != "model.delta"),
        "ephemeral deltas must not be retained"
    );
    assert!(page
        .events
        .iter()
        .any(|event| event.kind == "task.succeeded"));

    let persisted = std::fs::read_to_string(directory.path().join("state.json")).unwrap();
    assert!(!persisted.contains("model.delta"));
}
