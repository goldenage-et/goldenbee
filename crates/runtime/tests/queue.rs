//! Runtime admission evidence for paused work.
//!
//! A native workflow that pauses for an exact approval must not retain an
//! execution permit. The per-identity writing lock stays in force, so a
//! follow-up turn for the same session still waits while an unrelated session
//! uses the freed capacity.

use common::{ApprovalDecision, Limits, Principal, SubmitTaskRequest, Task, TaskState};
use runtime::{ExecutionOutcome, Executor, Runtime, RuntimeConfig};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Test executor that pauses one nominated task on a real runtime approval.
struct PausingExecutor;

#[async_trait::async_trait]
impl Executor for PausingExecutor {
    async fn execute(
        &self,
        runtime: Runtime,
        task: Task,
        _cancel: CancellationToken,
    ) -> ExecutionOutcome {
        if task.input != "pause for approval" {
            return ExecutionOutcome::success(format!("done: {}", task.input));
        }
        let approval = match runtime
            .request_approval(
                &task.id,
                "protected_write",
                "target.txt",
                "sha256:protected-write-digest",
                "write",
                60_000,
            )
            .await
        {
            Ok(approval) => approval,
            Err(error) => return ExecutionOutcome::failure(error),
        };
        match runtime
            .wait_for_approval(&approval.id, Duration::from_secs(15))
            .await
        {
            Ok(approval) if approval.status == "approved" => {
                ExecutionOutcome::success("approved effect")
            }
            Ok(approval) => ExecutionOutcome::failure(common::ApiError::new(
                "approval.not_approved",
                format!("approval ended as {}", approval.status),
            )),
            Err(error) => ExecutionOutcome::failure(error),
        }
    }

    async fn cancel(&self, _task_id: &str) -> bool {
        false
    }
}

fn request(session: &str, input: &str, key: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: None,
        agent: "general".into(),
        session_id: session.into(),
        input: input.into(),
        idempotency_key: key.into(),
        profile: None,
        team: None,
        worker: None,
        limits: Limits::default(),
    }
}

async fn wait_for_state(runtime: &Runtime, task_id: &str, wanted: TaskState) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let task = runtime
            .task_unscoped(task_id)
            .await
            .expect("task remains visible");
        if task.state == wanted {
            return task;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task {task_id} stayed in {:?}, wanted {wanted:?}",
            task.state
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_terminal(runtime: &Runtime, task_id: &str) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let task = runtime
            .task_unscoped(task_id)
            .await
            .expect("task remains visible");
        if task.state.is_terminal() {
            return task;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task {task_id} did not finish: {:?}",
            task.state
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_active(runtime: &Runtime, active: u32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let health = runtime.health().await;
        if health.capacity.active == active {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "active count stayed at {}, wanted {active}",
            health.capacity.active
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waiting_approval_releases_the_execution_slot_but_keeps_session_order() {
    let directory = tempfile::tempdir().expect("temporary data directory");
    let runtime = Runtime::open(RuntimeConfig::local(directory.path()).with_limits(1, 3, 4))
        .await
        .expect("runtime opens");
    runtime.set_executor(Arc::new(PausingExecutor)).await;
    let principal = Principal::new("local", "local", &["*"]);

    let paused = runtime
        .submit_task(
            &principal,
            request("shared-session", "pause for approval", "pause-1"),
        )
        .await
        .expect("paused task is admitted");
    wait_for_state(&runtime, &paused.task_id, TaskState::WaitingApproval).await;
    // The paused native workflow released its only execution slot.
    wait_for_active(&runtime, 0).await;

    // Same-session writing turns remain ordered even though the slot is free.
    let same_session = runtime
        .submit_task(
            &principal,
            request("shared-session", "quick follow-up", "pause-2"),
        )
        .await
        .expect("same-session task is admitted");
    // A different session uses the freed slot immediately.
    let peer = runtime
        .submit_task(
            &principal,
            request("other-session", "quick peer", "pause-3"),
        )
        .await
        .expect("peer task is admitted");
    let peer = wait_for_terminal(&runtime, &peer.task_id).await;
    assert_eq!(peer.state, TaskState::Succeeded);
    assert_eq!(peer.output.as_deref(), Some("done: quick peer"));

    // The same-session turn is still waiting for the writing lock, not running.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let blocked = runtime
        .task_unscoped(&same_session.task_id)
        .await
        .expect("follow-up remains visible");
    assert!(
        !matches!(
            blocked.state,
            TaskState::Running | TaskState::Succeeded | TaskState::WaitingApproval
        ),
        "same-session writes must not proceed while the first turn is paused: {:?}",
        blocked.state
    );

    let pending = runtime
        .approvals(&principal, true)
        .await
        .expect("approvals");
    let approval = pending
        .iter()
        .find(|approval| approval.task_id == paused.task_id)
        .expect("paused task has a pending approval");
    runtime
        .decide_approval(
            &principal,
            &approval.id,
            ApprovalDecision {
                decision: "approved".into(),
                digest: approval.digest.clone(),
            },
        )
        .await
        .expect("approval is decided");

    let finished = wait_for_terminal(&runtime, &paused.task_id).await;
    assert_eq!(finished.state, TaskState::Succeeded);
    assert_eq!(finished.output.as_deref(), Some("approved effect"));

    let follow_up = wait_for_terminal(&runtime, &same_session.task_id).await;
    assert_eq!(follow_up.state, TaskState::Succeeded);
    assert_eq!(follow_up.output.as_deref(), Some("done: quick follow-up"));
    wait_for_active(&runtime, 0).await;
}
