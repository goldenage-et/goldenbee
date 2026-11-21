//! Diagnostic composition keeps audit, trace, metric, and approval evidence
//! distinct while exposing actionable conditions without prompt text.

use common::{ApiError, Limits, Principal, SubmitTaskRequest, Task, TaskState};
use runtime::{ExecutionOutcome, Executor, Runtime};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const CANARY: &str = "sk-diagnostics-canary-do-not-export";

struct BlockingExecutor;

#[async_trait::async_trait]
impl Executor for BlockingExecutor {
    async fn execute(
        &self,
        runtime: Runtime,
        task: Task,
        _cancel: CancellationToken,
    ) -> ExecutionOutcome {
        let approval = match runtime
            .request_approval(
                &task.id,
                "protected_write",
                CANARY,
                "sha256:exact-operation",
                "write",
                60_000,
            )
            .await
        {
            Ok(approval) => approval,
            Err(error) => return ExecutionOutcome::failure(error),
        };
        match runtime
            .wait_for_approval(&approval.id, Duration::from_secs(10))
            .await
        {
            Ok(approval) if approval.status == "approved" => ExecutionOutcome::success("approved"),
            Ok(approval) => ExecutionOutcome::failure(ApiError::new(
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

fn request(key: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: "general".into(),
        session_id: "session-diagnostics-secret".into(),
        input: format!("prompt canary {CANARY}"),
        idempotency_key: key.into(),
        profile: None,
        team: None,
        worker: None,
        limits: Limits::default(),
    }
}

#[tokio::test]
async fn diagnostics_expose_blocked_approval_without_prompt_or_secret_export() {
    let (runtime, _directory) = Runtime::temporary().await.unwrap();
    runtime.set_executor(Arc::new(BlockingExecutor)).await;
    let principal = Principal::new("caller", "local", &["*"]);
    let trace = "0123456789abcdef0123456789abcdef";
    let response = runtime
        .submit_task_with_trace(
            &principal,
            request("diagnostic-approval"),
            None,
            Some(trace),
        )
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let task = runtime.task_unscoped(&response.task_id).await.unwrap();
        if task.state == TaskState::WaitingApproval {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{task:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mut attributes = BTreeMap::new();
    attributes.insert("tool".into(), format!("value {CANARY}"));
    runtime
        .trace(trace, &response.task_id, "tool.call", 4, attributes)
        .await
        .unwrap();

    let bundle = runtime
        .diagnostics(&principal, Some(&response.task_id))
        .await
        .unwrap();
    assert_eq!(bundle.correlation.trace_id.as_deref(), Some(trace));
    assert_eq!(
        bundle.correlation.task_id.as_deref(),
        Some(response.task_id.as_str())
    );
    assert_eq!(
        bundle.correlation.attempt,
        bundle.task.as_ref().unwrap().attempt
    );
    assert_eq!(bundle.task.as_ref().unwrap().traffic, "local");
    assert_eq!(
        bundle.task.as_ref().unwrap().state,
        TaskState::WaitingApproval
    );
    assert!(bundle
        .findings
        .iter()
        .any(|finding| finding.kind == "blocked_approval"));
    assert_eq!(bundle.task.as_ref().unwrap().approvals.len(), 1);
    assert!(bundle.audit.iter().any(|record| {
        record["action"] == "task.submit"
            && record["resource"] == response.task_id
            && record["outcome"] == "accepted"
            && record["policy_revision"] == 1
            && record["correlation"] == trace
    }));

    // Metrics use the bounded vocabulary only.
    let labels: Vec<&str> = bundle
        .metrics
        .iter()
        .flat_map(|sample| sample.labels.keys().map(String::as_str))
        .collect();
    assert!(!labels.contains(&"task_id"));
    assert!(!labels.contains(&"session_id"));
    let prompt = bundle
        .metrics
        .iter()
        .find(|sample| sample.name == "tasks.usage.prompt_tokens")
        .expect("usage metric");
    assert_eq!(prompt.value, None);
    assert_eq!(prompt.quality, common::MetricQuality::Unknown);

    let serialized = serde_json::to_string(&bundle).unwrap();
    assert!(!serialized.contains(CANARY), "diagnostics leaked a secret");
    assert!(
        !serialized.contains("session-diagnostics-secret"),
        "diagnostics leaked a session id"
    );
    assert!(
        !serialized.contains("prompt canary"),
        "diagnostics leaked prompt content"
    );

    // Audit retention is independent of sampled telemetry: removing the trace
    // sample does not remove the security decision.
    let approval = bundle.task.as_ref().unwrap().approvals[0].clone();
    runtime
        .decide_approval(
            &principal,
            &approval.id,
            common::ApprovalDecision {
                decision: "approved".into(),
                digest: approval.digest,
                answer: None,
            },
        )
        .await
        .unwrap();
    let _ = runtime.task_unscoped(&response.task_id).await.unwrap();
}
