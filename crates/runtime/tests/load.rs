//! Ticket 49 measurements for bounded event fan-out at the Worker serving seam.
//!
//! A test executor emits deterministic event bursts through the real Axum SSE
//! route. This isolates subscriber buffering and stream-slot limits from model
//! latency and product Client behavior. The executor is test tooling, not a
//! replacement for the native ADK Runner.

use common::{Limits, PackageManifest, Principal, StreamEvent, SubmitTaskRequest, TaskState};
use load_harness::{elapsed_ns, now_ms, Distribution, Report, Sampler, WorkloadReport};
use runtime::serving::{api_router, authenticate, WorkerState};
use runtime::{ExecutionOutcome, Executor, Runtime, RuntimeConfig};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "load-stream-token";

struct Burst {
    delay_before_ms: u64,
    events: usize,
    interval_ms: u64,
}

#[async_trait::async_trait]
impl Executor for Burst {
    async fn execute(
        &self,
        runtime: Runtime,
        task: common::Task,
        _cancel: CancellationToken,
    ) -> ExecutionOutcome {
        if self.delay_before_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.delay_before_ms)).await;
        }
        for index in 0..self.events {
            let _ = runtime
                .append_event(
                    &task.id,
                    "model.delta",
                    serde_json::json!({"text": index.to_string()}),
                    false,
                )
                .await;
            if self.interval_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.interval_ms)).await;
            }
        }
        ExecutionOutcome::success("burst complete")
    }

    async fn cancel(&self, _task_id: &str) -> bool {
        false
    }
}

fn config(directory: &Path, stream_buffer: usize, max_streams: usize) -> RuntimeConfig {
    let mut config = RuntimeConfig::local(directory).with_limits(4, 32, 64);
    config.stream_buffer = stream_buffer;
    config.max_streams = max_streams;
    config.stream_keepalive_ms = 1_000;
    config.event_retention = 5_000;
    config.tokens.clear();
    config
        .tokens
        .insert(TOKEN.into(), Principal::new("local", "local", &["*"]));
    config
}

fn manifest() -> PackageManifest {
    PackageManifest {
        name: "load-stream-fixture".into(),
        version: "0.1.0".into(),
        protocol: common::PROTOCOL_VERSION.into(),
        runtime: "0.1.0".into(),
        adk: "2.3.0".into(),
        capabilities: vec![common::CapabilityDescriptor {
            id: "general".into(),
            kind: "agent".into(),
            description: "event burst fixture".into(),
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
    let state = WorkerState::new(runtime, manifest());
    let app = authenticate(api_router(state.clone()), state);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}"), handle)
}

fn request(session: &str, key: &str, input: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: Some("local".into()),
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

async fn submit(runtime: &Runtime, session: &str, key: &str, input: &str) -> String {
    runtime
        .submit_task(
            &Principal::new("local", "local", &["*"]),
            request(session, key, input),
        )
        .await
        .expect("submit")
        .task_id
}

async fn wait_terminal(runtime: &Runtime, id: &str) -> common::Task {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let task = runtime.task_unscoped(id).await.expect("task visible");
        if task.state.is_terminal() {
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn open(base: &str, task_id: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{base}/v1/tasks/{task_id}/events/stream?after=0"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("stream opens")
}

async fn read_events(response: &mut reqwest::Response, seconds: u64) -> Vec<(StreamEvent, u64)> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut body = Vec::new();
    let mut events: BTreeMap<String, (StreamEvent, u64)> = BTreeMap::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, response.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                body.extend_from_slice(&chunk);
                let text = String::from_utf8_lossy(&body);
                for block in text.split("\n\n") {
                    for line in block.lines() {
                        if let Some(data) = line.strip_prefix("data:") {
                            if let Ok(event) = serde_json::from_str::<StreamEvent>(data.trim()) {
                                events
                                    .entry(event.event_id.clone())
                                    .or_insert_with(|| (event, now_ms()));
                            }
                        }
                    }
                }
                if text.contains("\"kind\":\"task.succeeded\"") {
                    break;
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(_)) | Err(_) => break,
        }
    }
    events.into_values().collect()
}

async fn duplicate_task_count(runtime: &Runtime) -> usize {
    runtime
        .tasks(&Principal::new("local", "local", &["*"]), None)
        .await
        .map(|tasks| tasks.len())
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_subscriber_bursts_and_stream_slots_expose_bounded_limits() {
    let data = tempfile::tempdir().expect("data directory");
    let runtime = Runtime::open(config(data.path(), 4, 2))
        .await
        .expect("runtime");
    runtime
        .set_executor(Arc::new(Burst {
            delay_before_ms: 250,
            events: 2_000,
            interval_ms: 0,
        }))
        .await;
    let (base, server) = serve(runtime.clone()).await;
    let sampler = Sampler::start(
        vec![std::process::id()],
        vec![data.path().join("state.json")],
        Duration::from_millis(25),
    );

    // A viewer that does not read while the executor emits more events than
    // the four-slot broadcast buffer can hold must receive one explicit lag
    // notice, close, and never stall execution.
    let slow_started = Instant::now();
    let id = submit(&runtime, "slow-subscriber", "slow-subscriber", "burst").await;
    let mut response = open(&base, &id).await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(700)).await;
    let events = read_events(&mut response, 10).await;
    let lag_notices = events
        .iter()
        .filter(|(event, _)| event.kind == "stream.lagged")
        .count();
    assert_eq!(
        lag_notices,
        1,
        "{:?}",
        events
            .iter()
            .map(|(event, _)| &event.kind)
            .collect::<Vec<_>>()
    );
    let task = wait_terminal(&runtime, &id).await;
    assert_eq!(task.state, TaskState::Succeeded);
    let durable = runtime
        .events_since(&id, 0, 1_000)
        .await
        .expect("retained events");
    assert!(durable
        .events
        .iter()
        .any(|event| event.kind == "task.succeeded"));

    let mut slow_report = WorkloadReport::new(
        "slow-subscriber-event-burst",
        "runtime-serving-fixture",
        "test executor emits 2000 ephemeral events with stream_buffer=4",
        5,
        1,
        1,
    );
    slow_report
        .identities
        .insert("instance".into(), runtime.config().instance_id.clone());
    slow_report
        .identities
        .insert("package".into(), "load-stream-fixture".into());
    slow_report
        .limits
        .insert("stream_buffer".into(), "4".into());
    slow_report.limits.insert("max_streams".into(), "2".into());
    slow_report.limiting_resource =
        "broadcast stream_buffer=4 for the slow subscriber; execution remained independent".into();
    slow_report.accepted = 1;
    slow_report.completed = 1;
    slow_report.effects_verified = 1;
    slow_report.terminal_events = 1;
    slow_report.missing_terminal_events = 0;
    slow_report.completion_latency = Distribution::from_nanos(&[elapsed_ns(slow_started)]);
    slow_report
        .extra
        .insert("lag_notices".into(), Value::from(lag_notices));
    slow_report
        .extra
        .insert("ephemeral_events_emitted".into(), Value::from(2_000));
    slow_report.notes.push(
        "The lag notice is explicit and the durable task.succeeded event remains replayable."
            .into(),
    );
    let slow_process = sampler.finish();
    slow_report.process = Some(slow_process);

    // A prompt reader measures arrival lag for live durable envelopes and
    // receives the terminal event without reconnecting.
    runtime
        .set_executor(Arc::new(Burst {
            delay_before_ms: 50,
            events: 40,
            interval_ms: 5,
        }))
        .await;
    let id = submit(&runtime, "event-lag", "event-lag", "lag").await;
    let mut response = open(&base, &id).await;
    let started = Instant::now();
    let events = read_events(&mut response, 10).await;
    let terminal_events = events
        .iter()
        .filter(|(event, _)| event.kind == "task.succeeded")
        .count();
    let mut lags = Vec::new();
    for (event, received_at) in &events {
        let lag = received_at.saturating_sub(event.at as u64);
        lags.push(lag.saturating_mul(1_000_000));
    }
    let task = wait_terminal(&runtime, &id).await;
    assert_eq!(task.state, TaskState::Succeeded);
    assert_eq!(
        terminal_events,
        1,
        "{:?}",
        events
            .iter()
            .map(|(event, _)| event.kind.as_str())
            .collect::<Vec<_>>()
    );

    let mut lag_report = WorkloadReport::new(
        "live-event-arrival-lag",
        "runtime-serving-fixture",
        "test executor emits 40 ephemeral events with a prompt reader",
        3,
        1,
        1,
    );
    lag_report
        .identities
        .insert("instance".into(), runtime.config().instance_id.clone());
    lag_report.limits.insert("stream_buffer".into(), "4".into());
    lag_report.limiting_resource =
        "local SSE delivery and broadcast buffering; no external network hop".into();
    lag_report.accepted = 1;
    lag_report.completed = 1;
    lag_report.effects_verified = 1;
    lag_report.terminal_events = terminal_events as u64;
    lag_report.missing_terminal_events = 0;
    lag_report.event_lag = Distribution::from_nanos(&lags);
    lag_report.completion_latency = Distribution::from_nanos(&[elapsed_ns(started)]);
    lag_report.notes.push(
        "Event lag is receive-time minus the source envelope timestamp on this loopback host."
            .into(),
    );

    // Two held streams consume max_streams=2. A third is rejected with a typed
    // retryable limit while both admitted tasks still finish.
    let limit_started = Instant::now();
    let first = submit(&runtime, "slot-a", "slot-a", "slow").await;
    let second = submit(&runtime, "slot-b", "slot-b", "slow").await;
    let held_a = open(&base, &first).await;
    let held_b = open(&base, &second).await;
    assert_eq!(held_a.status(), reqwest::StatusCode::OK);
    assert_eq!(held_b.status(), reqwest::StatusCode::OK);
    let third = submit(&runtime, "slot-c", "slot-c", "slow").await;
    let rejected = open(&base, &third).await;
    let rejected_status = rejected.status();
    let rejected_body = rejected.text().await.unwrap_or_default();
    assert!(
        rejected_status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || rejected_status == reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "third stream status {rejected_status}: {rejected_body}"
    );
    assert!(
        rejected_body.contains("stream.subscriber_limit"),
        "{rejected_body}"
    );
    drop(held_a);
    drop(held_b);
    let first_task = wait_terminal(&runtime, &first).await;
    let second_task = wait_terminal(&runtime, &second).await;
    let third_task = wait_terminal(&runtime, &third).await;
    assert_eq!(first_task.state, TaskState::Succeeded);
    assert_eq!(second_task.state, TaskState::Succeeded);
    assert_eq!(third_task.state, TaskState::Succeeded);
    let mut terminal_events = 0u64;
    for id in [&first, &second, &third] {
        let page = runtime
            .events_since(id, 0, 1_000)
            .await
            .expect("retained events");
        if page
            .events
            .iter()
            .any(|event| event.kind == "task.succeeded")
        {
            terminal_events += 1;
        }
    }
    assert_eq!(terminal_events, 3);
    let duplicate_tasks = duplicate_task_count(&runtime).await;

    let mut limit_report = WorkloadReport::new(
        "bounded-stream-subscriber-limit",
        "runtime-serving-fixture",
        "two held SSE streams against max_streams=2",
        5,
        3,
        1,
    );
    limit_report
        .identities
        .insert("instance".into(), runtime.config().instance_id.clone());
    limit_report.limits.insert("max_streams".into(), "2".into());
    limit_report.accepted = 3;
    limit_report.completed = 3;
    limit_report.effects_verified = 3;
    limit_report.overload_rejections = 1;
    limit_report.terminal_events = terminal_events;
    limit_report.missing_terminal_events = 0;
    limit_report.completion_latency = Distribution::from_nanos(&[elapsed_ns(limit_started)]);
    limit_report.extra.insert(
        "rejected_status".into(),
        Value::from(rejected_status.as_u16()),
    );
    limit_report.extra.insert(
        "rejected_code".into(),
        Value::from("stream.subscriber_limit"),
    );
    limit_report.extra.insert(
        "total_tasks_after_limit".into(),
        Value::from(duplicate_tasks),
    );
    limit_report.limiting_resource =
        "stream slot semaphore (max_streams=2); execution admitted and completed independently"
            .into();
    limit_report
        .notes
        .push("Dropping held streams released capacity for later viewers.".into());

    server.abort();
    let report = Report::new(vec![slow_report, lag_report, limit_report]);
    let path = report.persist("49-runtime-stream-limits");
    assert!(path.is_file());
}

struct NoSecrets;

impl runtime::SecretSource for NoSecrets {
    fn available(&self, _reference: &str) -> bool {
        false
    }

    fn value(&self, reference: &str) -> std::result::Result<String, String> {
        Err(format!(
            "credential reference '{reference}' is not configured"
        ))
    }
}

/// Separate live-provider run. This is ignored because it depends on an
/// operator-installed Ollama model and external process scheduling. It
/// discloses provider influence rather than folding model latency into the
/// deterministic harness measurements above.
#[tokio::test]
#[ignore = "requires a live Ollama endpoint and GOLDENBEE_LIVE_OLLAMA=1"]
async fn live_supported_provider_run_is_disclosed_separately() {
    use adk_rust::{Content, LlmRequest};
    use futures::StreamExt;
    use runtime::{ProviderProfile, ProviderRegistry, ProviderRequirements};

    if std::env::var("GOLDENBEE_LIVE_OLLAMA").as_deref() != Ok("1") {
        eprintln!("live provider run skipped: set GOLDENBEE_LIVE_OLLAMA=1");
        return;
    }
    let model_id = std::env::var("GOLDENBEE_OLLAMA_MODEL").unwrap_or_else(|_| "tev1:0.8b".into());
    let endpoint = std::env::var("GOLDENBEE_OLLAMA_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:11434".into());
    let prompt = "Reply with the single word OK.";
    let profile = ProviderProfile {
        adapter: "ollama".into(),
        model: model_id.clone(),
        endpoint: Some(endpoint.clone()),
        modalities: vec!["text".into()],
        tool_calling: true,
        local: true,
        residency: "local".into(),
        context_window: Some(2_048),
        max_output_tokens: None,
        reasoning_levels: Vec::new(),
        credential: None,
        cost_rank: Some(0),
        input_cost_micros_per_million: None,
        candidate_cost_micros_per_million: None,
        cache_read_cost_micros_per_million: None,
        cache_write_cost_micros_per_million: None,
        data_class: Some("local".into()),
        fallbacks: Vec::new(),
    };
    let profiles = BTreeMap::from([("live-local".to_string(), profile)]);
    let registry = ProviderRegistry::new(profiles, false, Arc::new(NoSecrets))
        .expect("live provider registry");
    let resolved = registry
        .resolve("live-local", 1, &ProviderRequirements::text())
        .expect("live provider resolves");
    let started = Instant::now();
    let response = resolved
        .model
        .generate_content(
            LlmRequest::new(
                model_id.clone(),
                vec![Content::new("user").with_text(prompt)],
            ),
            false,
        )
        .await
        .expect("live provider request succeeds");
    let mut stream = response;
    let mut text = String::new();
    let mut thinking = String::new();
    let mut usage_reported = false;
    while let Some(item) = stream.next().await {
        let event = item.expect("live provider event");
        if event.usage_metadata.is_some() {
            usage_reported = true;
        }
        if let Some(content) = event.content {
            for part in content.parts {
                match part {
                    adk_rust::Part::Text { text: value } => text.push_str(&value),
                    adk_rust::Part::Thinking {
                        thinking: value, ..
                    } => thinking.push_str(&value),
                    _ => {}
                }
            }
        }
    }
    let elapsed = elapsed_ns(started);
    assert!(
        !text.trim().is_empty() || !thinking.trim().is_empty(),
        "live provider returned no content"
    );
    let mut report = WorkloadReport::new(
        "live-ollama-single-invocation",
        "live-provider",
        format!("live Ollama model {model_id}"),
        prompt.len(),
        1,
        1,
    );
    report.identities.insert("provider".into(), "ollama".into());
    report.identities.insert("model".into(), model_id);
    report.identities.insert("endpoint".into(), endpoint);
    report
        .identities
        .insert("external_process".into(), "ollama serve".into());
    report.accepted = 1;
    report.completed = 1;
    report.effects_verified = 1;
    report.acceptance_latency = Distribution::from_nanos(&[elapsed]);
    report.completion_latency = Distribution::from_nanos(&[elapsed]);
    report.limiting_resource =
        "external Ollama CPU inference, model scheduling, and provider streaming".into();
    report.notes.push(
        "External-model influence is disclosed. These numbers are not comparable to deterministic native-direct measurements and are not a universal SLO."
            .into(),
    );
    report
        .extra
        .insert("usage_reported".into(), Value::from(usage_reported));
    report
        .extra
        .insert("text_chars".into(), Value::from(text.len()));
    report
        .extra
        .insert("thinking_chars".into(), Value::from(thinking.len()));
    let path = Report::new(vec![report]).persist("49-live-provider");
    assert!(path.is_file());
}
