//! Ticket 27 budget accounting and enforcement through the shared runtime.
//!
//! These tests use the real packaged General Worker, native ADK Runner, and
//! durable task projections. They assert public task results and durable events
//! rather than runtime internals.

use common::{Limits, Principal, ProfileRevision, StreamEvent, SubmitTaskRequest, Task, TaskState};
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod fixture;

fn revision(limits: Limits) -> ProfileRevision {
    revision_with_model("deterministic", "single", &[], limits)
}

fn revision_with_model(
    model: &str,
    composition: &str,
    members: &[&str],
    limits: Limits,
) -> ProfileRevision {
    ProfileRevision {
        revision: 0,
        instruction: "BUDGET".into(),
        model: model.into(),
        capabilities: vec!["general".into()],
        grants: vec!["task:submit".into()],
        restrictions: Vec::new(),
        composition: composition.into(),
        members: members.iter().map(|value| (*value).into()).collect(),
        skills: Vec::new(),
        mcp: Vec::new(),
        workflow: None,
        limits,
        compaction: None,
        secret_refs: Vec::new(),
        created_at: 0,
    }
}

fn request(
    profile: &str,
    session: &str,
    input: &str,
    key: &str,
    limits: Limits,
) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: "general".into(),
        session_id: session.into(),
        input: input.into(),
        idempotency_key: key.into(),
        profile: Some(profile.into()),
        team: None,
        worker: None,
        limits,
    }
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

async fn start(config: &Path) -> runtime::worker::WorkerInstance {
    general::worker()
        .expect("package definition")
        .start(config)
        .await
        .expect("worker starts")
}

fn fixture(data: &Path, instance: &str) -> fixture::Fixture {
    fixture::write(
        data,
        fixture::Options {
            instance: instance.into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    )
}

async fn create(runtime: &runtime::Runtime, admin: &Principal, limits: Limits) {
    create_revision(runtime, admin, revision(limits)).await;
}

async fn create_revision(runtime: &runtime::Runtime, admin: &Principal, revision: ProfileRevision) {
    runtime
        .create_profile(admin, "local", "budget", revision)
        .await
        .expect("profile created");
    runtime
        .activate_profile(admin, "local", "budget", 1)
        .await
        .expect("profile activated");
}

fn budget_events(events: &[StreamEvent]) -> Vec<&StreamEvent> {
    events
        .iter()
        .filter(|event| event.kind.starts_with("budget."))
        .collect()
}

fn with_pricing(config: &Path) {
    let mut text = std::fs::read_to_string(config).expect("worker config");
    text.push_str(
        "\n[providers.deterministic]\nadapter = \"deterministic\"\nmodel = \"deterministic\"\nmodalities = [\"text\"]\ntool_calling = true\nlocal = true\nresidency = \"local\"\ncontext_window = 131072\ncost_rank = 0\ndata_class = \"local\"\ninput_cost_micros_per_million = 1000000\ncandidate_cost_micros_per_million = 2000000\n",
    );
    std::fs::write(config, text).expect("pricing config");
}

fn stub(status: u16, body: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let address = listener.local_addr().expect("fixture address");
    let count = Arc::new(AtomicUsize::new(0));
    let requests = count.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut socket) = stream else { continue };
            requests.fetch_add(1, Ordering::SeqCst);
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match socket.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                    Err(_) => break,
                }
                if let Some(header_end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header_end = header_end + 4;
                    let text = String::from_utf8_lossy(&buffer);
                    let length = text
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buffer.len() >= header_end + length {
                        break;
                    }
                }
            }
            let reason = if status == 200 { "OK" } else { "Rate Limited" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes());
            let _ = socket.flush();
        }
    });
    (format!("http://{address}/generate"), count)
}

fn with_fallback_providers(config: &Path, primary: &str, backup: &str) {
    let mut text = std::fs::read_to_string(config).expect("worker config");
    text.push_str(&format!(
        "\n[providers.primary]\nadapter = \"http-json\"\nmodel = \"primary\"\nendpoint = \"{primary}\"\nmodalities = [\"text\"]\ntool_calling = true\nlocal = true\nresidency = \"local\"\ncontext_window = 8192\ncost_rank = 0\ndata_class = \"local\"\nfallbacks = [\"backup\"]\n\n[providers.backup]\nadapter = \"http-json\"\nmodel = \"backup\"\nendpoint = \"{backup}\"\nmodalities = [\"text\"]\ntool_calling = true\nlocal = true\nresidency = \"local\"\ncontext_window = 8192\ncost_rank = 0\ndata_class = \"local\"\n"
    ));
    std::fs::write(config, text).expect("provider config");
}

fn event_kinds(events: &[StreamEvent], kind: &str) -> usize {
    events.iter().filter(|event| event.kind == kind).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_separates_provider_reported_values_from_unavailable_and_estimated_values() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "budget-usage");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(&runtime, &admin, Limits::default()).await;

    let known = runtime
        .submit_task(
            &admin,
            request(
                "budget",
                "usage-known",
                "KNOWN-USAGE",
                "usage-known",
                Limits::default(),
            ),
        )
        .await
        .expect("known usage task admitted");
    let known = wait(&runtime, &known.task_id).await;
    assert_eq!(known.state, TaskState::Succeeded, "{known:?}");
    assert_eq!(known.usage.model_calls, 1, "{known:?}");
    assert!(
        known.usage.prompt_tokens.is_some(),
        "provider-reported prompt tokens must be recorded: {known:?}"
    );
    assert!(
        known.usage.candidate_tokens.is_some(),
        "provider-reported candidate tokens must be recorded: {known:?}"
    );
    assert_eq!(
        known.usage.estimated_tokens, None,
        "a fully reported call must not gain an invented estimate: {known:?}"
    );

    let unknown = runtime
        .submit_task(
            &admin,
            request(
                "budget",
                "usage-unknown",
                "usage-unknown UNKNOWN-USAGE",
                "usage-unknown",
                Limits::default(),
            ),
        )
        .await
        .expect("unknown usage task admitted");
    let unknown = wait(&runtime, &unknown.task_id).await;
    assert_eq!(unknown.state, TaskState::Succeeded, "{unknown:?}");
    assert_eq!(
        unknown.usage.model_calls, 1,
        "a known model call is counted even when the provider omits token counts: {unknown:?}"
    );
    assert_eq!(
        unknown.usage.prompt_tokens, None,
        "missing provider prompt tokens are unavailable, not zero: {unknown:?}"
    );
    assert_eq!(
        unknown.usage.candidate_tokens, None,
        "missing provider candidate tokens are unavailable, not zero: {unknown:?}"
    );
    assert!(
        unknown
            .usage
            .estimated_tokens
            .is_some_and(|value| value > 0),
        "unknown provider counts must expose a separate nonzero estimate: {unknown:?}"
    );
    let wire = serde_json::to_value(&unknown.usage).expect("usage serializes");
    assert!(
        wire.get("prompt_tokens").is_none(),
        "unavailable provider counts must not serialize as zero: {wire}"
    );
    assert!(
        wire.get("estimated_tokens").is_some(),
        "the harness estimate must be distinguishable on the wire: {wire}"
    );

    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn model_call_budget_stops_before_pending_tool_dispatch_and_records_policy_event() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "budget-model-calls");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        Limits {
            max_model_calls: 1,
            ..Limits::default()
        },
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "budget",
                "model-call-budget",
                r#"tool-loop:2:exec_command {"command":"true"} api_key=super-secret-canary"#,
                "model-call-budget",
                Limits::default(),
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(
        task.error.as_ref().map(|error| error.code.as_str()),
        Some("budget.model_calls_exceeded"),
        "{task:?}"
    );
    assert_eq!(task.usage.model_calls, 1, "{task:?}");
    assert_eq!(
        task.usage.tool_calls, 0,
        "no tool may run after the model-call budget is exhausted: {task:?}"
    );

    let events = events(&runtime, &task.id).await;
    assert_eq!(
        event_kinds(&events, "tool.result"),
        0,
        "a pending tool dispatch must not run: {events:?}"
    );
    let policy = budget_events(&events);
    let exhausted = policy
        .iter()
        .find(|event| event.kind == "budget.exhausted")
        .expect("durable budget policy event");
    assert_eq!(exhausted.payload["kind"], "model_calls", "{exhausted:?}");
    assert_eq!(exhausted.payload["limit"], 1, "{exhausted:?}");
    assert_eq!(exhausted.payload["used"], 1, "{exhausted:?}");
    assert!(
        exhausted.payload["recovery"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "a budget stop must expose an authorized recovery option: {exhausted:?}"
    );
    assert!(
        exhausted.payload["usage"]["prompt_tokens"].is_u64(),
        "numeric accounting must survive redaction: {exhausted:?}"
    );
    assert!(
        !exhausted
            .payload
            .to_string()
            .contains("super-secret-canary"),
        "budget accounting must not expose request secrets: {exhausted:?}"
    );

    // The stopped task released its execution permit and another session runs.
    let after = runtime
        .submit_task(
            &admin,
            request(
                "default",
                "after-model-budget",
                "AFTER-MODEL-BUDGET",
                "after-model-budget",
                Limits::default(),
            ),
        )
        .await
        .expect("unrelated task admitted");
    let after = wait(&runtime, &after.task_id).await;
    assert_eq!(after.state, TaskState::Succeeded, "{after:?}");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_call_budget_allows_the_limit_and_stops_the_next_dispatch() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "budget-tool-calls");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        Limits {
            max_tool_calls: 1,
            ..Limits::default()
        },
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "budget",
                "tool-call-budget",
                r#"tool-loop:2:exec_command {"command":"true"}"#,
                "tool-call-budget",
                Limits::default(),
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(
        task.error.as_ref().map(|error| error.code.as_str()),
        Some("budget.tool_calls_exceeded"),
        "{task:?}"
    );
    assert_eq!(
        task.usage.tool_calls, 1,
        "the allowed tool call must still be accounted: {task:?}"
    );

    let events = events(&runtime, &task.id).await;
    assert_eq!(
        event_kinds(&events, "tool.result"),
        1,
        "exactly the allowed tool call may execute: {events:?}"
    );
    let exhausted = budget_events(&events)
        .into_iter()
        .find(|event| event.kind == "budget.exhausted")
        .expect("durable budget policy event");
    assert_eq!(exhausted.payload["kind"], "tool_calls", "{exhausted:?}");
    assert_eq!(exhausted.payload["limit"], 1, "{exhausted:?}");
    assert_eq!(exhausted.payload["used"], 1, "{exhausted:?}");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn token_budget_uses_reported_usage_and_discloses_non_preemptive_enforcement() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "budget-tokens");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        Limits {
            max_tokens: 1,
            ..Limits::default()
        },
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "budget",
                "token-budget",
                r#"tool-loop:2:exec_command {"command":"true"}"#,
                "token-budget",
                Limits::default(),
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(
        task.error.as_ref().map(|error| error.code.as_str()),
        Some("budget.tokens_exceeded"),
        "{task:?}"
    );
    assert_eq!(
        task.usage.tool_calls, 0,
        "a token stop observed before dispatch must prevent the tool call: {task:?}"
    );
    let events = events(&runtime, &task.id).await;
    let exhausted = budget_events(&events)
        .into_iter()
        .find(|event| event.kind == "budget.exhausted")
        .expect("durable budget policy event");
    assert_eq!(exhausted.payload["kind"], "tokens", "{exhausted:?}");
    assert_eq!(
        exhausted.payload["exact_preemptive"], false,
        "token budgets must not claim exact preemptive enforcement: {exhausted:?}"
    );
    assert!(
        exhausted.payload["uncertainty"]
            .as_str()
            .is_some_and(|value| value.contains("reported")),
        "the policy event must disclose the enforcement limit: {exhausted:?}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_usage_with_a_token_budget_records_an_explicit_disclosure() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "budget-unknown");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        Limits {
            max_tokens: 10_000,
            ..Limits::default()
        },
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "budget",
                "unknown-budget",
                "usage-unknown marker",
                "unknown-budget",
                Limits::default(),
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert_eq!(task.usage.prompt_tokens, None, "{task:?}");
    assert!(
        task.usage.estimated_tokens.is_some(),
        "an unavailable provider value still exposes a harness estimate: {task:?}"
    );
    let events = events(&runtime, &task.id).await;
    let disclosure = budget_events(&events)
        .into_iter()
        .find(|event| event.kind == "budget.usage_unavailable")
        .expect("unknown usage with a token budget must be disclosed");
    assert_eq!(disclosure.payload["kind"], "tokens", "{disclosure:?}");
    assert!(
        disclosure.payload["uncertainty"]
            .as_str()
            .is_some_and(|value| value.contains("provider")),
        "{disclosure:?}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deadline_records_a_policy_event_stops_only_its_task_and_releases_capacity() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "budget-deadline");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(&runtime, &admin, Limits::default()).await;

    let slow = runtime
        .submit_task(
            &admin,
            request(
                "budget",
                "deadline-slow",
                "delay:600 slow",
                "deadline-slow",
                Limits {
                    max_duration_ms: 100,
                    ..Limits::default()
                },
            ),
        )
        .await
        .expect("slow task admitted");

    let fast = runtime
        .submit_task(
            &admin,
            request(
                "default",
                "deadline-fast",
                "FAST-UNRELATED",
                "deadline-fast",
                Limits::default(),
            ),
        )
        .await
        .expect("unrelated task admitted");
    let fast = wait(&runtime, &fast.task_id).await;
    assert_eq!(fast.state, TaskState::Succeeded, "{fast:?}");

    let slow = wait(&runtime, &slow.task_id).await;
    assert_eq!(slow.state, TaskState::Failed, "{slow:?}");
    assert_eq!(
        slow.error.as_ref().map(|error| error.code.as_str()),
        Some("budget.deadline_exceeded"),
        "{slow:?}"
    );
    let events = events(&runtime, &slow.id).await;
    let exhausted = budget_events(&events)
        .into_iter()
        .find(|event| event.kind == "budget.exhausted")
        .expect("deadline must record a durable policy event");
    assert_eq!(exhausted.payload["kind"], "deadline", "{exhausted:?}");
    assert_eq!(exhausted.payload["limit"], 100, "{exhausted:?}");

    // Capacity was released, not left reserved by the timed-out attempt.
    let after = runtime
        .submit_task(
            &admin,
            request(
                "default",
                "deadline-after",
                "AFTER-DEADLINE",
                "deadline-after",
                Limits::default(),
            ),
        )
        .await
        .expect("task after timeout admitted");
    let after = wait(&runtime, &after.task_id).await;
    assert_eq!(after.state, TaskState::Succeeded, "{after:?}");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_is_not_reported_as_budget_exhaustion_and_releases_capacity() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "budget-cancel");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(&runtime, &admin, Limits::default()).await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "budget",
                "cancel-session",
                "delay:600 cancel me",
                "cancel-me",
                Limits::default(),
            ),
        )
        .await
        .expect("task admitted");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let task = runtime
            .task_unscoped(&submitted.task_id)
            .await
            .expect("task");
        if task.state == TaskState::Running {
            break;
        }
        assert!(Instant::now() < deadline, "never ran: {task:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    runtime
        .cancel_task(&admin, &submitted.task_id)
        .await
        .expect("cancel accepted");
    let cancelled = wait(&runtime, &submitted.task_id).await;
    assert_eq!(cancelled.state, TaskState::Cancelled, "{cancelled:?}");
    let events = events(&runtime, &cancelled.id).await;
    assert!(
        budget_events(&events).is_empty(),
        "cancellation must not fabricate a budget policy event: {events:?}"
    );

    let after = runtime
        .submit_task(
            &admin,
            request(
                "default",
                "cancel-after",
                "AFTER-CANCEL",
                "cancel-after",
                Limits::default(),
            ),
        )
        .await
        .expect("task after cancel admitted");
    let after = wait(&runtime, &after.task_id).await;
    assert_eq!(after.state, TaskState::Succeeded, "{after:?}");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_idempotent_submission_does_not_double_budget_or_leak_accounting_input() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "budget-repeat");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        Limits {
            max_model_calls: 1,
            ..Limits::default()
        },
    )
    .await;

    let request = request(
        "budget",
        "repeat-session",
        "api_key=super-secret-canary",
        "repeat-key",
        Limits::default(),
    );
    let first = runtime
        .submit_task(&admin, request.clone())
        .await
        .expect("first admission");
    let task = wait(&runtime, &first.task_id).await;
    assert_eq!(task.usage.model_calls, 1, "{task:?}");

    let replay = runtime
        .submit_task(&admin, request)
        .await
        .expect("replay admission");
    assert!(replay.replayed, "the original handle must be returned");
    assert_eq!(replay.task_id, first.task_id);
    let replayed = wait(&runtime, &replay.task_id).await;
    assert_eq!(
        replayed.usage.model_calls, 1,
        "replay must not spend or duplicate the task budget: {replayed:?}"
    );
    assert_eq!(
        event_kinds(&events(&runtime, &replayed.id).await, "task.accepted"),
        1,
        "a replay must not create a second accounting task"
    );

    let serialized = serde_json::to_string(&budget_payloads(&runtime, &replayed.id).await)
        .expect("budget payloads serialize");
    assert!(
        !serialized.contains("super-secret-canary"),
        "budget accounting output must not include request input: {serialized}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_pricing_produces_a_cost_estimate_separate_from_provider_tokens() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "budget-cost");
    with_pricing(&fixture.config);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(&runtime, &admin, Limits::default()).await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "budget",
                "cost-estimate",
                "COST-ESTIMATE",
                "cost-estimate",
                Limits::default(),
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert!(task.usage.prompt_tokens.is_some(), "{task:?}");
    assert!(
        task.usage
            .estimated_cost_micros
            .is_some_and(|value| value > 0),
        "operator-declared pricing must produce a separate cost estimate: {task:?}"
    );
    let wire = serde_json::to_value(&task.usage).expect("usage serializes");
    assert!(
        wire.get("estimated_cost_micros").is_some(),
        "estimated cost must be named distinctly from provider token counts: {wire}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cost_budget_stops_after_reported_usage_with_estimation_disclosure() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "budget-cost-stop");
    with_pricing(&fixture.config);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        Limits {
            max_cost_micros: 1,
            ..Limits::default()
        },
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "budget",
                "cost-budget",
                r#"tool-loop:2:exec_command {"command":"true"}"#,
                "cost-budget",
                Limits::default(),
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(
        task.error.as_ref().map(|error| error.code.as_str()),
        Some("budget.cost_exceeded"),
        "{task:?}"
    );
    assert_eq!(
        task.usage.tool_calls, 0,
        "a cost stop observed before dispatch must prevent the tool call: {task:?}"
    );
    let events = events(&runtime, &task.id).await;
    let exhausted = budget_events(&events)
        .into_iter()
        .find(|event| event.kind == "budget.exhausted")
        .expect("durable budget policy event");
    assert_eq!(exhausted.payload["kind"], "cost", "{exhausted:?}");
    assert_eq!(
        exhausted.payload["exact_preemptive"], false,
        "cost budgets must not claim exact preemptive enforcement: {exhausted:?}"
    );
    assert!(
        exhausted.payload["uncertainty"]
            .as_str()
            .is_some_and(|value| value.contains("estimated")),
        "cost enforcement must disclose that it is an estimate: {exhausted:?}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_rate_limit_preserves_unknown_usage_and_releases_capacity() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "budget-rate-limit");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(&runtime, &admin, Limits::default()).await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "budget",
                "rate-limit-session",
                "rate-limit request",
                "rate-limit",
                Limits::default(),
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(
        task.usage.prompt_tokens, None,
        "a rate-limited call reports no provider tokens; it is not zero: {task:?}"
    );
    assert_eq!(task.usage.candidate_tokens, None, "{task:?}");
    let events = events(&runtime, &task.id).await;
    assert!(
        budget_events(&events).is_empty(),
        "a provider rate limit is not a budget exhaustion: {events:?}"
    );

    // A later attempt gets a fresh execution permit and authenticates normally.
    let after = runtime
        .submit_task(
            &admin,
            request(
                "default",
                "after-rate-limit-session",
                "AFTER LIMIT",
                "rate-limit-after",
                Limits::default(),
            ),
        )
        .await
        .expect("task after rate limit admitted");
    let after = wait(&runtime, &after.task_id).await;
    assert_eq!(after.state, TaskState::Succeeded, "{after:?}");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_fallback_consumes_the_same_task_budget_and_is_reported_once() {
    let (primary, primary_requests) = stub(429, "{}");
    let (backup, backup_requests) = stub(200, r#"{"text":"backup: ok"}"#);
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "budget-fallback".into(),
            token: Some("dev-token".into()),
            model: "primary".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    with_fallback_providers(&fixture.config, &primary, &backup);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create_revision(
        &runtime,
        &admin,
        revision_with_model(
            "primary",
            "sequential",
            &["general"],
            Limits {
                max_model_calls: 1,
                ..Limits::default()
            },
        ),
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "budget",
                "fallback-session",
                "fallback request",
                "fallback",
                Limits::default(),
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert_eq!(
        task.usage.model_calls, 1,
        "one task model call includes the policy fallback: {task:?}"
    );
    assert_eq!(
        task.resolved_model
            .as_ref()
            .map(|model| model.provider.as_str()),
        Some("backup"),
        "the provider that actually ran must be recorded: {task:?}"
    );
    assert_eq!(primary_requests.load(Ordering::SeqCst), 1);
    assert_eq!(backup_requests.load(Ordering::SeqCst), 1);
    let events = events(&runtime, &task.id).await;
    assert_eq!(
        event_kinds(&events, "model.fallback"),
        1,
        "fallback must be observable once: {events:?}"
    );
    instance.shutdown().await;
}

async fn budget_payloads(runtime: &runtime::Runtime, task_id: &str) -> Vec<serde_json::Value> {
    events(runtime, task_id)
        .await
        .into_iter()
        .filter(|event| event.kind.starts_with("budget."))
        .map(|event| event.payload)
        .collect()
}
