//! Provider vault HTTP behavior: write-only credentials, scope checks, Worker
//! enrollment, assignment, and envelope decryption.

use base64::Engine;
use common::{ProviderBundle, WorkerEnrollRequest};
use gateway_core::{CoreConfig, VaultConfig, WorkerConfig};
use goldenbee_gateway::{CallerConfig, Config, Gateway, SocketConfig};
use provider_crypto::{envelope_aad, generate_private_key, open_envelope, public_key, MasterKey};
use serde_json::{json, Value};

const ADMIN_TOKEN: &str = "provider-admin-token";
const READONLY_TOKEN: &str = "provider-readonly-token";
const CONTROL_TOKEN: &str = "provider-worker-control";

fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn start_gateway(directory: &std::path::Path, env_name: &str) -> String {
    let config = Config {
        listen: "127.0.0.1:0".into(),
        callers: vec![
            CallerConfig {
                token: ADMIN_TOKEN.into(),
                id: "operator".into(),
                namespace: "local".into(),
                scopes: vec!["admin".into(), "provider:admin".into()],
                workers: None,
            },
            CallerConfig {
                token: READONLY_TOKEN.into(),
                id: "readonly".into(),
                namespace: "local".into(),
                scopes: vec!["task:read".into()],
                workers: None,
            },
        ],
        socket: SocketConfig::default(),
        core: CoreConfig {
            data_dir: directory.join("gateway").display().to_string(),
            registry: None,
            vault: Some(VaultConfig {
                enabled: true,
                directory: directory.join("vault").display().to_string(),
                master_key_env: env_name.into(),
            }),
            workers: vec![WorkerConfig {
                id: "general-a".into(),
                endpoint: "http://127.0.0.1:1".into(),
                instance: Some("general-a".into()),
                package: Some("general".into()),
                capabilities: None,
                control_token: Some(CONTROL_TOKEN.into()),
                preflight: Some(false),
            }],
            ..Default::default()
        },
    };
    let gateway = Gateway::prepare(config).await.expect("gateway prepares");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, gateway.router()).await;
    });
    format!("http://{addr}")
}

async fn create_provider(client: &reqwest::Client, base: &str) {
    let response = client
        .post(format!("{base}/v1/admin/model-providers"))
        .bearer_auth(ADMIN_TOKEN)
        .json(&json!({
            "id": "openrouter",
            "adapter": "http-json",
            "model": "test-model",
            "endpoint": "http://127.0.0.1:1/generate",
            "modalities": ["text"],
            "tool_calling": true,
            "context_window": 128000,
            "local": false,
            "residency": "remote",
            "allowed_workers": ["general-a"],
        }))
        .send()
        .await
        .expect("create response");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn provider_vault_http_flow_is_write_only_and_worker_scoped() {
    std::env::set_var("GOLDENBEE_DELEGATION_SECRET", "provider-vault-test-secret");
    let env_name = format!(
        "GOLDENBEE_TEST_VAULT_KEY_{}_{}",
        std::process::id(),
        common::now_ms()
    );
    std::env::set_var(&env_name, MasterKey::generate().encode());
    let directory = tempfile::tempdir().expect("directory");
    let base = start_gateway(directory.path(), &env_name).await;
    let client = reqwest::Client::new();

    // Read-only callers cannot inspect the provider catalog.
    let denied = client
        .get(format!("{base}/v1/admin/model-providers"))
        .bearer_auth(READONLY_TOKEN)
        .send()
        .await
        .expect("denied response");
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);

    create_provider(&client, &base).await;

    // Write-only credential: the response exposes only version and fingerprint.
    let response = client
        .put(format!(
            "{base}/v1/admin/model-providers/openrouter/credential"
        ))
        .bearer_auth(ADMIN_TOKEN)
        .json(&json!({"key": "sk-http-secret"}))
        .send()
        .await
        .expect("credential response");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let text = response.text().await.expect("body");
    assert!(!text.contains("sk-http-secret"));
    let summary: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(summary["version"], 1);
    assert!(summary["fingerprint"]
        .as_str()
        .expect("fingerprint")
        .starts_with("sha256:"));

    let catalog = client
        .get(format!("{base}/v1/admin/model-providers"))
        .bearer_auth(ADMIN_TOKEN)
        .send()
        .await
        .expect("catalog")
        .text()
        .await
        .expect("catalog body");
    assert!(!catalog.contains("sk-http-secret"));
    assert!(!catalog.contains("ciphertext"));
    assert!(catalog.contains("sha256:"));

    // A pair code is bound to the approved Worker instance.
    let wrong = client
        .post(format!("{base}/v1/admin/model-providers/pair-code"))
        .bearer_auth(ADMIN_TOKEN)
        .json(&json!({"worker_id": "general-a", "instance": "other"}))
        .send()
        .await
        .expect("wrong pair");
    assert_eq!(wrong.status(), reqwest::StatusCode::BAD_REQUEST);

    let pair: Value = client
        .post(format!("{base}/v1/admin/model-providers/pair-code"))
        .bearer_auth(ADMIN_TOKEN)
        .json(&json!({"worker_id": "general-a", "instance": "general-a"}))
        .send()
        .await
        .expect("pair")
        .json()
        .await
        .expect("pair body");
    let code = pair["code"].as_str().expect("code").to_string();
    assert!(!pair.to_string().contains("sk-http-secret"));

    let private = generate_private_key();
    let enroll = WorkerEnrollRequest {
        code: code.clone(),
        worker_id: "general-a".into(),
        instance: "general-a".into(),
        public_key: encode(&public_key(&private)),
    };
    let unauthorized = client
        .post(format!("{base}/v1/model-providers/enroll"))
        .bearer_auth("wrong-control-token")
        .json(&enroll)
        .send()
        .await
        .expect("unauthorized enroll");
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

    let enrolled = client
        .post(format!("{base}/v1/model-providers/enroll"))
        .bearer_auth(CONTROL_TOKEN)
        .json(&enroll)
        .send()
        .await
        .expect("enroll");
    assert_eq!(enrolled.status(), reqwest::StatusCode::OK);

    let replay = client
        .post(format!("{base}/v1/model-providers/enroll"))
        .bearer_auth(CONTROL_TOKEN)
        .json(&enroll)
        .send()
        .await
        .expect("replay");
    assert_eq!(replay.status(), reqwest::StatusCode::BAD_REQUEST);

    let bundle_text = client
        .get(format!("{base}/v1/model-providers"))
        .bearer_auth(CONTROL_TOKEN)
        .send()
        .await
        .expect("bundle")
        .text()
        .await
        .expect("bundle body");
    assert!(!bundle_text.contains("sk-http-secret"));
    let bundle: ProviderBundle = serde_json::from_str(&bundle_text).expect("bundle json");
    assert_eq!(bundle.worker, "general-a");
    assert_eq!(bundle.providers.len(), 1);
    let assignment = &bundle.providers[0];
    assert_eq!(assignment.metadata.id, "openrouter");
    let envelope = &assignment.envelope;
    let aad = envelope_aad(
        "openrouter",
        envelope.version,
        "general-a",
        "general-a",
        envelope.expires_at,
    );
    let decrypted = open_envelope(
        &private,
        &base64::engine::general_purpose::STANDARD
            .decode(&envelope.ephemeral_public_key)
            .expect("ephemeral")
            .try_into()
            .expect("32 bytes"),
        &base64::engine::general_purpose::STANDARD
            .decode(&envelope.nonce)
            .expect("nonce"),
        &base64::engine::general_purpose::STANDARD
            .decode(&envelope.ciphertext)
            .expect("ciphertext"),
        &aad,
    )
    .expect("worker decrypts");
    assert_eq!(decrypted.as_slice(), b"sk-http-secret");

    let wrong_private = generate_private_key();
    assert!(open_envelope(
        &wrong_private,
        &base64::engine::general_purpose::STANDARD
            .decode(&envelope.ephemeral_public_key)
            .expect("ephemeral")
            .try_into()
            .expect("32 bytes"),
        &base64::engine::general_purpose::STANDARD
            .decode(&envelope.nonce)
            .expect("nonce"),
        &base64::engine::general_purpose::STANDARD
            .decode(&envelope.ciphertext)
            .expect("ciphertext"),
        &aad,
    )
    .is_err());

    // Revocation removes the provider from new envelopes.
    let revoked = client
        .delete(format!(
            "{base}/v1/admin/model-providers/openrouter/credential"
        ))
        .bearer_auth(ADMIN_TOKEN)
        .send()
        .await
        .expect("revoke");
    assert_eq!(revoked.status(), reqwest::StatusCode::OK);
    let bundle: ProviderBundle = client
        .get(format!("{base}/v1/model-providers"))
        .bearer_auth(CONTROL_TOKEN)
        .send()
        .await
        .expect("bundle after revoke")
        .json()
        .await
        .expect("bundle json");
    assert!(bundle.providers.is_empty());
}

#[derive(Clone)]
struct FakeModels {
    models: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    seen_auth: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

async fn fake_models(
    axum::extract::State(state): axum::extract::State<FakeModels>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    state
        .seen_auth
        .lock()
        .expect("seen auth")
        .push(auth.clone());
    if auth != "Bearer sk-good" {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    let models = state.models.lock().expect("models").clone();
    axum::Json(serde_json::json!({
        "data": models.into_iter().map(|id| serde_json::json!({"id": id})).collect::<Vec<_>>()
    }))
    .into_response()
}

async fn fake_tags() -> axum::Json<Value> {
    axum::Json(serde_json::json!({
        "models": [{"name": "llama3:latest"}, {"name": "qwen2.5:7b"}]
    }))
}

async fn fake_auth_failure() -> axum::http::StatusCode {
    axum::http::StatusCode::UNAUTHORIZED
}

async fn fake_malformed() -> axum::Json<Value> {
    axum::Json(serde_json::json!({"unexpected": true}))
}

async fn fake_empty() -> axum::Json<Value> {
    axum::Json(serde_json::json!({"data": []}))
}

async fn start_fake_provider() -> (String, FakeModels) {
    let state = FakeModels {
        models: std::sync::Arc::new(std::sync::Mutex::new(vec![
            "alpha".into(),
            "beta".into(),
            "gamma".into(),
            "alpha".into(),
        ])),
        seen_auth: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    let app = axum::Router::new()
        .route("/v1/models", axum::routing::get(fake_models))
        .route("/api/tags", axum::routing::get(fake_tags))
        .route("/auth/v1/models", axum::routing::get(fake_auth_failure))
        .route("/bad/v1/models", axum::routing::get(fake_malformed))
        .route("/empty/v1/models", axum::routing::get(fake_empty))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake listener");
    let addr = listener.local_addr().expect("fake addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), state)
}

async fn create_discovery_provider(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    endpoint: &str,
    model: &str,
    allowed: Option<Vec<&str>>,
) -> Value {
    let response = client
        .post(format!("{base}/v1/admin/model-providers"))
        .bearer_auth(ADMIN_TOKEN)
        .json(&json!({
            "id": id,
            "adapter": "openai-compatible",
            "model": model,
            "endpoint": endpoint,
            "modalities": ["text"],
            "tool_calling": true,
            "context_window": 128000,
            "local": false,
            "residency": "remote",
            "allowed_workers": ["general-a"],
            "allowed_models": allowed,
        }))
        .send()
        .await
        .expect("create discovery provider");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json().await.expect("create body")
}

async fn set_discovery_credential(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    key: &str,
) -> (String, Value) {
    let response = client
        .put(format!("{base}/v1/admin/model-providers/{id}/credential"))
        .bearer_auth(ADMIN_TOKEN)
        .json(&json!({"key": key}))
        .send()
        .await
        .expect("credential response");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let text = response.text().await.expect("credential body");
    assert!(!text.contains(key));
    (
        text.clone(),
        serde_json::from_str(&text).expect("credential json"),
    )
}

#[tokio::test]
async fn discovery_discovers_filters_refreshes_and_never_leaks_the_key() {
    std::env::set_var(
        "GOLDENBEE_DELEGATION_SECRET",
        "provider-discovery-test-secret",
    );
    let env_name = format!(
        "GOLDENBEE_TEST_DISCOVERY_KEY_{}_{}",
        std::process::id(),
        common::now_ms()
    );
    std::env::set_var(&env_name, MasterKey::generate().encode());
    let directory = tempfile::tempdir().expect("directory");
    let base = start_gateway(directory.path(), &env_name).await;
    let (fake, fake_state) = start_fake_provider().await;
    let client = reqwest::Client::new();

    let created = create_discovery_provider(
        &client,
        &base,
        "discovered",
        &format!("{fake}/v1"),
        "beta",
        Some(vec!["alpha", "beta"]),
    )
    .await;
    assert_eq!(created["ready"], false, "credential is not set yet");

    let (_, summary) = set_discovery_credential(&client, &base, "discovered", "sk-good").await;
    assert_eq!(summary["ready"], true);
    assert_eq!(summary["models"], json!(["alpha", "beta"]));
    assert!(summary["discovered_at"].is_i64());

    let shown: Value = client
        .get(format!("{base}/v1/admin/model-providers/discovered"))
        .bearer_auth(ADMIN_TOKEN)
        .send()
        .await
        .expect("show")
        .json()
        .await
        .expect("show json");
    assert_eq!(shown["models_source"], "discovered");
    assert_eq!(shown["model"], "beta");

    // A refresh is revisioned and converges on the current fake provider.
    *fake_state.models.lock().expect("models") =
        vec!["beta".into(), "gamma".into(), "alpha".into()];
    let refreshed: Value = client
        .post(format!(
            "{base}/v1/admin/model-providers/discovered/discover"
        ))
        .bearer_auth(ADMIN_TOKEN)
        .send()
        .await
        .expect("refresh")
        .json()
        .await
        .expect("refresh json");
    assert!(
        refreshed["revision"].as_u64().expect("revision")
            > shown["revision"].as_u64().expect("revision")
    );
    assert_eq!(refreshed["models"], json!(["alpha", "beta"]));
    assert_eq!(refreshed["ready"], true);

    // Authentication failure keeps the provider not-ready with a stable code.
    create_discovery_provider(
        &client,
        &base,
        "auth",
        &format!("{fake}/auth/v1"),
        "beta",
        None,
    )
    .await;
    let (_, auth) = set_discovery_credential(&client, &base, "auth", "sk-bad").await;
    assert_eq!(auth["ready"], false);
    assert_eq!(auth["discovery_error_code"], "provider.discovery_auth");

    // Malformed responses do not invent a model.
    create_discovery_provider(
        &client,
        &base,
        "bad",
        &format!("{fake}/bad/v1"),
        "beta",
        None,
    )
    .await;
    let (_, malformed) = set_discovery_credential(&client, &base, "bad", "sk-good").await;
    assert_eq!(malformed["ready"], false);
    assert_eq!(
        malformed["discovery_error_code"],
        "provider.discovery_invalid"
    );

    // Empty discovery results fail rather than selecting a placeholder.
    create_discovery_provider(
        &client,
        &base,
        "empty",
        &format!("{fake}/empty/v1"),
        "beta",
        None,
    )
    .await;
    let (_, empty) = set_discovery_credential(&client, &base, "empty", "sk-good").await;
    assert_eq!(empty["ready"], false);
    assert_eq!(empty["discovery_error_code"], "provider.discovery_empty");

    // Ollama discovers without a credential.
    let ollama: Value = client
        .post(format!("{base}/v1/admin/model-providers"))
        .bearer_auth(ADMIN_TOKEN)
        .json(&json!({
            "id": "ollama",
            "adapter": "ollama",
            "model": "llama3:latest",
            "endpoint": fake,
            "modalities": ["text"],
            "local": true,
            "residency": "local",
            "allowed_workers": ["general-a"],
        }))
        .send()
        .await
        .expect("ollama create")
        .json()
        .await
        .expect("ollama body");
    assert_eq!(ollama["ready"], true);
    assert_eq!(ollama["models"], json!(["llama3:latest", "qwen2.5:7b"]));

    // The catalog and audit contain no credential value.
    let catalog = client
        .get(format!("{base}/v1/admin/model-providers"))
        .bearer_auth(ADMIN_TOKEN)
        .send()
        .await
        .expect("catalog")
        .text()
        .await
        .expect("catalog text");
    assert!(!catalog.contains("sk-good"));
    assert!(!catalog.contains("sk-bad"));
    let audit = client
        .get(format!("{base}/v1/admin/workers/audit"))
        .bearer_auth(ADMIN_TOKEN)
        .send()
        .await
        .expect("audit")
        .text()
        .await
        .expect("audit text");
    assert!(!audit.contains("sk-good"));
    assert!(!audit.contains("sk-bad"));

    let seen = fake_state.seen_auth.lock().expect("seen").clone();
    assert!(
        seen.iter().any(|value| value == "Bearer sk-good"),
        "discovery must use the credential server-side"
    );
}

#[tokio::test]
async fn missing_master_key_fails_gateway_startup() {
    let directory = tempfile::tempdir().expect("directory");
    let env_name = format!(
        "GOLDENBEE_TEST_MISSING_KEY_{}_{}",
        std::process::id(),
        common::now_ms()
    );
    let config = Config {
        listen: "127.0.0.1:0".into(),
        callers: vec![CallerConfig {
            token: ADMIN_TOKEN.into(),
            id: "operator".into(),
            namespace: "local".into(),
            scopes: vec!["admin".into()],
            workers: None,
        }],
        socket: SocketConfig::default(),
        core: CoreConfig {
            data_dir: directory.path().join("gateway").display().to_string(),
            vault: Some(VaultConfig {
                enabled: true,
                directory: directory.path().join("vault").display().to_string(),
                master_key_env: env_name,
            }),
            ..Default::default()
        },
    };
    let error = match Gateway::prepare(config).await {
        Ok(_) => panic!("missing master key must fail closed"),
        Err(error) => error,
    };
    assert_eq!(error.code, "vault.master_key_missing");
}
