//! Provider selection, preflight, fallback, revocation, and secret-safety.
//!
//! These tests use real native adapters. Endpoint-backed legs point at
//! deterministic loopback fixtures, so no hosted credential or network is
//! required.

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use common::{Principal, ProfileRevision, ResolvedModel, SubmitTaskRequest};
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
}

async fn chat_handler(
    State(stub): State<Arc<ChatStub>>,
    _body: axum::body::Bytes,
) -> axum::response::Response {
    stub.count.fetch_add(1, Ordering::SeqCst);
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
    let count = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_handler))
        .with_state(Arc::new(ChatStub {
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
    (format!("http://{address}/v1"), count)
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
        credential: None,
        cost_rank: Some(if local { 0 } else { 5 }),
        input_cost_micros_per_million: None,
        candidate_cost_micros_per_million: None,
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
