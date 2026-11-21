//! Remote MCP integration: native Runner tool calls, credential policy,
//! destination policy, tool ceilings, ambiguity, and instance/profile scoping.

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use common::{Limits, Principal, ProfileRevision, StreamEvent, SubmitTaskRequest, Task, TaskState};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod fixture;

const TOKEN_ENV: &str = "GOLDENBEE_MCP_TEST_TOKEN";
const TOKEN: &str = "sk-mcp-canary-6f9d";
const CREDENTIAL: &str = "secret://mcp/tools";

struct Stub {
    token: String,
    tools: Vec<&'static str>,
    calls: Mutex<Vec<String>>,
    effects: Mutex<Vec<String>>,
    redirect: Option<String>,
    require_auth: bool,
}

#[derive(Clone)]
struct StubHandle {
    state: Arc<Stub>,
}

impl StubHandle {
    fn calls(&self) -> Vec<String> {
        self.state.calls.lock().expect("calls lock").clone()
    }

    fn effects(&self) -> Vec<String> {
        self.state.effects.lock().expect("effects lock").clone()
    }
}

async fn start_stub(
    tools: &[&'static str],
    redirect: Option<String>,
    require_auth: bool,
) -> (String, StubHandle) {
    let state = Arc::new(Stub {
        token: TOKEN.into(),
        tools: tools.to_vec(),
        calls: Mutex::new(Vec::new()),
        effects: Mutex::new(Vec::new()),
        redirect,
        require_auth,
    });
    let app = Router::new()
        .route("/mcp", post(stub_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub listener");
    let addr = listener.local_addr().expect("stub address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/mcp"), StubHandle { state })
}

async fn stub_handler(
    State(state): State<Arc<Stub>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if state.require_auth {
        let expected = format!("Bearer {}", state.token);
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some(expected.as_str())
        {
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    }
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "initialize" => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "goldenbee-test-mcp", "version": "1.0.0"}
            }
        }))
        .into_response(),
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/list" => {
            let tools = state
                .tools
                .iter()
                .map(|name| {
                    json!({
                        "name": name,
                        "description": format!("stub tool {name}"),
                        "inputSchema": {"type": "object", "properties": {}}
                    })
                })
                .collect::<Vec<_>>();
            Json(json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools}})).into_response()
        }
        "tools/call" => {
            let name = body
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if name == "write_record" {
                if let Some(target) = &state.redirect {
                    return (
                        StatusCode::TEMPORARY_REDIRECT,
                        [(header::LOCATION, target.as_str())],
                    )
                        .into_response();
                }
            }
            state
                .calls
                .lock()
                .expect("calls lock")
                .push(name.to_string());
            state.effects.lock().expect("effects lock").push(format!(
                "{name}:{}",
                body.pointer("/params/arguments/value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            ));
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": format!("ok:{name}")}],
                    "isError": false
                }
            }))
            .into_response()
        }
        _ => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": "method not found"}
        }))
        .into_response(),
    }
}

/// Raw HTTP server that completes the MCP handshake and then truncates the
/// response to the first `tools/call` with a declared, too-large body.
fn start_interrupting_server(tool: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("raw listener");
    let addr = listener.local_addr().expect("raw address");
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let counter = counter.clone();
            std::thread::spawn(move || serve_raw(stream, tool, counter));
        }
    });
    (format!("http://{addr}/mcp"), calls)
}

fn serve_raw(mut stream: TcpStream, tool: &'static str, calls: Arc<AtomicUsize>) {
    let Some(request) = read_http(&mut stream) else {
        return;
    };
    let Ok(body) = serde_json::from_slice::<Value>(&request) else {
        return;
    };
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "initialize" => respond(
            &mut stream,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "interrupting", "version": "1.0.0"}
                }
            }),
        ),
        "notifications/initialized" => {
            let _ = stream.write_all(
                b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
        "tools/list" => respond(
            &mut stream,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"tools": [{
                    "name": tool,
                    "description": "mutating stub",
                    "inputSchema": {"type": "object", "properties": {}}
                }]}
            }),
        ),
        "tools/call" => {
            calls.fetch_add(1, Ordering::SeqCst);
            // One full request reached the server and may have committed.
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":",
            );
            let _ = stream.flush();
            // Drop the socket without completing the declared body.
        }
        _ => respond(
            &mut stream,
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}}),
        ),
    }
}

fn read_http(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        let Some(split) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&buffer[..split]).to_ascii_lowercase();
        let length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if buffer.len() >= split + 4 + length {
            return Some(buffer[split + 4..split + 4 + length].to_vec());
        }
    }
    None
}

fn respond(stream: &mut TcpStream, value: Value) {
    let body = value.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

struct Fixture {
    #[allow(dead_code)]
    instance: runtime::worker::WorkerInstance,
    runtime: runtime::Runtime,
    admin: Principal,
}

#[allow(clippy::too_many_arguments)]
fn append_mcp(
    path: &Path,
    id: &str,
    endpoint: &str,
    allowed_hosts: &[&str],
    allow_loopback: bool,
    allow_insecure_http: bool,
    allowed_tools: &[&str],
    expires_at: i64,
    revoked: bool,
) {
    let mut config = std::fs::read_to_string(path).expect("worker config");
    config.push_str(&format!(
        "\n[mcp.servers.{id}]\nendpoint = \"{endpoint}\"\nallowed_tools = [{}]\ncredential = \"{CREDENTIAL}\"\nrequired_scopes = [\"mcp:call\"]\nallowed_hosts = [{}]\nallow_loopback = {}\nallow_insecure_http = {}\ntimeout_ms = 5000\n",
        allowed_tools
            .iter()
            .map(|tool| format!("\"{tool}\""))
            .collect::<Vec<_>>()
            .join(", "),
        allowed_hosts
            .iter()
            .map(|host| format!("\"{host}\""))
            .collect::<Vec<_>>()
            .join(", "),
        allow_loopback,
        allow_insecure_http
    ));
    config.push_str(&format!(
        "\n[[mcp.credentials]]\nreference = \"{CREDENTIAL}\"\nsource = \"env:{TOKEN_ENV}\"\naudience = \"{endpoint}\"\nscopes = [\"mcp:call\"]\nexpires_at = {expires_at}\nrevoked = {revoked}\n"
    ));
    std::fs::write(path, config).expect("mcp config");
}

#[allow(clippy::too_many_arguments)]
async fn fixture(
    name: &str,
    endpoint: &str,
    allowed_hosts: &[&str],
    allow_loopback: bool,
    allow_insecure_http: bool,
    allowed_tools: &[&str],
    expires_at: i64,
    revoked: bool,
) -> Fixture {
    let data = tempfile::tempdir().expect("data");
    // Keep the data directory alive for the Worker lifetime by leaking the
    // handle in this test process.
    let path = data.keep();
    let fixture = fixture::write(
        &path,
        fixture::Options {
            instance: name.into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    append_mcp(
        &fixture.config,
        "tools",
        endpoint,
        allowed_hosts,
        allow_loopback,
        allow_insecure_http,
        allowed_tools,
        expires_at,
        revoked,
    );
    let instance = general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let admin = instance
        .operator_principal()
        .await
        .expect("operator principal");
    Fixture {
        instance,
        runtime,
        admin,
    }
}

fn revision(mcp: &[&str], grants: &[&str]) -> ProfileRevision {
    ProfileRevision {
        revision: 0,
        instruction: "remote MCP test profile".into(),
        model: "deterministic".into(),
        capabilities: Vec::new(),
        grants: grants.iter().map(|value| (*value).into()).collect(),
        restrictions: Vec::new(),
        composition: "single".into(),
        members: Vec::new(),
        skills: Vec::new(),
        mcp: mcp.iter().map(|value| (*value).into()).collect(),
        workflow: None,
        limits: Limits::default(),
        compaction: None,
        secret_refs: Vec::new(),
        created_at: 0,
    }
}

fn request(agent: &str, input: &str, key: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: agent.into(),
        session_id: format!("session-{key}"),
        input: input.into(),
        idempotency_key: key.into(),
        profile: Some("remote".into()),
        team: None,
        worker: None,
        limits: Limits::default(),
    }
}

async fn activate_profile(fixture: &Fixture, grants: &[&str]) {
    fixture
        .runtime
        .create_profile(
            &fixture.admin,
            "local",
            "remote",
            revision(&["tools"], grants),
        )
        .await
        .expect("profile created");
    fixture
        .runtime
        .activate_profile(&fixture.admin, "local", "remote", 1)
        .await
        .expect("profile activated");
}

async fn wait(runtime: &runtime::Runtime, task_id: &str) -> Task {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let task = runtime.task_unscoped(task_id).await.expect("task exists");
        if task.state.is_terminal() || task.state == TaskState::Unknown {
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn events(runtime: &runtime::Runtime, task_id: &str) -> Vec<StreamEvent> {
    runtime
        .events_since(task_id, 0, 1_000)
        .await
        .expect("events")
        .events
}

async fn events_until(runtime: &runtime::Runtime, task_id: &str, kind: &str) -> Vec<StreamEvent> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let events = events(runtime, task_id).await;
        if events.iter().any(|event| event.kind == kind) {
            return events;
        }
        assert!(Instant::now() < deadline, "event {kind} was not recorded");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn native_runner_calls_remote_tool_and_records_evidence() {
    std::env::set_var(TOKEN_ENV, TOKEN);
    let (endpoint, stub) = start_stub(&["write_record"], None, true).await;
    let fixture = fixture(
        "mcp-success",
        &endpoint,
        &["127.0.0.1"],
        true,
        true,
        &["write_record"],
        0,
        false,
    )
    .await;
    activate_profile(&fixture, &["task:submit", "mcp:remote:tools"]).await;
    let submitted = fixture
        .runtime
        .submit_task(
            &fixture.admin,
            request(
                "general",
                "mcp:mcp__tools__write_record {\"value\":\"one\"}",
                "mcp-success",
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&fixture.runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert_eq!(stub.calls(), vec!["write_record".to_string()]);
    assert_eq!(stub.effects(), vec!["write_record:one".to_string()]);
    assert_eq!(task.usage.tool_calls, 1);
    let events = events_until(&fixture.runtime, &task.id, "task.succeeded").await;
    let call = events
        .iter()
        .find(|event| event.kind == "tool.call")
        .expect("native tool.call evidence");
    assert_eq!(call.payload["name"], "mcp__tools__write_record");
    assert_eq!(call.payload["server"], "tools");
    let result = events
        .iter()
        .find(|event| event.kind == "tool.result")
        .expect("native tool.result evidence");
    assert_eq!(result.payload["ok"], true);
    assert!(events.iter().any(|event| event.kind == "task.succeeded"));
    let encoded = serde_json::to_string(&events).expect("events serialize");
    assert!(
        !encoded.contains(TOKEN),
        "token leaked into events: {encoded}"
    );
    fixture.instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn loaded_and_manual_agents_share_the_same_remote_context() {
    std::env::set_var(TOKEN_ENV, TOKEN);
    let (endpoint, stub) = start_stub(&["write_record"], None, true).await;
    let fixture = fixture(
        "mcp-manual",
        &endpoint,
        &["127.0.0.1"],
        true,
        true,
        &["write_record"],
        0,
        false,
    )
    .await;
    activate_profile(&fixture, &["task:submit", "mcp:remote:tools"]).await;
    for (agent, key) in [("general", "loaded"), ("manual", "manual")] {
        let submitted = fixture
            .runtime
            .submit_task(
                &fixture.admin,
                request(
                    agent,
                    "mcp:mcp__tools__write_record {\"value\":\"shared\"}",
                    key,
                ),
            )
            .await
            .expect("task admitted");
        let task = wait(&fixture.runtime, &submitted.task_id).await;
        assert_eq!(task.state, TaskState::Succeeded, "{agent}: {task:?}");
    }
    assert_eq!(
        stub.calls(),
        vec!["write_record".to_string(), "write_record".to_string()]
    );
    fixture.instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unauthorized_destination_is_denied_before_any_request() {
    std::env::set_var(TOKEN_ENV, TOKEN);
    let (endpoint, stub) = start_stub(&["write_record"], None, true).await;
    // Loopback is present in the allowlist but private/loopback access is not
    // enabled for this deployment.
    let fixture = fixture(
        "mcp-endpoint",
        &endpoint,
        &["127.0.0.1"],
        false,
        false,
        &["write_record"],
        0,
        false,
    )
    .await;
    activate_profile(&fixture, &["task:submit", "mcp:remote:tools"]).await;
    let submitted = fixture
        .runtime
        .submit_task(
            &fixture.admin,
            request(
                "general",
                "mcp:mcp__tools__write_record {}",
                "endpoint-denied",
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&fixture.runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(
        task.error.as_ref().map(|error| error.code.as_str()),
        Some("mcp.endpoint_denied")
    );
    assert!(stub.calls().is_empty(), "no request may reach the endpoint");
    fixture.instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn advertised_tools_cannot_exceed_the_installed_ceiling() {
    std::env::set_var(TOKEN_ENV, TOKEN);
    let (endpoint, stub) = start_stub(&["write_record", "delete_record"], None, true).await;
    let fixture = fixture(
        "mcp-ceiling",
        &endpoint,
        &["127.0.0.1"],
        true,
        true,
        &["write_record"],
        0,
        false,
    )
    .await;
    activate_profile(&fixture, &["task:submit", "mcp:remote:tools"]).await;
    let submitted = fixture
        .runtime
        .submit_task(
            &fixture.admin,
            request("general", "mcp:mcp__tools__delete_record {}", "tool-denied"),
        )
        .await
        .expect("task admitted");
    let task = wait(&fixture.runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert!(stub.calls().is_empty(), "denied tool must never be called");
    let events = events(&fixture.runtime, &task.id).await;
    assert!(
        events
            .iter()
            .any(|event| event.kind == "tool.result" && event.payload["ok"] == false),
        "the denied call must remain visible as a tool result"
    );
    fixture.instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_and_revoked_credentials_block_new_calls() {
    std::env::set_var(TOKEN_ENV, TOKEN);
    let (endpoint, stub) = start_stub(&["write_record"], None, true).await;
    let expired = fixture(
        "mcp-expired",
        &endpoint,
        &["127.0.0.1"],
        true,
        true,
        &["write_record"],
        common::now_ms() - 1,
        false,
    )
    .await;
    activate_profile(&expired, &["task:submit", "mcp:remote:tools"]).await;
    let submitted = expired
        .runtime
        .submit_task(
            &expired.admin,
            request("general", "mcp:mcp__tools__write_record {}", "expired"),
        )
        .await
        .expect("task admitted");
    let task = wait(&expired.runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(
        task.error.as_ref().map(|error| error.code.as_str()),
        Some("mcp.credential_expired")
    );
    assert!(stub.calls().is_empty());
    expired.instance.shutdown().await;

    let live = fixture(
        "mcp-revoked",
        &endpoint,
        &["127.0.0.1"],
        true,
        true,
        &["write_record"],
        0,
        false,
    )
    .await;
    activate_profile(&live, &["task:submit", "mcp:remote:tools"]).await;
    let first = live
        .runtime
        .submit_task(
            &live.admin,
            request(
                "general",
                "mcp:mcp__tools__write_record {\"value\":\"first\"}",
                "revoke-first",
            ),
        )
        .await
        .expect("task admitted");
    assert_eq!(
        wait(&live.runtime, &first.task_id).await.state,
        TaskState::Succeeded
    );
    live.runtime
        .revoke_mcp_credential(&live.admin, CREDENTIAL)
        .await
        .expect("credential revoked");
    let second = live
        .runtime
        .submit_task(
            &live.admin,
            request(
                "general",
                "mcp:mcp__tools__write_record {\"value\":\"second\"}",
                "revoke-second",
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&live.runtime, &second.task_id).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(
        task.error.as_ref().map(|error| error.code.as_str()),
        Some("mcp.credential_revoked")
    );
    assert_eq!(
        stub.calls(),
        vec!["write_record".to_string()],
        "revocation must block every new request"
    );
    live.instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_redirect_is_not_followed() {
    std::env::set_var(TOKEN_ENV, TOKEN);
    let (target, target_stub) = start_stub(&["write_record"], None, true).await;
    let (endpoint, _redirect_stub) = start_stub(&["write_record"], Some(target), true).await;
    let fixture = fixture(
        "mcp-redirect",
        &endpoint,
        &["127.0.0.1"],
        true,
        true,
        &["write_record"],
        0,
        false,
    )
    .await;
    activate_profile(&fixture, &["task:submit", "mcp:remote:tools"]).await;
    let submitted = fixture
        .runtime
        .submit_task(
            &fixture.admin,
            request("general", "mcp:mcp__tools__write_record {}", "redirect"),
        )
        .await
        .expect("task admitted");
    let task = wait(&fixture.runtime, &submitted.task_id).await;
    assert!(task.state == TaskState::Succeeded || task.state == TaskState::Unknown);
    assert!(target_stub.calls().is_empty(), "redirect target was dialed");
    let events = events(&fixture.runtime, &task.id).await;
    let result = events
        .iter()
        .find(|event| event.kind == "tool.result")
        .expect("tool result");
    assert_eq!(
        result.payload["error_code"], "mcp.redirect_denied",
        "{}",
        result.payload
    );
    fixture.instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_write_is_unknown_and_not_replayed() {
    std::env::set_var(TOKEN_ENV, TOKEN);
    let (endpoint, calls) = start_interrupting_server("write_record");
    let fixture = fixture(
        "mcp-interrupted",
        &endpoint,
        &["127.0.0.1"],
        true,
        true,
        &["write_record"],
        0,
        false,
    )
    .await;
    activate_profile(&fixture, &["task:submit", "mcp:remote:tools"]).await;
    let submitted = fixture
        .runtime
        .submit_task(
            &fixture.admin,
            request(
                "general",
                "mcp:mcp__tools__write_record {\"value\":\"once\"}",
                "interrupted",
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&fixture.runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Unknown, "{task:?}");
    assert!(task.uncertainty.is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the write was replayed");
    let events = events(&fixture.runtime, &task.id).await;
    let result = events
        .iter()
        .find(|event| event.kind == "tool.result")
        .expect("tool result");
    assert_eq!(result.payload["error_code"], "mcp.outcome_unknown");
    assert!(events
        .iter()
        .any(|event| event.kind == "tool.outcome_unknown"));

    // An unrelated task still succeeds on the same Worker.
    let (endpoint, stub) = start_stub(&["write_record"], None, true).await;
    let unrelated = fixture
        .runtime
        .submit_task(
            &fixture.admin,
            SubmitTaskRequest {
                profile: None,
                agent: "general".into(),
                session_id: "session-unrelated".into(),
                input: "plain: unrelated".into(),
                idempotency_key: "unrelated".into(),
                ..request("general", "", "unrelated")
            },
        )
        .await
        .expect("unrelated task admitted");
    assert_eq!(
        wait(&fixture.runtime, &unrelated.task_id).await.state,
        TaskState::Succeeded
    );
    assert!(stub.calls().is_empty());
    let _ = endpoint;
    fixture.instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_and_exports_do_not_contain_credentials_or_endpoints() {
    std::env::set_var(TOKEN_ENV, TOKEN);
    let (endpoint, _stub) = start_stub(&["write_record"], None, true).await;
    let fixture = fixture(
        "mcp-export",
        &endpoint,
        &["127.0.0.1"],
        true,
        true,
        &["write_record"],
        0,
        false,
    )
    .await;
    let manifest = fixture.instance.manifest();
    let encoded = serde_json::to_string(manifest).expect("manifest");
    assert!(!encoded.contains(TOKEN));
    assert!(!encoded.contains(&endpoint));
    let config = fixture
        .runtime
        .config()
        .capabilities
        .iter()
        .find(|capability| capability.kind == "mcp")
        .expect("mcp capability descriptor");
    assert_eq!(config.id, "mcp:tools");
    assert_eq!(config.permissions, vec!["mcp:remote:tools".to_string()]);
    fixture.instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_must_grant_the_connection_and_name_an_instance_server() {
    std::env::set_var(TOKEN_ENV, TOKEN);
    let (endpoint, _stub) = start_stub(&["write_record"], None, true).await;
    let fixture = fixture(
        "mcp-profile-scope",
        &endpoint,
        &["127.0.0.1"],
        true,
        true,
        &["write_record"],
        0,
        false,
    )
    .await;
    let denied = fixture
        .runtime
        .create_profile(
            &fixture.admin,
            "local",
            "denied",
            revision(&["tools"], &["task:submit"]),
        )
        .await
        .expect_err("profile must grant the MCP connection");
    assert_eq!(denied.code, "profile.mcp_grant_missing");

    let unknown = fixture
        .runtime
        .create_profile(
            &fixture.admin,
            "local",
            "unknown",
            revision(
                &["other-instance"],
                &["task:submit", "mcp:remote:other-instance"],
            ),
        )
        .await
        .expect_err("profile cannot name another instance's connection");
    assert_eq!(unknown.code, "profile.mcp_unavailable");
    fixture.instance.shutdown().await;
}
