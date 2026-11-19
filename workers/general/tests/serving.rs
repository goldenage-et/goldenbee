//! Cross-process Worker serving evidence: HTTP, A2A, and authentication.

use common::{Limits, SubmitTaskRequest};
use runtime::crypto::{issue_delegation, DelegationClaims};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

mod fixture;

struct Worker {
    child: Child,
    #[allow(dead_code)]
    data: tempfile::TempDir,
    base: String,
    instance: String,
    token: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(token: &str) -> Worker {
    start_with_gateway(
        token,
        fixture::Options {
            instance: "general-serving".into(),
            token: Some(token.to_string()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
        None,
    )
}

fn start_with_gateway(
    token: &str,
    options: fixture::Options,
    gateway: Option<fixture::GatewayFixture>,
) -> Worker {
    let data = tempfile::tempdir().unwrap();
    let fixture = fixture::write_with_gateway(data.path(), options, gateway);
    let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            fixture.config.to_str().unwrap(),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("worker serves");
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("listener address is announced");
    let base = line
        .split("listening on ")
        .nth(1)
        .expect("address announcement")
        .trim()
        .to_string();
    assert!(base.starts_with("http://127.0.0.1:"), "{line}");
    let client = reqwest::blocking::Client::new();
    let instance = client
        .get(format!("{base}/v1/health"))
        .send()
        .expect("health responds")
        .json::<Value>()
        .expect("health is JSON")["instance"]
        .as_str()
        .expect("instance identity")
        .to_string();
    Worker {
        child,
        data,
        base,
        instance,
        token: token.to_string(),
    }
}

impl Worker {
    fn client(&self) -> reqwest::blocking::Client {
        reqwest::blocking::Client::new()
    }

    fn authed(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        self.client()
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
    }

    fn submit(&self, agent: &str, input: &str, key: &str) -> reqwest::blocking::Response {
        let response = self
            .authed(reqwest::Method::POST, "/v1/tasks")
            .json(&SubmitTaskRequest {
                namespace: None,
                agent: agent.into(),
                session_id: "s1".into(),
                input: input.into(),
                idempotency_key: key.into(),
                profile: None,
                team: None,
                worker: None,
                limits: Limits::default(),
            })
            .send()
            .expect("submit responds");
        if !response.status().is_success() {
            panic!(
                "submit failed: {} {}",
                response.status(),
                response.text().unwrap()
            );
        }
        response
    }

    fn wait(&self, id: &str) -> Value {
        for _ in 0..200 {
            let task = self
                .authed(reqwest::Method::GET, &format!("/v1/tasks/{id}"))
                .send()
                .expect("task responds")
                .json::<Value>()
                .expect("task is JSON");
            if matches!(
                task["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled" | "unknown")
            ) {
                return task;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("task did not reach a terminal state");
    }
}

fn delegation(instance: &str, capabilities: &[&str]) -> String {
    issue_delegation(
        b"local-development-delegation-secret",
        &DelegationClaims {
            issuer: "gateway".into(),
            audience: instance.into(),
            principal: "caller".into(),
            namespace: "local".into(),
            scopes: vec!["task:submit".into(), "task:read".into()],
            capabilities: capabilities.iter().map(|id| (*id).into()).collect(),
            expires_at: common::now_ms() + 30_000,
            depth: 0,
            max_depth: 2,
            limits: Limits::default(),
            token_id: "t1".into(),
            team: None,
            path: Vec::new(),
        },
    )
    .expect("delegation issues")
}

#[test]
fn worker_serves_and_executes_with_gateway_stopped() {
    // A Gateway URL that cannot answer must never block local execution.
    let worker = start_with_gateway(
        "dev-token",
        fixture::Options {
            instance: "general-gateway-down".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
        Some(fixture::GatewayFixture {
            url: "http://127.0.0.1:1".into(),
            control_token: "gateway-is-not-running".into(),
            renew_interval_ms: 100,
            advertise_url: None,
        }),
    );
    let submitted: Value = worker
        .submit("general", "local execution without gateway", "gateway-down")
        .json()
        .unwrap();
    let task = worker.wait(submitted["task_id"].as_str().unwrap());
    assert_eq!(task["state"], "succeeded");
    assert!(task["output"]
        .as_str()
        .unwrap_or_default()
        .contains("local execution without gateway"));
}

#[test]
fn serving_requires_authorization_and_runs_native_http_and_a2a() {
    let worker = start("dev-token");
    let client = worker.client();

    let health: Value = client
        .get(format!("{}/v1/health", worker.base))
        .send()
        .expect("health responds")
        .json()
        .expect("health is JSON");
    assert_eq!(health["status"], "live");
    assert!(health.get("capabilities").is_none(), "liveness is minimal");

    assert_eq!(
        worker
            .client()
            .post(format!("{}/v1/tasks", worker.base))
            .json(&SubmitTaskRequest {
                namespace: None,
                agent: "general".into(),
                session_id: "s0".into(),
                input: "denied".into(),
                idempotency_key: "denied".into(),
                profile: None,
                team: None,
                worker: None,
                limits: Limits::default(),
            })
            .send()
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{}/v1/status", worker.base))
            .bearer_auth("wrong")
            .send()
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let capabilities: Value = worker
        .authed(reqwest::Method::GET, "/v1/capabilities")
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(capabilities["package"], "general");
    assert_eq!(capabilities["durable_tasks"], true);
    assert_eq!(capabilities["a2a_task_storage"], "sqlite");
    assert_eq!(capabilities["a2a_durable"], true);
    assert_eq!(capabilities["artifact_storage"], "native_adk");
    assert_eq!(
        capabilities["artifact_transfer"],
        "authenticated_or_scoped_grant"
    );
    assert_eq!(capabilities["artifact_max_bytes"], 64 * 1024 * 1024);
    assert!(capabilities["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["id"] == "general"));
    assert!(capabilities["installed_capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["id"] == "manual"));

    let malformed = worker
        .authed(reqwest::Method::POST, "/v1/tasks")
        .header("content-type", "application/json")
        .body("{\"agent\":")
        .send()
        .unwrap();
    assert_eq!(malformed.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(!malformed.text().unwrap().contains("dev-token"));

    let submitted: Value = worker
        .submit("general", "hello serving", "key-1")
        .json()
        .unwrap();
    let task = worker.wait(submitted["task_id"].as_str().unwrap());
    assert_eq!(task["state"], "succeeded");
    assert!(task["output"]
        .as_str()
        .unwrap_or_default()
        .contains("hello serving"));
    assert!(task["native_invocation_id"].as_str().is_some());

    let page: Value = worker
        .authed(
            reqwest::Method::GET,
            &format!(
                "/v1/tasks/{}/events",
                submitted["task_id"].as_str().unwrap()
            ),
        )
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert!(page["events"]
        .as_array()
        .is_some_and(|events| !events.is_empty()));

    let stream = worker
        .authed(
            reqwest::Method::GET,
            &format!(
                "/v1/tasks/{}/events/stream?after=0",
                submitted["task_id"].as_str().unwrap()
            ),
        )
        .send()
        .unwrap();
    assert_eq!(stream.status(), reqwest::StatusCode::OK);
    assert_eq!(
        stream
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let replay = stream.text().unwrap();
    assert!(replay.contains("event: task.succeeded"), "{replay}");
    assert!(replay.contains("\"durable\":true"), "{replay}");

    // Delegated product traffic carries audience and capability narrowing.
    assert_eq!(
        client
            .post(format!("{}/v1/tasks", worker.base))
            .bearer_auth(delegation("another-worker", &["general"]))
            .json(&SubmitTaskRequest {
                namespace: None,
                agent: "general".into(),
                session_id: "s2".into(),
                input: "wrong audience".into(),
                idempotency_key: "key-2".into(),
                profile: None,
                team: None,
                worker: None,
                limits: Limits::default(),
            })
            .send()
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .post(format!("{}/v1/tasks", worker.base))
            .bearer_auth(delegation(&worker.instance, &["echo"]))
            .json(&SubmitTaskRequest {
                namespace: None,
                agent: "general".into(),
                session_id: "s3".into(),
                input: "not granted".into(),
                idempotency_key: "key-3".into(),
                profile: None,
                team: None,
                worker: None,
                limits: Limits::default(),
            })
            .send()
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );

    // Native HTTP invocation is real, but unauthenticated callers cannot drive it.
    let native = |token: Option<&str>| {
        let mut request =
            client
                .post(format!("{}/api/run", worker.base))
                .json(&serde_json::json!({
                    "appName": "general",
                    "userId": "driver",
                    "sessionId": "native",
                    "newMessage": {"role": "user", "parts": [{"text": "hello http"}]},
                }));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().unwrap()
    };
    assert_eq!(native(None).status(), reqwest::StatusCode::UNAUTHORIZED);
    let response = native(Some("dev-token"));
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let events: Value = response.json().unwrap();
    assert!(events.to_string().contains("hello http"));

    // A2A discovery is public; the JSON-RPC invocation surface is not.
    assert_eq!(
        client
            .get(format!("{}/.well-known/agent.json", worker.base))
            .send()
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    let a2a = |token: Option<&str>| {
        let mut request = client
            .post(format!("{}/a2a", worker.base))
            .header("A2A-Version", "1.0")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "SendMessage",
                "params": {
                    "message": {
                        "role": "ROLE_USER",
                        "parts": [{"text": "hello a2a"}],
                        "messageId": "m1",
                    }
                },
                "id": 1,
            }));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().unwrap()
    };
    assert_eq!(a2a(None).status(), reqwest::StatusCode::UNAUTHORIZED);
    let response = a2a(Some("dev-token"));
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.text().unwrap();
    assert!(
        body.contains("echo: he") && body.contains("llo a2a"),
        "{body}"
    );
    assert!(!body.contains("dev-token"), "secrets stay out of responses");
}
