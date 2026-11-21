//! Provider selection, preflight, fallback, revocation, and secret-safety.
//!
//! These tests use real native adapters. Endpoint-backed legs point at
//! deterministic loopback fixtures, so no hosted credential or network is
//! required.

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use common::{Principal, ProfileRevision, ReasoningLevel, ResolvedModel, SubmitTaskRequest};
use runtime::native::proof::ProofHarness;
use runtime::{
    AgentRequirement, ProviderProfile, ProviderRegistry, ProviderRequirements, Runtime,
    RuntimeConfig, SecretSource,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct MapSecrets(BTreeMap<String, String>);

impl MapSecrets {
    fn empty() -> Arc<Self> {
        Arc::new(Self(BTreeMap::new()))
    }

    fn with(pairs: &[(&str, &str)]) -> Arc<Self> {
        Arc::new(Self(
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        ))
    }
}

impl SecretSource for MapSecrets {
    fn available(&self, reference: &str) -> bool {
        self.0.contains_key(reference)
    }

    fn value(&self, reference: &str) -> Result<String, String> {
        self.0
            .get(reference)
            .cloned()
            .ok_or_else(|| format!("credential reference '{reference}' is not available"))
    }
}

struct JsonStub {
    status: u16,
    body: serde_json::Value,
    count: Arc<AtomicUsize>,
}

async fn json_handler(
    State(stub): State<Arc<JsonStub>>,
    _body: axum::body::Bytes,
) -> impl IntoResponse {
    stub.count.fetch_add(1, Ordering::SeqCst);
    (
        axum::http::StatusCode::from_u16(stub.status).expect("valid status"),
        Json(stub.body.clone()),
    )
}

async fn json_stub(status: u16, body: serde_json::Value) -> (String, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/generate", post(json_handler))
        .with_state(Arc::new(JsonStub {
            status,
            body,
            count: count.clone(),
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture");
    let address = listener.local_addr().expect("fixture address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}/generate"), count)
}

struct ChatStub {
    status: u16,
    body: serde_json::Value,
    count: Arc<AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

async fn chat_handler(
    State(stub): State<Arc<ChatStub>>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    stub.count.fetch_add(1, Ordering::SeqCst);
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) {
        stub.requests
            .lock()
            .expect("request recorder is not poisoned")
            .push(value);
    }
    let status = axum::http::StatusCode::from_u16(stub.status).expect("valid status");
    if stub.status >= 400 {
        return (status, Json(stub.body.clone())).into_response();
    }
    // The native Runner requests streaming, so the fixture speaks the
    // provider's SSE chat-completion protocol.
    let text = stub
        .body
        .pointer("/choices/0/message/content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let chunk = serde_json::json!({
        "id": "chatcmpl-fixture",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "fixture",
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}, "finish_reason": null}]
    });
    let done = serde_json::json!({
        "id": "chatcmpl-fixture",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "fixture",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
    });
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        format!("data: {chunk}\n\ndata: {done}\n\ndata: [DONE]\n\n"),
    )
        .into_response()
}

async fn chat_stub(status: u16, body: serde_json::Value) -> (String, Arc<AtomicUsize>) {
    let (endpoint, count, _) = chat_stub_recorded(status, body).await;
    (endpoint, count)
}

async fn chat_stub_recorded(
    status: u16,
    body: serde_json::Value,
) -> (
    String,
    Arc<AtomicUsize>,
    Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
) {
    let count = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_handler))
        .with_state(Arc::new(ChatStub {
            status,
            body,
            count: count.clone(),
            requests: requests.clone(),
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture");
    let address = listener.local_addr().expect("fixture address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}/v1"), count, requests)
}

fn chat_completion(text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-fixture",
        "object": "chat.completion",
        "created": 0,
        "model": "fixture",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
    })
}

fn provider(adapter: &str, model: &str, endpoint: Option<String>, local: bool) -> ProviderProfile {
    ProviderProfile {
        adapter: adapter.into(),
        model: model.into(),
        endpoint,
        modalities: vec!["text".into()],
        tool_calling: true,
        local,
        residency: if local { "local" } else { "remote" }.into(),
        context_window: Some(8_192),
        max_output_tokens: None,
        reasoning_levels: Vec::new(),
        credential: None,
        cost_rank: Some(if local { 0 } else { 5 }),
        input_cost_micros_per_million: None,
        candidate_cost_micros_per_million: None,
        cache_read_cost_micros_per_million: None,
        cache_write_cost_micros_per_million: None,
        data_class: Some(if local { "local" } else { "shared" }.into()),
        fallbacks: Vec::new(),
    }
}

fn registry(
    entries: Vec<(&str, ProviderProfile)>,
    allow_remote: bool,
    secrets: Arc<MapSecrets>,
) -> Arc<ProviderRegistry> {
    let profiles: BTreeMap<_, _> = entries
        .into_iter()
        .map(|(id, profile)| (id.to_string(), profile))
        .collect();
    ProviderRegistry::new(profiles, allow_remote, secrets).expect("registry is valid")
}

fn text() -> ProviderRequirements {
    ProviderRequirements::text()
}

#[tokio::test]
async fn configured_local_http_adapter_drives_native_runner() {
    let (endpoint, count) = json_stub(200, serde_json::json!({"text": "local: ok"})).await;
    let registry = registry(
        vec![(
            "local-http",
            provider("http-json", "local-model", Some(endpoint), true),
        )],
        false,
        MapSecrets::empty(),
    );
    let resolved = registry
        .resolve("local-http", 1, &text())
        .expect("provider resolves");
    assert_eq!(resolved.record.provider, "local-http");
    assert_eq!(resolved.record.adapter, "http-json");
    assert_eq!(resolved.record.model, "local-model");
    assert!(resolved.record.local);

    let harness =
        ProofHarness::new("provider-local", resolved.model).expect("native runner builds");
    let turn = harness
        .run_turn("user-local", "session-local", "hello")
        .await
        .expect("native runner executes the local adapter");
    assert_eq!(turn.output, "local: ok");
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn configured_remote_openai_adapter_drives_native_runner() {
    let (endpoint, count) = chat_stub(200, chat_completion("remote: ok")).await;
    let mut remote = provider("openai-compatible", "remote-model", Some(endpoint), false);
    remote.credential = Some("env:REMOTE_KEY".into());
    let registry = registry(
        vec![("remote-openai", remote)],
        true,
        MapSecrets::with(&[("env:REMOTE_KEY", "test-key")]),
    );
    let resolved = registry
        .resolve("remote-openai", 1, &text())
        .expect("remote provider resolves");
    assert!(!resolved.record.local);
    assert_eq!(resolved.record.residency, "remote");

    let harness =
        ProofHarness::new("provider-remote", resolved.model).expect("native runner builds");
    let turn = harness
        .run_turn("user-remote", "session-remote", "hello")
        .await
        .expect("native runner executes the remote adapter");
    assert_eq!(turn.output, "remote: ok");
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn declared_reasoning_level_reaches_the_native_openai_request_and_evidence() {
    let (endpoint, count, requests) =
        chat_stub_recorded(200, chat_completion("reasoning: ok")).await;
    let mut remote = provider(
        "openai-compatible",
        "reasoning-model",
        Some(endpoint),
        false,
    );
    remote.credential = Some("env:REASONING_KEY".into());
    remote.reasoning_levels = vec![
        ReasoningLevel::Low,
        ReasoningLevel::Medium,
        ReasoningLevel::High,
    ];
    remote.max_output_tokens = Some(64);
    let registry = registry(
        vec![("reasoning-openai", remote)],
        true,
        MapSecrets::with(&[("env:REASONING_KEY", "test-key")]),
    );
    let requirements = text().with_reasoning(Some(ReasoningLevel::High));
    let resolved = registry
        .resolve("reasoning-openai", 1, &requirements)
        .expect("declared reasoning level resolves");
    assert_eq!(resolved.record.reasoning, Some(ReasoningLevel::High));
    assert_eq!(resolved.record.max_output_tokens, Some(64));

    let harness =
        ProofHarness::new("provider-reasoning", resolved.model).expect("native runner builds");
    let turn = harness
        .run_turn("user-reasoning", "session-reasoning", "hello")
        .await
        .expect("native runner applies reasoning");
    assert_eq!(turn.output, "reasoning: ok");
    assert_eq!(count.load(Ordering::SeqCst), 1);

    let body = requests
        .lock()
        .expect("request recorder is not poisoned")
        .first()
        .cloned()
        .expect("the native adapter sent a request");
    assert_eq!(body["reasoning_effort"], "high", "{body}");
    assert_eq!(body["max_completion_tokens"], 64, "{body}");
}

#[tokio::test]
async fn reasoning_level_on_an_adapter_without_the_pinned_field_is_a_typed_gap() {
    let mut unsupported = provider(
        "http-json",
        "m",
        Some("http://127.0.0.1:1/generate".into()),
        true,
    );
    unsupported.reasoning_levels = vec![ReasoningLevel::High];
    let profiles = BTreeMap::from([("local-json".to_string(), unsupported)]);
    let error = ProviderRegistry::new(profiles, false, MapSecrets::empty())
        .err()
        .expect("an adapter without a reasoning field cannot claim levels");
    assert_eq!(error.code, "provider.reasoning_unsupported");
    assert_eq!(
        error.details.get("adapter").map(String::as_str),
        Some("http-json")
    );

    let (endpoint, _) = json_stub(200, serde_json::json!({"text": "unused"})).await;
    let mut closed = provider("http-json", "m", Some(endpoint), true);
    closed.reasoning_levels = Vec::new();
    let registry = registry(vec![("declared-none", closed)], false, MapSecrets::empty());
    let error = registry
        .preflight(
            "declared-none",
            &text().with_reasoning(Some(ReasoningLevel::High)),
        )
        .expect_err("a requested level outside the catalog must fail closed");
    assert_eq!(error.code, "provider.reasoning_unsupported");
}

#[test]
fn fallback_that_cannot_apply_the_reasoning_level_is_rejected() {
    let mut primary = provider(
        "openai-compatible",
        "primary",
        Some("http://127.0.0.1:1/v1".into()),
        false,
    );
    primary.credential = Some("env:REASONING_KEY".into());
    primary.reasoning_levels = vec![ReasoningLevel::High];
    primary.fallbacks = vec!["backup".into()];
    let backup = provider(
        "http-json",
        "backup",
        Some("http://127.0.0.1:1/generate".into()),
        false,
    );
    let registry = registry(
        vec![("primary", primary), ("backup", backup)],
        true,
        MapSecrets::with(&[("env:REASONING_KEY", "test-key")]),
    );
    let error = registry
        .preflight(
            "primary",
            &text().with_reasoning(Some(ReasoningLevel::High)),
        )
        .expect_err("fallback must not silently drop reasoning");
    assert_eq!(error.code, "provider.reasoning_unsupported");
    assert_eq!(
        error.details.get("provider").map(String::as_str),
        Some("backup")
    );
}

#[tokio::test]
async fn per_agent_model_and_reasoning_are_validated_before_child_admission() {
    let dir = tempfile::tempdir().expect("runtime directory");
    let mut config = RuntimeConfig::local(dir.path());
    config.providers.clear();
    let mut strong = provider(
        "openai-compatible",
        "strong-model",
        Some("http://127.0.0.1:1/v1".into()),
        false,
    );
    strong.credential = Some("env:GOLDENBEE_TEST_CHILD_REASONING_KEY".into());
    strong.reasoning_levels = vec![ReasoningLevel::High];
    config.providers.insert("strong".into(), strong);
    let mut small = provider(
        "http-json",
        "small-model",
        Some("http://127.0.0.1:1/generate".into()),
        true,
    );
    small.context_window = Some(1_000);
    config.providers.insert("small".into(), small);
    config.default_model = "strong".into();
    config
        .agent_providers
        .insert("reasoning-child".into(), "strong".into());
    config
        .agent_reasoning
        .insert("reasoning-child".into(), ReasoningLevel::High);
    config
        .agent_providers
        .insert("small-child".into(), "small".into());
    let capabilities = config.capabilities.clone();
    config.capabilities = capabilities
        .into_iter()
        .chain([
            common::CapabilityDescriptor {
                id: "reasoning-child".into(),
                kind: "agent".into(),
                description: "reasoning child".into(),
                permissions: Vec::new(),
            },
            common::CapabilityDescriptor {
                id: "small-child".into(),
                kind: "agent".into(),
                description: "small-window child".into(),
                permissions: Vec::new(),
            },
        ])
        .collect();
    std::env::set_var("GOLDENBEE_TEST_CHILD_REASONING_KEY", "test-key");
    let runtime = Runtime::open(config.remote_approved())
        .await
        .expect("runtime opens");
    let principal = Principal::new("tester", "local", &["*"]);

    let admitted = runtime
        .submit_task(
            &principal,
            SubmitTaskRequest {
                namespace: Some("local".into()),
                agent: "reasoning-child".into(),
                session_id: "session-reasoning-child".into(),
                input: "hello".into(),
                idempotency_key: "child-reasoning".into(),
                profile: Some("default".into()),
                team: None,
                worker: None,
                limits: Default::default(),
            },
        )
        .await
        .expect("the declared child model admits");
    let task = runtime
        .task_unscoped(&admitted.task_id)
        .await
        .expect("task recorded");
    let resolved = task.resolved_model.expect("model recorded");
    assert_eq!(resolved.provider, "strong");
    assert_eq!(resolved.reasoning, Some(ReasoningLevel::High));

    let error = runtime
        .submit_task(
            &principal,
            SubmitTaskRequest {
                namespace: Some("local".into()),
                agent: "small-child".into(),
                session_id: "session-small-child".into(),
                input: "hello".into(),
                idempotency_key: "child-context".into(),
                profile: Some("default".into()),
                team: None,
                worker: None,
                limits: common::Limits {
                    max_tokens: 2_000,
                    ..common::Limits::default()
                },
            },
        )
        .await
        .expect_err("a child whose window cannot hold the budget must not admit");
    assert_eq!(error.code, "provider.context_unsupported");
    std::env::remove_var("GOLDENBEE_TEST_CHILD_REASONING_KEY");
}

#[tokio::test]
async fn openrouter_reasoning_is_applied_as_a_request_option() {
    let (endpoint, count, requests) =
        chat_stub_recorded(200, chat_completion("openrouter: ok")).await;
    let mut remote = provider("openrouter", "openrouter-model", Some(endpoint), false);
    remote.credential = Some("env:OPENROUTER_REASONING_KEY".into());
    remote.reasoning_levels = vec![ReasoningLevel::High];
    let registry = registry(
        vec![("openrouter-reasoning", remote)],
        true,
        MapSecrets::with(&[("env:OPENROUTER_REASONING_KEY", "test-key")]),
    );
    let resolved = registry
        .resolve(
            "openrouter-reasoning",
            1,
            &text().with_reasoning(Some(ReasoningLevel::High)),
        )
        .expect("openrouter reasoning resolves");
    assert_eq!(resolved.record.reasoning, Some(ReasoningLevel::High));

    let harness =
        ProofHarness::new("provider-openrouter", resolved.model).expect("native runner builds");
    let turn = harness
        .run_turn("user-openrouter", "session-openrouter", "hello")
        .await
        .expect("openrouter request runs");
    assert_eq!(turn.output, "openrouter: ok");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let body = requests
        .lock()
        .expect("request recorder is not poisoned")
        .first()
        .cloned()
        .expect("the native adapter sent a request");
    assert_eq!(body["reasoning"]["effort"], "high", "{body}");
}

#[tokio::test]
async fn discovered_model_alias_drives_the_native_adapter() {
    let (endpoint, count) = chat_stub(200, chat_completion("alias: ok")).await;
    let mut base = provider("openai-compatible", "model-a", Some(endpoint), false);
    base.credential = Some("gateway:openrouter".into());
    let mut alias = base.clone();
    alias.model = "model-b".into();
    let profiles = BTreeMap::from([
        ("openrouter".to_string(), base),
        ("openrouter/model-b".to_string(), alias),
    ]);
    let registry = ProviderRegistry::new(
        profiles,
        true,
        Arc::new(runtime::MemorySecrets::new(BTreeMap::from([(
            "gateway:openrouter".to_string(),
            "test-key".to_string(),
        )]))),
    )
    .expect("alias registry");
    let selected = registry
        .resolve_primary("openrouter/model-b", 1, &text())
        .expect("alias resolves");
    assert_eq!(selected.record.model, "model-b");
    assert_eq!(
        selected.record.credential.as_deref(),
        Some("gateway:openrouter")
    );
    let harness =
        ProofHarness::new("provider-alias", selected.model).expect("native runner builds");
    let turn = harness
        .run_turn("user-alias", "session-alias", "hello")
        .await
        .expect("native runner executes the discovered alias");
    assert_eq!(turn.output, "alias: ok");
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn task_records_concrete_provider_and_effective_revision() {
    let (endpoint, _count) = json_stub(200, serde_json::json!({"text": "recorded"})).await;
    let dir = tempfile::tempdir().expect("runtime directory");
    let mut config = RuntimeConfig::local(dir.path());
    config.providers.clear();
    config.providers.insert(
        "chosen".into(),
        provider("http-json", "chosen-model", Some(endpoint), true),
    );
    config
        .agent_providers
        .insert("general".into(), "chosen".into());
    config.agent_requirements.insert(
        "general".into(),
        AgentRequirement {
            tools: 0,
            toolsets: 0,
        },
    );
    let runtime = Runtime::open(config).await.expect("runtime opens");
    let principal = Principal::new("tester", "local", &["*"]);
    let revision = ProfileRevision {
        revision: 0,
        instruction: "Answer.".into(),
        model: "chosen".into(),
        capabilities: Vec::new(),
        grants: vec!["task:submit".into()],
        restrictions: Vec::new(),
        composition: "single".into(),
        members: vec!["general".into()],
        skills: Vec::new(),
        mcp: Vec::new(),
        workflow: None,
        limits: common::Limits::default(),
        compaction: None,
        secret_refs: Vec::new(),
        reasoning: None,
        created_at: 0,
    };
    let profile = runtime
        .create_profile(&principal, "local", "chosen-profile", revision)
        .await
        .expect("profile is created");
    let revision_number = profile.revisions.last().expect("revision").revision;
    runtime
        .activate_profile(&principal, "local", "chosen-profile", revision_number)
        .await
        .expect("profile activates");

    let response = runtime
        .submit_task(
            &principal,
            SubmitTaskRequest {
                namespace: Some("local".into()),
                agent: "general".into(),
                session_id: "session-record".into(),
                input: "hello".into(),
                idempotency_key: "record-provider".into(),
                profile: Some("chosen-profile".into()),
                team: None,
                worker: None,
                limits: Default::default(),
            },
        )
        .await
        .expect("task admits");
    for _ in 0..200 {
        let task = runtime
            .task_unscoped(&response.task_id)
            .await
            .expect("task exists");
        if task.state.is_terminal() {
            let resolved: ResolvedModel = task.resolved_model.expect("provider is recorded");
            assert_eq!(resolved.provider, "chosen");
            assert_eq!(resolved.adapter, "http-json");
            assert_eq!(resolved.model, "chosen-model");
            assert_eq!(resolved.profile_revision, revision_number);
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("task did not reach a terminal state");
}

#[tokio::test]
async fn admission_preflight_rejects_incompatible_provider_before_recording_a_task() {
    let dir = tempfile::tempdir().expect("runtime directory");
    let mut config = RuntimeConfig::local(dir.path());
    let mut incompatible = provider(
        "http-json",
        "m",
        Some("http://127.0.0.1:1/generate".into()),
        true,
    );
    incompatible.tool_calling = false;
    config.providers.insert("incompatible".into(), incompatible);
    config.agent_requirements.insert(
        "general".into(),
        AgentRequirement {
            tools: 3,
            toolsets: 0,
        },
    );
    let runtime = Runtime::open(config).await.expect("runtime opens");
    let principal = Principal::new("tester", "local", &["*"]);
    let revision = ProfileRevision {
        revision: 0,
        instruction: "Answer.".into(),
        model: "incompatible".into(),
        capabilities: Vec::new(),
        grants: vec!["task:submit".into()],
        restrictions: Vec::new(),
        composition: "single".into(),
        members: vec!["general".into()],
        skills: Vec::new(),
        mcp: Vec::new(),
        workflow: None,
        limits: common::Limits::default(),
        compaction: None,
        secret_refs: Vec::new(),
        reasoning: None,
        created_at: 0,
    };
    let profile = runtime
        .create_profile(&principal, "local", "tool-profile", revision)
        .await
        .expect("profile creation only validates generic provider policy");
    let number = profile.revisions.last().expect("revision").revision;
    runtime
        .activate_profile(&principal, "local", "tool-profile", number)
        .await
        .expect("profile activates");

    let error = runtime
        .submit_task(
            &principal,
            SubmitTaskRequest {
                namespace: Some("local".into()),
                agent: "general".into(),
                session_id: "session-tools".into(),
                input: "hello".into(),
                idempotency_key: "tool-schema".into(),
                profile: Some("tool-profile".into()),
                team: None,
                worker: None,
                limits: Default::default(),
            },
        )
        .await
        .expect_err("admission must reject an unsupported tool schema");
    assert_eq!(error.code, "provider.tool_schema_unsupported");

    let tasks = runtime
        .tasks(&principal, None)
        .await
        .expect("task list is readable");
    assert!(
        tasks.is_empty(),
        "an incompatible provider must not record a task: {tasks:?}"
    );
}

fn local_http(endpoint: String) -> Arc<ProviderRegistry> {
    registry(
        vec![(
            "local-http",
            provider("http-json", "m", Some(endpoint), true),
        )],
        false,
        MapSecrets::empty(),
    )
}

#[tokio::test]
async fn preflight_rejects_unsupported_modality() {
    let registry = local_http("http://127.0.0.1:1/generate".into());
    let error = registry
        .preflight(
            "local-http",
            &ProviderRequirements {
                modalities: vec!["image".into()],
                tools: 0,
                max_tokens: 0,
                reasoning: None,
            },
        )
        .expect_err("image modality must fail");
    assert_eq!(error.code, "provider.modality_unsupported");
}

#[tokio::test]
async fn preflight_rejects_tool_schema_when_tool_calling_is_disabled() {
    let mut profile = provider(
        "http-json",
        "m",
        Some("http://127.0.0.1:1/generate".into()),
        true,
    );
    profile.tool_calling = false;
    let registry = registry(vec![("no-tools", profile)], false, MapSecrets::empty());
    let error = registry
        .preflight(
            "no-tools",
            &ProviderRequirements {
                modalities: vec!["text".into()],
                tools: 2,
                max_tokens: 0,
                reasoning: None,
            },
        )
        .expect_err("bound tools must fail");
    assert_eq!(error.code, "provider.tool_schema_unsupported");
}

#[tokio::test]
async fn preflight_rejects_context_overflow_and_unknown_windows() {
    let mut profile = provider(
        "http-json",
        "m",
        Some("http://127.0.0.1:1/generate".into()),
        true,
    );
    profile.context_window = Some(1_000);
    let providers = registry(vec![("small", profile)], false, MapSecrets::empty());
    let error = providers
        .preflight(
            "small",
            &ProviderRequirements {
                modalities: vec!["text".into()],
                tools: 0,
                max_tokens: 2_000,
                reasoning: None,
            },
        )
        .expect_err("context overflow must fail");
    assert_eq!(error.code, "provider.context_unsupported");

    let mut unknown = provider(
        "http-json",
        "m",
        Some("http://127.0.0.1:1/generate".into()),
        true,
    );
    unknown.context_window = None;
    let providers = registry(vec![("unknown", unknown)], false, MapSecrets::empty());
    let error = providers
        .preflight(
            "unknown",
            &ProviderRequirements {
                modalities: vec!["text".into()],
                tools: 0,
                max_tokens: 2_000,
                reasoning: None,
            },
        )
        .expect_err("unknown context must fail closed");
    assert_eq!(error.code, "provider.context_unknown");
}

#[tokio::test]
async fn preflight_requires_configured_credential_and_approved_remote_locality() {
    let remote = provider(
        "openai-compatible",
        "m",
        Some("http://127.0.0.1:1/v1".into()),
        false,
    );
    let providers = registry(vec![("remote", remote)], false, MapSecrets::empty());
    let error = providers
        .preflight("remote", &text())
        .expect_err("remote provider needs approval and a credential");
    assert!(
        error.code == "provider.credential_missing" || error.code == "provider.remote_not_approved",
        "unexpected code: {}",
        error.code
    );

    let mut local = provider(
        "http-json",
        "m",
        Some("http://127.0.0.1:1/generate".into()),
        true,
    );
    local.credential = Some("env:MISSING".into());
    let providers = registry(vec![("local", local)], false, MapSecrets::empty());
    let error = providers
        .preflight("local", &text())
        .expect_err("missing credential must fail");
    assert_eq!(error.code, "provider.credential_unavailable");
}

#[tokio::test]
async fn preflight_rejects_local_label_on_non_loopback_endpoint() {
    let registry = registry(
        vec![(
            "mislabeled",
            provider(
                "http-json",
                "m",
                Some("http://192.0.2.10/generate".into()),
                true,
            ),
        )],
        true,
        MapSecrets::empty(),
    );
    let error = registry
        .preflight("mislabeled", &text())
        .expect_err("non-loopback endpoint cannot be declared local");
    assert_eq!(error.code, "provider.locality_mismatch");
}

#[tokio::test]
async fn allowed_fallback_is_used_only_after_a_transient_primary_failure() {
    let (primary_endpoint, primary_count) = json_stub(429, serde_json::json!({})).await;
    let (fallback_endpoint, fallback_count) =
        json_stub(200, serde_json::json!({"text": "fallback: ok"})).await;
    let mut primary = provider("http-json", "primary", Some(primary_endpoint), true);
    primary.fallbacks = vec!["backup".into()];
    let backup = provider("http-json", "backup", Some(fallback_endpoint), true);
    let registry = registry(
        vec![("primary", primary), ("backup", backup)],
        false,
        MapSecrets::empty(),
    );
    let resolved = registry
        .resolve("primary", 1, &text())
        .expect("policy-compatible fallback resolves");
    let harness = ProofHarness::new("fallback", resolved.model).expect("runner builds");
    let turn = harness
        .run_turn("user-fallback", "session-fallback", "hello")
        .await
        .expect("fallback turn succeeds");
    assert_eq!(turn.output, "fallback: ok");
    assert_eq!(primary_count.load(Ordering::SeqCst), 1);
    assert_eq!(fallback_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn forbidden_fallback_is_never_called() {
    let (primary_endpoint, primary_count) = json_stub(503, serde_json::json!({})).await;
    let (fallback_endpoint, fallback_count) =
        json_stub(200, serde_json::json!({"text": "must not run"})).await;
    let primary = provider("http-json", "primary", Some(primary_endpoint), true);
    let backup = provider("http-json", "backup", Some(fallback_endpoint), true);
    let registry = registry(
        vec![("primary", primary), ("backup", backup)],
        false,
        MapSecrets::empty(),
    );
    let resolved = registry
        .resolve("primary", 1, &text())
        .expect("primary resolves without fallback policy");
    let harness = ProofHarness::new("forbidden", resolved.model).expect("runner builds");
    let error = harness
        .run_turn("user-forbidden", "session-forbidden", "hello")
        .await
        .expect_err("primary outage must surface");
    assert!(format!("{error}").contains("unavailable"), "{error}");
    assert_eq!(primary_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        fallback_count.load(Ordering::SeqCst),
        0,
        "an unlisted provider must never be called"
    );
}

#[tokio::test]
async fn fallback_cannot_widen_locality_residency_or_authority() {
    let (primary_endpoint, _) = json_stub(429, serde_json::json!({})).await;
    let (remote_endpoint, _) = chat_stub(200, chat_completion("remote")).await;
    let mut primary = provider("http-json", "primary", Some(primary_endpoint), true);
    primary.fallbacks = vec!["remote".into()];
    let mut remote = provider("openai-compatible", "remote", Some(remote_endpoint), false);
    remote.credential = Some("env:REMOTE_KEY".into());
    let registry = registry(
        vec![("primary", primary), ("remote", remote)],
        true,
        MapSecrets::with(&[("env:REMOTE_KEY", "test-key")]),
    );
    let error = match registry.resolve("primary", 1, &text()) {
        Ok(_) => panic!("residency-widening fallback must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, "provider.fallback_locality");
}

#[tokio::test]
async fn fallback_cannot_widen_cost_or_credential_authority() {
    let (primary_endpoint, _) = json_stub(429, serde_json::json!({})).await;
    let (fallback_endpoint, _) = json_stub(200, serde_json::json!({"text": "backup"})).await;

    let mut primary = provider("http-json", "primary", Some(primary_endpoint.clone()), true);
    primary.fallbacks = vec!["costly".into()];
    primary.cost_rank = Some(1);
    let mut costly = provider("http-json", "costly", Some(fallback_endpoint.clone()), true);
    costly.cost_rank = Some(2);
    let providers = registry(
        vec![("primary", primary), ("costly", costly)],
        false,
        MapSecrets::empty(),
    );
    let error = match providers.resolve("primary", 1, &text()) {
        Ok(_) => panic!("cost-widening fallback must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, "provider.fallback_cost");

    let mut primary = provider("http-json", "primary", Some(primary_endpoint), true);
    primary.fallbacks = vec!["authority".into()];
    let mut authority = provider("openai-compatible", "authority", None, true);
    authority.endpoint = Some(format!("{fallback_endpoint}/v1"));
    authority.credential = Some("env:EXTRA_KEY".into());
    let providers = registry(
        vec![("primary", primary), ("authority", authority)],
        false,
        MapSecrets::with(&[("env:EXTRA_KEY", "extra-key")]),
    );
    let error = match providers.resolve("primary", 1, &text()) {
        Ok(_) => panic!("authority-widening fallback must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, "provider.fallback_authority");
}

#[tokio::test]
async fn credential_revocation_blocks_new_calls_without_rebuilding() {
    let (endpoint, count) = chat_stub(200, chat_completion("remote: ok")).await;
    let mut remote = provider("openai-compatible", "remote-model", Some(endpoint), false);
    remote.credential = Some("env:REMOTE_KEY".into());
    let registry = registry(
        vec![("remote", remote)],
        true,
        MapSecrets::with(&[("env:REMOTE_KEY", "test-key")]),
    );
    let resolved = registry
        .resolve("remote", 1, &text())
        .expect("provider resolves");
    let model = resolved.model;

    let first = ProofHarness::new("revoke", model.clone()).expect("runner builds");
    first
        .run_turn("user-revoke", "session-before", "hello")
        .await
        .expect("first call succeeds");
    assert_eq!(count.load(Ordering::SeqCst), 1);

    registry.revoke("env:REMOTE_KEY");
    let second = ProofHarness::new("revoke", model).expect("same model is reused");
    let error = second
        .run_turn("user-revoke", "session-after", "hello")
        .await
        .expect_err("revoked credential blocks a new call");
    assert!(format!("{error}").contains("revoked"), "{error}");
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "no provider request may reach the endpoint after revocation"
    );
}

#[tokio::test]
async fn secret_values_never_appear_in_inspection_or_errors() {
    let canary = "canary-value-4a1f";
    let (endpoint, _) = chat_stub(
        401,
        serde_json::json!({
            "error": {"message": format!("invalid api key {canary}"), "type": "invalid_request_error"}
        }),
    )
    .await;
    let mut remote = provider("openai-compatible", "remote-model", Some(endpoint), false);
    remote.credential = Some("env:CANARY_KEY".into());
    let registry = registry(
        vec![("remote", remote)],
        true,
        MapSecrets::with(&[("env:CANARY_KEY", canary)]),
    );

    let views = serde_json::to_string(&registry.views()).expect("views serialize");
    assert!(
        !views.contains(canary),
        "inspection leaked a secret: {views}"
    );
    assert!(views.contains("env:CANARY_KEY"), "reference stays visible");

    let resolved = registry
        .resolve("remote", 1, &text())
        .expect("provider resolves");
    let harness = ProofHarness::new("secret", resolved.model).expect("runner builds");
    let error = harness
        .run_turn("user-secret", "session-secret", "hello")
        .await
        .expect_err("credential failure surfaces");
    let text = format!("{error}");
    assert!(!text.contains(canary), "error leaked a secret: {text}");
}

#[tokio::test]
#[ignore = "requires a live local Ollama server and an installed model"]
async fn live_local_ollama_inference_produces_a_native_response() {
    use adk_rust::{Content, LlmRequest};

    let model_id = std::env::var("GOLDENBEE_OLLAMA_MODEL").unwrap_or_else(|_| "tev1:0.8b".into());
    let mut profile = provider(
        "ollama",
        &model_id,
        Some("http://127.0.0.1:11434".into()),
        true,
    );
    profile.context_window = Some(2_048);
    let registry = registry(vec![("live-local", profile)], false, MapSecrets::empty());
    let resolved = registry
        .resolve("live-local", 1, &text())
        .expect("live provider resolves");
    let response = resolved
        .model
        .generate_content(
            LlmRequest::new(
                model_id.clone(),
                vec![Content::new("user")
                    .with_text("Reply with one short sentence saying that local inference works.")],
            ),
            false,
        )
        .await
        .expect("live local request succeeds");
    use futures::StreamExt;
    let mut stream = response;
    let mut text = String::new();
    let mut thinking = String::new();
    let mut usage = None;
    while let Some(item) = stream.next().await {
        let event = item.expect("live response event");
        if let Some(metadata) = event.usage_metadata {
            usage = Some(metadata);
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
    assert!(
        !text.trim().is_empty() || !thinking.trim().is_empty(),
        "live local model returned no content"
    );
    assert!(usage.is_some(), "live local model reported usage");
}
