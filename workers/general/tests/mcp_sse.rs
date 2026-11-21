//! Ticket 12: legacy SSE compatibility adapter through the same registry,
//! destination policy, credential, and allowlist boundary as stdio and
//! Streamable HTTP.
//!
//! The stub speaks the deprecated 2024-11-05 HTTP+SSE transport directly over
//! TCP so the test proves the adapter, not the pinned Streamable HTTP client.

use common::{Limits, Principal, ProfileRevision, SubmitTaskRequest, Task, TaskState};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod fixture;

const TOKEN_ENV: &str = "GOLDENBEE_MCP_SSE_TOKEN";
const TOKEN: &str = "sk-sse-canary-4e1c";
const CREDENTIAL: &str = "secret://mcp/sse";

struct Stub {
    token: String,
    require_auth: bool,
    endpoint: String,
    calls: Mutex<Vec<String>>,
    stream: Mutex<Option<std::sync::mpsc::Sender<String>>>,
}

#[derive(Clone)]
struct StubHandle {
    state: Arc<Stub>,
}

impl StubHandle {
    fn calls(&self) -> Vec<String> {
        self.state.calls.lock().expect("calls lock").clone()
    }
}

fn start_stub(endpoint: Option<&str>) -> (String, StubHandle) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("stub listener");
    let address = listener.local_addr().expect("stub address");
    let state = Arc::new(Stub {
        token: TOKEN.into(),
        require_auth: true,
        endpoint: endpoint
            .map(str::to_string)
            .unwrap_or_else(|| "/messages?session_id=fixture".into()),
        calls: Mutex::new(Vec::new()),
        stream: Mutex::new(None),
    });
    let accept_state = state.clone();
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(stream) = connection else { continue };
            let state = accept_state.clone();
            std::thread::spawn(move || {
                let _ = serve(stream, &state);
            });
        }
    });
    (format!("http://{address}/sse"), StubHandle { state })
}

fn serve(stream: TcpStream, state: &Stub) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();

    if state.require_auth
        && headers.get("authorization").map(String::as_str)
            != Some(format!("Bearer {}", state.token).as_str())
    {
        return write_response(stream, "401 Unauthorized", "text/plain", "unauthorized");
    }

    if method == "GET" && path.starts_with("/sse") {
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n"
        )?;
        write!(stream, "event: endpoint\ndata: {}\n\n", state.endpoint)?;
        stream.flush()?;
        let (sender, receiver) = std::sync::mpsc::channel();
        *state.stream.lock().expect("stream lock") = Some(sender);
        while let Ok(message) = receiver.recv() {
            write!(stream, "event: message\ndata: {message}\n\n")?;
            stream.flush()?;
        }
        return Ok(());
    }

    if method == "POST" {
        let length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body)?;
        let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        write_response(stream, "202 Accepted", "text/plain", "")?;
        if let Some(response) = respond(state, &request) {
            if let Some(sender) = state.stream.lock().expect("stream lock").as_ref() {
                let _ = sender.send(response.to_string());
            }
        }
        return Ok(());
    }

    write_response(stream, "404 Not Found", "text/plain", "not found")
}

fn respond(state: &Stub, request: &Value) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
            "serverInfo": {"name": "goldenbee-sse-fixture", "version": "1.0.0"}
        }),
        "notifications/initialized" => return None,
        "ping" => json!({}),
        "tools/list" => json!({
            "tools": [{
                "name": "sse_echo",
                "description": "Echo one value over the legacy transport.",
                "inputSchema": {
                    "type": "object",
                    "properties": {"value": {"type": "string"}},
                    "required": ["value"]
                }
            }]
        }),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let value = request
                .pointer("/params/arguments/value")
                .and_then(Value::as_str)
                .unwrap_or_default();
            state
                .calls
                .lock()
                .expect("calls lock")
                .push(format!("{name}:{value}"));
            json!({
                "content": [{"type": "text", "text": format!("sse:{name}:{value}")}],
                "isError": false
            })
        }
        "resources/list" => json!({
            "resources": [{
                "uri": "file:///sse/note.txt",
                "name": "sse-note",
                "description": "A note served over legacy SSE.",
                "mimeType": "text/plain"
            }]
        }),
        "resources/templates/list" => json!({"resourceTemplates": []}),
        "resources/read" => json!({
            "contents": [{
                "uri": "file:///sse/note.txt",
                "mimeType": "text/plain",
                "text": "sse resource body"
            }]
        }),
        "prompts/list" => json!({"prompts": []}),
        _ => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "method not found"}
            }))
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn write_response(
    mut stream: TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn append_mcp(config: &std::path::Path, endpoint: &str) {
    let mut text = std::fs::read_to_string(config).expect("worker config");
    text.push_str(&format!(
        "\n[mcp.servers.sse]\ntransport = \"sse\"\nendpoint = \"{endpoint}\"\nallowed_tools = [\"sse_echo\"]\nallowed_resources = [\"file:///sse/note.txt\"]\ncredential = \"{CREDENTIAL}\"\nrequired_scopes = [\"mcp:call\"]\nallowed_hosts = [\"127.0.0.1\"]\nallow_loopback = true\nallow_insecure_http = true\ntimeout_ms = 5000\n"
    ));
    text.push_str(&format!(
        "\n[[mcp.credentials]]\nreference = \"{CREDENTIAL}\"\nsource = \"env:{TOKEN_ENV}\"\naudience = \"{endpoint}\"\nscopes = [\"mcp:call\"]\n"
    ));
    std::fs::write(config, text).expect("mcp config");
}

fn revision() -> ProfileRevision {
    ProfileRevision {
        revision: 0,
        instruction: "Use the configured legacy SSE MCP capability.".into(),
        model: "deterministic".into(),
        reasoning: None,
        capabilities: vec!["general".into()],
        grants: vec!["task:submit".into(), "mcp:remote:sse".into()],
        restrictions: Vec::new(),
        composition: "single".into(),
        members: vec!["general".into()],
        skills: Vec::new(),
        mcp: vec!["sse".into()],
        workflow: None,
        limits: Limits::default(),
        compaction: None,
        secret_refs: Vec::new(),
        created_at: 0,
    }
}

async fn wait(runtime: &runtime::Runtime, task_id: &str) -> Task {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let task = runtime.task_unscoped(task_id).await.expect("task exists");
        if task.state.is_terminal() {
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn call(server: &str, tool: &str, arguments: &str) -> String {
    format!("mcp:mcp__{server}__{tool} {arguments}")
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_sse_transport_calls_tools_and_resources_through_policy() {
    std::env::set_var(TOKEN_ENV, TOKEN);
    let (endpoint, stub) = start_stub(None);
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "mcp-sse".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    append_mcp(&fixture.config, &endpoint);

    let instance = general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let admin: Principal = instance.operator_principal().await.expect("principal");
    runtime
        .create_profile(&admin, "local", "sse", revision())
        .await
        .expect("profile");
    runtime
        .activate_profile(&admin, "local", "sse", 1)
        .await
        .expect("activate");

    let task = runtime
        .submit_task(
            &admin,
            SubmitTaskRequest {
                namespace: Some("local".into()),
                agent: "general".into(),
                session_id: "session-sse".into(),
                input: call("sse", "sse_echo", "{\"value\":\"legacy\"}"),
                idempotency_key: "sse".into(),
                profile: Some("sse".into()),
                team: None,
                worker: None,
                limits: Limits::default(),
            },
        )
        .await
        .expect("admitted");
    let task = wait(&runtime, &task.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert!(
        task.output
            .as_deref()
            .unwrap_or_default()
            .contains("sse:sse_echo:legacy"),
        "{:?}",
        task.output
    );
    assert_eq!(stub.calls(), vec!["sse_echo:legacy"]);

    let inventory = instance.remote_mcp().inventory().await;
    let server = inventory
        .iter()
        .find(|server| server.id == "sse")
        .expect("sse inventory");
    assert_eq!(server.transport, "sse");
    assert_eq!(server.state, "connected");
    let serialized = serde_json::to_string(&inventory).expect("inventory json");
    assert!(
        !serialized.contains(TOKEN),
        "credential leaked: {serialized}"
    );

    let resources = instance
        .remote_mcp()
        .resources("sse")
        .await
        .expect("resources over SSE");
    assert_eq!(resources[0].id, "mcp:sse:resource:file:///sse/note.txt");
    let content = instance
        .remote_mcp()
        .read_resource("sse", "file:///sse/note.txt")
        .await
        .expect("resource read over SSE");
    assert_eq!(content.text.as_deref(), Some("sse resource body"));

    let diagnostics = serde_json::to_string(&runtime.health().await).expect("health json");
    assert!(
        !diagnostics.contains(TOKEN),
        "credential leaked: {diagnostics}"
    );

    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_sse_message_url_must_stay_on_the_vetted_origin() {
    std::env::set_var(TOKEN_ENV, TOKEN);
    let (endpoint, _stub) = start_stub(Some("http://evil.test/messages"));
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "mcp-sse-origin".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    append_mcp(&fixture.config, &endpoint);
    let instance = general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
        .expect("worker starts");

    let error = instance
        .remote_mcp()
        .resources("sse")
        .await
        .expect_err("cross-origin message URL is denied");
    assert_eq!(error.code, "mcp.endpoint_denied");
    assert!(!error.message.contains(TOKEN));

    instance.shutdown().await;
}
