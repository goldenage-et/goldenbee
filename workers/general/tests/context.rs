//! Long-conversation context bounds through native ADK Runner compaction.
//!
//! These tests drive the packaged General Worker through the shared runtime.
//! They assert public request bounds, canonical native history, inspectable
//! compaction events, provider compatibility, and caller isolation. They do
//! not assert a private summary implementation.

use adk_rust::session::GetRequest;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use common::{
    ContextCompaction, Limits, Principal, ProfileRevision, SubmitTaskRequest, Task, TaskState,
};
use runtime::worker::WorkerInstance;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod fixture;

const MODEL_MAX_CHARS: usize = 8_000;

#[derive(Clone)]
struct RecordingModel {
    max_chars: usize,
    delay_ms: u64,
    requests: Arc<Mutex<Vec<usize>>>,
    over_limit: Arc<AtomicUsize>,
}

async fn model_handler(
    State(model): State<RecordingModel>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let chars = string_chars(&body);
    model.requests.lock().expect("request log").push(chars);
    if chars > model.max_chars {
        model.over_limit.fetch_add(1, Ordering::SeqCst);
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "maximum context length exceeded",
                "limit": model.max_chars,
                "observed": chars,
            })),
        )
            .into_response();
    }
    if model.delay_ms > 0 && body.to_string().contains("slow-marker") {
        tokio::time::sleep(Duration::from_millis(model.delay_ms)).await;
    }
    Json(serde_json::json!({"text": "fixture-ok"})).into_response()
}

fn string_chars(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::String(text) => text.chars().count(),
        serde_json::Value::Array(values) => values.iter().map(string_chars).sum(),
        serde_json::Value::Object(map) => map.values().map(string_chars).sum(),
        _ => 0,
    }
}

async fn model_endpoint(delay_ms: u64) -> (String, Arc<Mutex<Vec<usize>>>, Arc<AtomicUsize>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let over_limit = Arc::new(AtomicUsize::new(0));
    let state = RecordingModel {
        max_chars: MODEL_MAX_CHARS,
        delay_ms,
        requests: requests.clone(),
        over_limit: over_limit.clone(),
    };
    let app = Router::new()
        .route("/generate", post(model_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind model fixture");
    let address = listener.local_addr().expect("model address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}/generate"), requests, over_limit)
}

fn append_http_provider(config: &Path, id: &str, endpoint: &str, window: u64) {
    let mut text = std::fs::read_to_string(config).expect("worker config");
    text.push_str(&format!(
        "\n[providers.{id}]\nadapter = \"http-json\"\nmodel = \"fixture-model\"\nendpoint = \"{endpoint}\"\nmodalities = [\"text\"]\ntool_calling = true\nlocal = true\nresidency = \"local\"\ncontext_window = {window}\ncost_rank = 0\ndata_class = \"local\"\n"
    ));
    std::fs::write(config, text).expect("provider config");
}

fn append_provider(config: &Path, id: &str, window: Option<u64>) {
    let mut text = std::fs::read_to_string(config).expect("worker config");
    let mut block = format!(
        "\n[providers.{id}]\nadapter = \"deterministic\"\nmodel = \"{id}\"\nmodalities = [\"text\"]\ntool_calling = true\nlocal = true\nresidency = \"local\"\ncost_rank = 0\ndata_class = \"local\"\n"
    );
    if let Some(window) = window {
        block.push_str(&format!("context_window = {window}\n"));
    }
    text.push_str(&block);
    std::fs::write(config, text).expect("provider config");
}

fn compaction(budget: u64, preserve_recent: usize, max_retries: usize) -> ContextCompaction {
    ContextCompaction {
        strategy: "truncation".into(),
        context_budget: budget,
        preserve_recent,
        max_retries,
    }
}

fn revision(model: &str, budget: u64) -> ProfileRevision {
    ProfileRevision {
        revision: 0,
        instruction: "Answer from the conversation history.".into(),
        model: model.into(),
        capabilities: vec!["general".into()],
        grants: vec!["task:submit".into()],
        restrictions: Vec::new(),
        composition: "single".into(),
        members: vec!["general".into()],
        skills: Vec::new(),
        mcp: Vec::new(),
        workflow: None,
        limits: Limits::default(),
        compaction: Some(compaction(budget, 3, 2)),
        secret_refs: Vec::new(),
        created_at: 0,
    }
}

fn request(session: &str, input: String, key: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: "general".into(),
        session_id: session.into(),
        input,
        idempotency_key: key.into(),
        profile: Some("context".into()),
        team: None,
        worker: None,
        limits: Limits::default(),
    }
}

fn long_input(marker: &str, chars: usize) -> String {
    format!("{marker}:{}", "x".repeat(chars))
}

async fn start(config: &Path) -> WorkerInstance {
    general::worker()
        .expect("package definition")
        .start(config)
        .await
        .expect("worker starts")
}

async fn wait(runtime: &runtime::Runtime, task_id: &str) -> Task {
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

async fn wait_running(runtime: &runtime::Runtime, task_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let task = runtime.task_unscoped(task_id).await.expect("task exists");
        if task.state == TaskState::Running {
            return;
        }
        assert!(Instant::now() < deadline, "task never ran: {task:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn create_and_activate(
    runtime: &runtime::Runtime,
    admin: &Principal,
    revision: ProfileRevision,
) {
    runtime
        .create_profile(admin, "local", "context", revision)
        .await
        .expect("profile created");
    runtime
        .activate_profile(admin, "local", "context", 1)
        .await
        .expect("profile activated");
}

async fn session_events(
    instance: &WorkerInstance,
    principal: &str,
    session: &str,
) -> Vec<adk_rust::Event> {
    let user =
        runtime::native::executor::native_user_id("local", principal).expect("native identity");
    let session = instance
        .session_service()
        .get(GetRequest {
            app_name: "general".into(),
            user_id: user,
            session_id: session.into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("canonical session is readable");
    session.events().all()
}

fn contents(events: &[adk_rust::Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| {
            content.parts.iter().filter_map(|part| match part {
                adk_rust::Part::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .collect()
}

async fn compaction_events(runtime: &runtime::Runtime, task_id: &str) -> Vec<serde_json::Value> {
    runtime
        .events_since(task_id, 0, 10_000)
        .await
        .expect("task events are readable")
        .events
        .into_iter()
        .filter(|event| event.kind == "context.compacted")
        .map(|event| event.payload)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_conversation_compacts_before_over_limit_and_keeps_canonical_turns() {
    let (endpoint, requests, over_limit) = model_endpoint(0).await;
    let data = tempfile::tempdir().expect("data directory");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "context-bounds".into(),
            token: Some("dev-token".into()),
            model: "fixture".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    append_http_provider(&fixture.config, "fixture", &endpoint, 2_000);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    let alice = Principal::new("alice", "local", &["*"]);
    create_and_activate(&runtime, &admin, revision("fixture", 1_300)).await;

    let turns: Vec<String> = (1..=6)
        .map(|number| long_input(&format!("turn{number}"), 1_500))
        .collect();
    assert!(
        turns.iter().map(String::len).sum::<usize>() > MODEL_MAX_CHARS,
        "the conversation must be longer than the model request bound"
    );

    let mut last = None;
    let mut task_ids = Vec::new();
    for (index, input) in turns.iter().enumerate() {
        let submitted = runtime
            .submit_task(
                &alice,
                request("bounded-session", input.clone(), &format!("bound-{index}")),
            )
            .await
            .expect("task admitted");
        let task = wait(&runtime, &submitted.task_id).await;
        assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
        assert_eq!(task.profile_revision, 1);
        assert!(task.output.as_deref().is_some_and(|text| !text.is_empty()));
        task_ids.push(task.id.clone());
        last = Some(task);
    }
    let last = last.expect("a final task");

    assert_eq!(
        over_limit.load(Ordering::SeqCst),
        0,
        "no request may exceed the configured model context"
    );
    let observed = requests.lock().expect("request log").clone();
    assert!(!observed.is_empty());
    assert!(
        observed.iter().all(|chars| *chars <= MODEL_MAX_CHARS),
        "requests exceeded the model bound: {observed:?}"
    );
    assert!(
        observed.iter().any(|chars| *chars < 6_000),
        "at least one compacted request must be materially smaller: {observed:?}"
    );

    let compacted = compaction_events(&runtime, &last.id).await;
    assert!(
        !compacted.is_empty(),
        "native compaction must leave an inspectable event"
    );
    assert_eq!(compacted[0]["strategy"], "truncation");
    assert_eq!(compacted[0]["context_budget"], 1_300);
    assert_eq!(compacted[0]["profile_revision"], 1);
    assert!(
        compacted[0]["events_after"].as_u64() <= compacted[0]["events_before"].as_u64(),
        "compaction must not invent events: {compacted:?}"
    );
    assert!(runtime
        .traces_for(&last.id)
        .await
        .iter()
        .any(|trace| { trace.span == "context.compaction" && trace.task_id == last.id }));

    let events = session_events(&instance, "alice", "bounded-session").await;
    let text = contents(&events).join("\n");
    for input in &turns {
        assert!(text.contains(input), "canonical turn was lost");
    }
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compaction_keeps_artifact_evidence_and_denies_foreign_history() {
    let data = tempfile::tempdir().expect("data directory");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "context-artifact".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    let alice = Principal::new(
        "alice",
        "local",
        &["task:submit", "task:read", "session:write", "artifact:read"],
    );
    let mut profile = revision("deterministic", 500);
    profile.compaction = Some(compaction(500, 2, 2));
    create_and_activate(&runtime, &admin, profile).await;

    let first = runtime
        .submit_task(
            &alice,
            request(
                "shared-session",
                "artifact:source-evidence".into(),
                "artifact-1",
            ),
        )
        .await
        .expect("artifact task admitted");
    let first = wait(&runtime, &first.task_id).await;
    assert_eq!(first.state, TaskState::Succeeded, "{first:?}");
    let artifacts = runtime
        .artifacts(&alice, &first.id)
        .await
        .expect("artifact evidence is accessible");
    let artifact = artifacts.first().expect("artifact was finalized").clone();
    let content = runtime
        .read_artifact(&alice, &artifact.id)
        .await
        .expect("artifact bytes are accessible");
    assert_eq!(content.bytes, b"source-evidence");

    let mut last = first.clone();
    for number in 0..3 {
        let submitted = runtime
            .submit_task(
                &alice,
                request(
                    "shared-session",
                    long_input(&format!("alice-long-{number}"), 1_400),
                    &format!("alice-{number}"),
                ),
            )
            .await
            .expect("long turn admitted");
        let task = wait(&runtime, &submitted.task_id).await;
        assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
        last = task;
    }

    let compacted = compaction_events(&runtime, &last.id).await;
    assert!(!compacted.is_empty());

    let events = session_events(&instance, "alice", "shared-session").await;
    let text = contents(&events).join("\n");
    assert!(text.contains("artifact:source-evidence"));
    assert!(events.iter().any(|event| {
        event
            .actions
            .artifact_delta
            .keys()
            .any(|name| name == "evidence.txt")
    }));
    assert_eq!(
        runtime
            .artifacts(&alice, &first.id)
            .await
            .expect("artifact still listed")
            .len(),
        1
    );

    // The same session id under another authenticated principal is a separate
    // canonical history and cannot read the first caller's task or evidence.
    let bob = Principal::new(
        "bob",
        "local",
        &["task:submit", "task:read", "session:write"],
    );
    let submitted = runtime
        .submit_task(
            &bob,
            request("shared-session", "bob-question".into(), "bob-1"),
        )
        .await
        .expect("foreign task admitted under its own identity");
    let bob_task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(bob_task.state, TaskState::Succeeded, "{bob_task:?}");
    let output = bob_task.output.clone().unwrap_or_default();
    assert!(!output.contains("source-evidence"));
    assert!(!output.contains("alice-long"));
    let bob_events = session_events(&instance, "bob", "shared-session").await;
    let bob_text = contents(&bob_events).join("\n");
    assert!(!bob_text.contains("source-evidence"));
    assert!(!bob_text.contains("alice-long"));
    assert!(runtime.task(&bob, &first.id).await.is_err());
    assert!(
        bob_events
            .iter()
            .all(|event| event.actions.compaction.is_none()
                || event.actions.artifact_delta.is_empty())
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revision_changes_cancellation_and_restart_keep_truthful_context() {
    let (endpoint, requests, over_limit) = model_endpoint(600).await;
    let data = tempfile::tempdir().expect("data directory");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "context-revision".into(),
            token: Some("dev-token".into()),
            model: "fixture".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    append_http_provider(&fixture.config, "fixture", &endpoint, 3_000);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    let alice = Principal::new("alice", "local", &["*"]);
    let mut first = revision("fixture", 1_600);
    first.compaction = Some(compaction(1_600, 3, 2));
    create_and_activate(&runtime, &admin, first).await;

    let submitted = runtime
        .submit_task(
            &alice,
            request(
                "revision-session",
                long_input("revision-one", 1_700),
                "revision-1",
            ),
        )
        .await
        .expect("task admitted");
    let task_one = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task_one.state, TaskState::Succeeded, "{task_one:?}");
    assert_eq!(task_one.profile_revision, 1);

    // Revising after admission cannot rewrite the pinned task revision.
    let mut second = revision("fixture", 1_500);
    second.compaction = Some(compaction(1_500, 3, 2));
    let profile = runtime
        .revise_profile(&admin, "local", "context", 1, second)
        .await
        .expect("profile revised");
    runtime
        .activate_profile(&admin, "local", "context", 2)
        .await
        .expect("revision two active");
    assert_eq!(profile.revisions.len(), 2);

    let before_slow = requests.lock().expect("request log").len();
    let slow = runtime
        .submit_task(
            &alice,
            request(
                "revision-session",
                format!("slow-marker:{}", long_input("cancel-me", 2_000)),
                "revision-2",
            ),
        )
        .await
        .expect("slow task admitted");
    wait_running(&runtime, &slow.task_id).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while requests.lock().expect("request log").len() <= before_slow {
        assert!(
            Instant::now() < deadline,
            "the cancelled turn never reached the model"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    runtime
        .cancel_task(&alice, &slow.task_id)
        .await
        .expect("cancellation requested");
    let cancelled = wait(&runtime, &slow.task_id).await;
    assert_eq!(cancelled.state, TaskState::Cancelled, "{cancelled:?}");
    assert_eq!(cancelled.profile_revision, 2);

    let task_before_restart = runtime
        .task_unscoped(&task_one.id)
        .await
        .expect("task exists");
    let compaction_before = compaction_events(&runtime, &task_one.id).await;
    if let Some(event) = compaction_before.first() {
        assert_eq!(event["profile_revision"], 1);
        assert_eq!(event["context_budget"], 1_600);
    }

    instance.shutdown().await;
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let alice = Principal::new("alice", "local", &["*"]);

    let after = runtime
        .task_unscoped(&task_before_restart.id)
        .await
        .expect("task projection survives restart");
    assert_eq!(after.profile_revision, 1);
    assert_eq!(after.state, TaskState::Succeeded);
    let restored_cancel = runtime
        .task_unscoped(&slow.task_id)
        .await
        .expect("cancelled task survives restart");
    assert_eq!(restored_cancel.state, TaskState::Cancelled);

    let submitted = runtime
        .submit_task(
            &alice,
            request(
                "revision-session",
                long_input("after-restart", 3_000),
                "revision-3",
            ),
        )
        .await
        .expect("follow-up admitted");
    let task_after = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task_after.state, TaskState::Succeeded, "{task_after:?}");
    assert_eq!(task_after.profile_revision, 2);
    let compacted = compaction_events(&runtime, &task_after.id).await;
    assert!(!compacted.is_empty());
    assert_eq!(compacted[0]["profile_revision"], 2);
    assert_eq!(compacted[0]["context_budget"], 1_500);

    let events = session_events(&instance, "alice", "revision-session").await;
    let text = contents(&events).join("\n");
    assert!(text.contains("revision-one"));
    assert!(text.contains("cancel-me"));
    assert!(text.contains("after-restart"));
    assert_eq!(over_limit.load(Ordering::SeqCst), 0);
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incompatible_provider_compaction_is_actionable_before_admission() {
    let data = tempfile::tempdir().expect("data directory");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "context-incompatible".into(),
            token: Some("dev-token".into()),
            model: "small".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    append_provider(&fixture.config, "small", Some(512));
    append_provider(&fixture.config, "unknown-window", None);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    let alice = Principal::new("alice", "local", &["*"]);

    let error = runtime
        .create_profile(&admin, "local", "too-small", revision("small", 600))
        .await
        .expect_err("a compaction budget above the provider window must fail");
    assert_eq!(error.code, "provider.compaction_context_unsupported");

    let error = runtime
        .create_profile(
            &admin,
            "local",
            "unknown-window",
            revision("unknown-window", 100),
        )
        .await
        .expect_err("an undeclared provider window must fail");
    assert_eq!(error.code, "provider.compaction_context_unknown");

    let mut unsupported = revision("deterministic", 100);
    unsupported.compaction = Some(ContextCompaction {
        strategy: "summarize".into(),
        context_budget: 100,
        preserve_recent: 4,
        max_retries: 2,
    });
    let error = runtime
        .create_profile(&admin, "local", "unsupported-strategy", unsupported)
        .await
        .expect_err("an unverified strategy must fail instead of guessing an API");
    assert_eq!(error.code, "profile.compaction_strategy_unsupported");

    // A profile may validate against the declaration of its own model while an
    // agent-only invocation resolves a different, incompatible provider.
    let mut base_only = revision("deterministic", 600);
    base_only.members.clear();
    base_only.capabilities.clear();
    runtime
        .create_profile(&admin, "local", "context", base_only)
        .await
        .expect("profile creation accepts the declared deterministic window");
    runtime
        .activate_profile(&admin, "local", "context", 1)
        .await
        .expect("profile activates");
    let error = runtime
        .submit_task(
            &alice,
            request("incompatible-session", "hello".into(), "incompatible-1"),
        )
        .await
        .expect_err("admission must reject the actual agent provider");
    assert_eq!(error.code, "provider.compaction_context_unsupported");
    assert!(
        runtime
            .tasks(&alice, None)
            .await
            .expect("task list")
            .is_empty(),
        "an incompatible request must not be recorded"
    );
    instance.shutdown().await;
}
