//! Headless one-shot, JSONL, input bounds, and session continuation through
//! the shared runtime.
//!
//! These tests observe the external contract: the returned result, the ordered
//! frames, the exit class, and durable admission. They never inspect executor
//! internals or a private frame cursor.

use adk_rust::session::{AppendEventRequest, CreateRequest, Event, SessionService};
use adk_rust::{AdkIdentity, AppName, Content, Part, SessionId, UserId};
use common::{
    ApiError, JsonlFrame, JsonlItem, Principal, RunImage, RunInput, RunRequest, RunSource,
    TaskState, MAX_RUN_FILE_BYTES,
};
use runtime::{ExecutionOutcome, Executor, Runtime};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const APP: &str = "goldenbee";

struct Scripted {
    events: Vec<(&'static str, serde_json::Value, bool)>,
    outcome: ExecutionOutcome,
    seen_images: Arc<std::sync::Mutex<usize>>,
}

#[async_trait::async_trait]
impl Executor for Scripted {
    async fn execute(
        &self,
        runtime: Runtime,
        task: common::Task,
        _cancel: CancellationToken,
    ) -> ExecutionOutcome {
        *self.seen_images.lock().expect("image slot") = task.input_images.len();
        for (kind, payload, durable) in &self.events {
            runtime
                .append_event(&task.id, kind, payload.clone(), *durable)
                .await
                .expect("event appends");
        }
        self.outcome.clone()
    }

    async fn cancel(&self, _task_id: &str) -> bool {
        false
    }
}

fn scripted(outcome: ExecutionOutcome) -> (Arc<dyn Executor>, Arc<std::sync::Mutex<usize>>) {
    let images = Arc::new(std::sync::Mutex::new(0));
    let executor = Scripted {
        events: vec![
            (
                "tool.call",
                serde_json::json!({
                    "name": "read_file",
                    "id": "call-1",
                    "agent": "echo",
                }),
                true,
            ),
            (
                "tool.result",
                serde_json::json!({
                    "name": "read_file",
                    "id": "call-1",
                    "agent": "echo",
                    "ok": true,
                }),
                true,
            ),
            (
                "approval.requested",
                serde_json::json!({
                    "approval_id": "approval-1",
                    "operation": "user.ask",
                    "target": "continue?",
                    "risk": "low",
                }),
                true,
            ),
            (
                "approval.approved",
                serde_json::json!({"approval_id": "approval-1"}),
                true,
            ),
            (
                "checkpoint.committed",
                serde_json::json!({
                    "checkpoint_id": "checkpoint-1",
                    "status": "closed",
                }),
                true,
            ),
            (
                "model.delta",
                serde_json::json!({"text": "hel", "author": "echo"}),
                true,
            ),
            (
                "model.delta.complete",
                serde_json::json!({"text": "hello", "author": "echo"}),
                true,
            ),
        ],
        outcome,
        seen_images: images.clone(),
    };
    (Arc::new(executor), images)
}

async fn runtime_with(executor: Arc<dyn Executor>) -> (Runtime, tempfile::TempDir) {
    let (runtime, directory) = Runtime::temporary().await.expect("runtime");
    runtime.set_executor(executor).await;
    let sessions = Arc::new(
        runtime::native::FileSessionService::open(
            directory.path().join(runtime::native::SESSION_STORE_FILE),
        )
        .expect("session store"),
    );
    runtime.set_sessions(sessions).await;
    (runtime, directory)
}

fn request(input: RunInput) -> RunRequest {
    RunRequest {
        agent: Some("echo".into()),
        input,
        ..RunRequest::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn one_shot_run_returns_a_structured_result_and_success_exit() {
    let (runtime, _directory) = runtime_with(scripted(ExecutionOutcome::success("hello")).0).await;
    let principal = Principal::new("local", "local", &["*"]);
    let mut frames = Vec::new();
    let result = runtime
        .run_headless(&principal, request(RunInput::text("hi")), &mut |frame| {
            frames.push(frame.clone());
            Ok(())
        })
        .await
        .expect("run succeeds");

    assert_eq!(result.state, TaskState::Succeeded);
    assert_eq!(result.output.as_deref(), Some("hello"));
    assert_eq!(result.exit_code, common::exit::SUCCESS);
    assert_eq!(result.session_id, result.session_id.trim());
    assert!(!result.task_id.is_empty());
    assert_eq!(frames.first().map(|f| kind(f)), Some("task_started"));
    assert_eq!(frames.last().map(|f| kind(f)), Some("task_completed"));
    let terminal = frames.last().unwrap();
    assert!(matches!(
        terminal,
        JsonlFrame::TaskCompleted {
            exit_code: 0,
            state: TaskState::Succeeded,
            ..
        }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn jsonl_stream_orders_task_tool_approval_diff_and_terminal_items() {
    let (runtime, _directory) = runtime_with(scripted(ExecutionOutcome::success("hello")).0).await;
    let principal = Principal::new("local", "local", &["*"]);
    let mut frames = Vec::new();
    let result = runtime
        .run_headless(&principal, request(RunInput::text("hi")), &mut |frame| {
            frames.push(frame.clone());
            Ok(())
        })
        .await
        .expect("run succeeds");
    assert_eq!(result.exit_code, 0);

    let kinds: Vec<_> = frames.iter().map(kind).collect();
    assert_eq!(kinds.first(), Some(&"task_started"));
    assert_eq!(kinds.last(), Some(&"task_completed"));
    assert!(kinds.contains(&"turn_started"));

    let items: Vec<_> = frames
        .iter()
        .filter_map(item)
        .map(|(item_type, id)| (item_type.to_string(), id.to_string()))
        .collect();
    assert_eq!(
        items,
        vec![
            ("tool_call".into(), "call-1".into()),
            ("tool_call".into(), "call-1".into()),
            ("approval".into(), "approval-1".into()),
            ("approval".into(), "approval-1".into()),
            ("diff".into(), "checkpoint-1".into()),
            ("agent_message".into(), "message:echo".into()),
            ("agent_message".into(), "message:echo".into()),
        ]
    );

    // The same call is projected once as started and once as completed even
    // when both the runner and the tool kernel record it.
    let started = frames
        .iter()
        .filter(|frame| matches!(frame, JsonlFrame::ItemStarted { .. }))
        .count();
    let completed = frames
        .iter()
        .filter(|frame| matches!(frame, JsonlFrame::ItemCompleted { .. }))
        .count();
    assert_eq!(started, 2);
    assert_eq!(completed, 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_run_keeps_its_truthful_exit_class() {
    let (runtime, _directory) = runtime_with(
        scripted(ExecutionOutcome::failure(ApiError::new(
            "execution.failed",
            "the native invocation failed",
        )))
        .0,
    )
    .await;
    let principal = Principal::new("local", "local", &["*"]);
    let result = runtime
        .run_headless(&principal, request(RunInput::text("hi")), &mut |_| Ok(()))
        .await
        .expect("run returns a projection");
    assert_eq!(result.state, TaskState::Failed);
    assert_eq!(result.exit_code, common::exit::FAILURE);
    assert_eq!(
        result.error.as_ref().map(|error| error.code.as_str()),
        Some("execution.failed")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_input_fails_before_admission_and_does_not_echo_values() {
    let (runtime, _directory) = runtime_with(scripted(ExecutionOutcome::success("ok")).0).await;
    let principal = Principal::new("local", "local", &["*"]);

    let error = runtime
        .run_headless(&principal, request(RunInput::default()), &mut |_| Ok(()))
        .await
        .unwrap_err();
    assert_eq!(error.code, "validation.run_input");
    assert!(runtime.all_tasks().await.is_empty());

    let oversized = RunImage {
        media_type: "image/png".into(),
        data: "seeded-secret-value".into(),
        name: Some("private-file.png".into()),
    };
    let error = runtime
        .run_headless(
            &principal,
            request(RunInput {
                text: "look".into(),
                images: vec![oversized],
                ..RunInput::default()
            }),
            &mut |_| Ok(()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "validation.run_image");
    assert!(!format!("{error:?}").contains("seeded-secret-value"));
    assert!(!format!("{error:?}").contains("private-file.png"));
    assert!(runtime.all_tasks().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn admission_rejects_an_unauthorized_run_with_the_shared_exit_class() {
    let (runtime, _directory) = runtime_with(scripted(ExecutionOutcome::success("ok")).0).await;
    let principal = Principal::new("local", "local", &[common::scopes::TASK_READ]);
    let error = runtime
        .run_headless(&principal, request(RunInput::text("hi")), &mut |_| Ok(()))
        .await
        .unwrap_err();
    assert_eq!(error.code, "scope.denied");
    assert_eq!(common::error_exit_code(&error), common::exit::AUTHORIZATION);
    assert!(runtime.all_tasks().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn bounded_images_reach_the_executor_as_native_input() {
    let (runtime, _directory) = runtime_with(scripted(ExecutionOutcome::success("ok")).0).await;
    let (_, images) = {
        let executor = Scripted {
            events: Vec::new(),
            outcome: ExecutionOutcome::success("ok"),
            seen_images: Arc::new(std::sync::Mutex::new(0)),
        };
        let seen = executor.seen_images.clone();
        runtime.set_executor(Arc::new(executor)).await;
        ((), seen)
    };
    let principal = Principal::new("local", "local", &["*"]);
    let result = runtime
        .run_headless(
            &principal,
            request(RunInput {
                text: "describe".into(),
                images: vec![RunImage::from_bytes(
                    "image/png",
                    b"\x89PNG\r\n\x1a\n",
                    Some("diagram.png".into()),
                )],
                ..RunInput::default()
            }),
            &mut |_| Ok(()),
        )
        .await
        .expect("run succeeds");
    assert_eq!(result.exit_code, 0);
    assert_eq!(*images.lock().expect("images"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_and_continue_use_the_visible_canonical_session() {
    let (runtime, _directory) = runtime_with(scripted(ExecutionOutcome::success("ok")).0).await;
    let principal = Principal::new("local", "local", &["*"]);
    create_session(&runtime, "local", "local", "existing").await;
    create_session(&runtime, "local", "local", "newest").await;

    let mut resume = request(RunInput::text("again"));
    resume.resume_session = Some("existing".into());
    let result = runtime
        .run_headless(&principal, resume, &mut |_| Ok(()))
        .await
        .expect("resume succeeds");
    assert_eq!(result.session_id, "existing");

    let mut resume = request(RunInput::text("again"));
    resume.continue_session = true;
    let result = runtime
        .run_headless(&principal, resume, &mut |_| Ok(()))
        .await
        .expect("continue succeeds");
    assert_eq!(result.session_id, "newest");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bounded_wait_reports_uncertainty_instead_of_success() {
    struct Slow;

    #[async_trait::async_trait]
    impl Executor for Slow {
        async fn execute(
            &self,
            _runtime: Runtime,
            _task: common::Task,
            _cancel: CancellationToken,
        ) -> ExecutionOutcome {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            ExecutionOutcome::success("late")
        }

        async fn cancel(&self, _task_id: &str) -> bool {
            true
        }
    }

    let (runtime, _directory) = runtime_with(Arc::new(Slow)).await;
    let principal = Principal::new("local", "local", &["*"]);
    let mut request = request(RunInput::text("hi"));
    request.wait_ms = Some(10);
    let mut frames = Vec::new();
    let result = runtime
        .run_headless(&principal, request, &mut |frame| {
            frames.push(frame.clone());
            Ok(())
        })
        .await
        .expect("run returns a projection");
    assert_eq!(result.exit_code, common::exit::UNCERTAIN);
    assert!(result.uncertainty.is_some());
    assert!(matches!(
        frames.last(),
        Some(JsonlFrame::TaskCompleted { exit_code: 6, .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delegation_cannot_widen_the_headless_agent_or_limits() {
    let (runtime, _directory) = runtime_with(scripted(ExecutionOutcome::success("ok")).0).await;
    let principal = Principal::new("local", "local", &["*"]);
    let claims = common::delegation::DelegationClaims {
        issuer: "gateway".into(),
        audience: "worker".into(),
        principal: "local".into(),
        namespace: "local".into(),
        scopes: vec![common::scopes::TASK_SUBMIT.into()],
        capabilities: vec!["general".into()],
        expires_at: common::now_ms() + 60_000,
        depth: 0,
        max_depth: 1,
        limits: common::Limits {
            max_duration_ms: 100,
            ..common::Limits::default()
        },
        token_id: "token-1".into(),
        team: None,
        path: Vec::new(),
    };
    let mut widened = request(RunInput::text("hi"));
    widened.limits.max_duration_ms = 5_000;
    let error = runtime
        .run_headless_with_claims(&principal, widened, Some(&claims), &mut |_| Ok(()))
        .await
        .unwrap_err();
    assert_eq!(error.code, "scope.capability_denied");
    assert!(runtime.all_tasks().await.is_empty());

    let narrowed = common::delegation::DelegationClaims {
        capabilities: vec!["echo".into()],
        ..claims
    };
    let mut narrowed_request = request(RunInput::text("hi"));
    narrowed_request.limits.max_duration_ms = 5_000;
    let result = runtime
        .run_headless_with_claims(&principal, narrowed_request, Some(&narrowed), &mut |_| {
            Ok(())
        })
        .await
        .expect("delegated run succeeds");
    let task = runtime.task_unscoped(&result.task_id).await.expect("task");
    assert_eq!(task.limits.max_duration_ms, 100);
    assert_eq!(result.exit_code, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn another_principal_cannot_resume_a_session() {
    let (runtime, _directory) = runtime_with(scripted(ExecutionOutcome::success("ok")).0).await;
    create_session(&runtime, "local", "local", "existing").await;
    let other = Principal::new("other", "local", &["*"]);
    let mut resume = request(RunInput::text("again"));
    resume.resume_session = Some("existing".into());
    let error = runtime
        .run_headless(&other, resume, &mut |_| Ok(()))
        .await
        .unwrap_err();
    assert_eq!(error.code, "session.not_found");
    assert!(runtime.all_tasks().await.is_empty());
}

async fn create_session(runtime: &Runtime, namespace: &str, principal: &str, session: &str) {
    let sessions = runtime.sessions().await.expect("session store");
    let user = runtime::native::native_user_id(namespace, principal).expect("native user");
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: user.clone(),
            session_id: Some(session.into()),
            state: HashMap::new(),
        })
        .await
        .expect("session created");
    let mut event = Event::new("inv-1");
    event.author = "user".into();
    let mut content = Content::new("user");
    content.parts.push(Part::Text {
        text: "prior turn".into(),
    });
    event.llm_response.content = Some(content);
    sessions
        .append_event_for_identity(AppendEventRequest {
            identity: AdkIdentity::new(
                AppName::try_from(APP).expect("app"),
                UserId::try_from(user).expect("user"),
                SessionId::try_from(session).expect("session"),
            ),
            event,
        })
        .await
        .expect("event appended");
}

#[test]
fn local_sources_are_assembled_and_bounded_before_admission() {
    let directory = tempfile::tempdir().expect("directory");
    let file = directory.path().join("notes.txt");
    std::fs::write(&file, "file body").expect("file");
    let input = runtime::resolve_sources(
        &[
            RunSource::Text {
                text: "prompt".into(),
            },
            RunSource::Stdin,
            RunSource::File {
                path: file.display().to_string(),
                media_type: None,
            },
        ],
        &mut Cursor::new("piped"),
    )
    .expect("sources resolve");
    assert_eq!(
        input.text,
        "prompt\npiped\n<file name=\"notes.txt\">\nfile body\n</file>"
    );
    assert_eq!(input.files.len(), 1);
    assert_eq!(input.files[0].name, "notes.txt");

    let oversized = directory.path().join("large.txt");
    std::fs::write(&oversized, vec![b'a'; MAX_RUN_FILE_BYTES + 1]).expect("file");
    let error = runtime::resolve_sources(
        &[RunSource::File {
            path: oversized.display().to_string(),
            media_type: None,
        }],
        &mut Cursor::new(""),
    )
    .unwrap_err();
    assert_eq!(error.code, "validation.run_source");
    assert!(!error.message.contains(directory.path().to_str().unwrap()));
}

#[test]
fn local_source_errors_name_a_position_not_a_host_path() {
    let directory = tempfile::tempdir().expect("directory");
    let missing = directory.path().join("secret-notes.txt");
    let error = runtime::resolve_sources(
        &[RunSource::File {
            path: missing.display().to_string(),
            media_type: None,
        }],
        &mut Cursor::new(""),
    )
    .unwrap_err();
    assert_eq!(error.code, "validation.run_source");
    assert!(!format!("{error:?}").contains("secret-notes"));
    assert!(!format!("{error:?}").contains(directory.path().to_str().unwrap()));
}

fn kind(frame: &JsonlFrame) -> &'static str {
    match frame {
        JsonlFrame::TaskStarted { .. } => "task_started",
        JsonlFrame::TurnStarted { .. } => "turn_started",
        JsonlFrame::ItemStarted { .. } => "item_started",
        JsonlFrame::ItemUpdated { .. } => "item_updated",
        JsonlFrame::ItemCompleted { .. } => "item_completed",
        JsonlFrame::TaskCompleted { .. } => "task_completed",
    }
}

fn item(frame: &JsonlFrame) -> Option<(&'static str, &str)> {
    let item: &JsonlItem = match frame {
        JsonlFrame::ItemStarted { item, .. }
        | JsonlFrame::ItemUpdated { item, .. }
        | JsonlFrame::ItemCompleted { item, .. } => item,
        _ => return None,
    };
    Some((
        match item {
            JsonlItem::AgentMessage { .. } => "agent_message",
            JsonlItem::Reasoning { .. } => "reasoning",
            JsonlItem::ToolCall { .. } => "tool_call",
            JsonlItem::Approval { .. } => "approval",
            JsonlItem::Diff { .. } => "diff",
        },
        item.id(),
    ))
}
