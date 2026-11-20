//! SDK-to-Gateway-to-Worker fenced control semantics.
//!
//! A fake Worker is the only lease authority. The test proves the Gateway
//! forwards the Worker-issued epoch and generation unchanged, issues a narrow
//! delegation, and rejects a caller that lacks the control scope.

use common::{
    scopes, ControlCommand, ControlCommandRequest, ControlHandBackRequest, ControlLeaseRequest,
    ControlOwner,
};
use gateway_core::{CoreConfig, WorkerConfig};
use goldenbee_gateway::{CallerConfig, Config, Gateway};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

fn read_request(stream: &mut std::net::TcpStream) -> (String, String, String) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).expect("request read");
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(head) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..head]).to_string();
            let length = head
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length: ")
                        .or_else(|| line.strip_prefix("content-length: "))
                })
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let start = head.len() + 4;
            while buffer.len() < start + length {
                let read = stream.read(&mut chunk).expect("body read");
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
            let body = String::from_utf8_lossy(&buffer[start..]).to_string();
            let line = head.lines().next().unwrap_or_default().to_string();
            let auth = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    if name.eq_ignore_ascii_case("authorization") {
                        value.trim().strip_prefix("Bearer ").map(str::to_string)
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            return (line, body, auth);
        }
    }
    (String::new(), String::new(), String::new())
}

fn respond(stream: &mut std::net::TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("response write");
}

#[tokio::test]
async fn sdk_control_semantics_survive_gateway_routing_without_local_lease_state() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fake worker binds");
    let worker_endpoint = format!("http://{}", listener.local_addr().expect("address"));
    let tokens = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = tokens.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..7 {
            let (mut stream, _) = listener.accept().expect("fake worker accepts");
            let (line, _body, auth) = read_request(&mut stream);
            if !auth.is_empty() && line.contains("/control") {
                seen.lock().expect("token lock").push(auth);
            }
            let response = if line.starts_with("GET /v1/health") {
                serde_json::json!({
                    "status": "live",
                    "instance": "worker-a",
                    "incarnation": "inc-1"
                })
            } else if line.starts_with("GET /v1/capabilities") {
                serde_json::json!({
                    "package": "general",
                    "version": "0.1.0",
                    "protocol": common::PROTOCOL_VERSION,
                    "instance": "worker-a",
                    "capabilities": []
                })
            } else if line.contains("/control/commands") {
                serde_json::json!({
                    "session_id": "session-a",
                    "generation": 3,
                    "command": "click",
                    "output": "",
                    "page": {"url": "http://127.0.0.1/", "title": "fixture", "text": "count=1"}
                })
            } else if line.contains("/control/handback") {
                serde_json::json!({
                    "session_id": "session-a",
                    "generation": 4,
                    "owner": "agent",
                    "epoch": "worker-epoch",
                    "expires_at_ms": 0
                })
            } else if line.contains("/control/audit") {
                serde_json::json!([{
                    "at": 1,
                    "action": "acquire",
                    "session_id": "session-a",
                    "principal": "caller",
                    "generation": 3,
                    "outcome": "accepted",
                    "detail": "takeover"
                }])
            } else {
                serde_json::json!({
                    "session_id": "session-a",
                    "generation": 3,
                    "owner": "human",
                    "principal": "caller",
                    "epoch": "worker-epoch",
                    "expires_at_ms": 123
                })
            };
            respond(&mut stream, &response.to_string());
        }
    });

    let data = tempfile::tempdir().expect("gateway data");
    let config = Config {
        listen: "127.0.0.1:0".into(),
        callers: vec![
            CallerConfig {
                token: "caller-token".into(),
                id: "caller".into(),
                namespace: "app".into(),
                scopes: vec![scopes::SESSION_CONTROL.into()],
                workers: None,
            },
            CallerConfig {
                token: "viewer-token".into(),
                id: "viewer".into(),
                namespace: "app".into(),
                scopes: vec![scopes::TASK_READ.into()],
                workers: None,
            },
        ],
        local: Default::default(),
        core: CoreConfig {
            data_dir: data.path().display().to_string(),
            workers: vec![WorkerConfig {
                id: "worker-a".into(),
                endpoint: worker_endpoint,
                instance: Some("worker-a".into()),
                package: Some("general".into()),
                capabilities: None,
                control_token: None,
                preflight: Some(true),
            }],
            delegation_secret: Some("test-control-secret".into()),
            ..CoreConfig::default()
        },
        socket: Default::default(),
    };
    let gateway = Gateway::prepare(config).await.expect("gateway prepares");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("gateway binds");
    let base = format!("http://{}", listener.local_addr().expect("gateway address"));
    tokio::spawn(async move {
        axum::serve(listener, gateway.router()).await.ok();
    });

    let client = sdk::Client::new(&base, "caller-token");
    let lease = client
        .acquire_control(
            "worker-a",
            &ControlLeaseRequest {
                session_id: "session-a".into(),
                ttl_ms: Some(60_000),
            },
        )
        .await
        .expect("SDK acquires");
    assert_eq!(lease.generation, 3);
    assert_eq!(lease.owner, ControlOwner::Human);
    assert_eq!(lease.epoch, "worker-epoch");

    let status = client
        .control_status("worker-a", "session-a")
        .await
        .expect("SDK reads status");
    assert_eq!(status.owner, ControlOwner::Human);

    let result = client
        .control_command(
            "worker-a",
            &ControlCommandRequest {
                session_id: "session-a".into(),
                generation: lease.generation,
                epoch: lease.epoch.clone(),
                command: ControlCommand::Click {
                    selector: "#go".into(),
                },
            },
        )
        .await
        .expect("SDK runs the fenced command");
    assert_eq!(result.page.text, "count=1");

    let hand_back = client
        .hand_back_control(
            "worker-a",
            &ControlHandBackRequest {
                session_id: "session-a".into(),
                generation: lease.generation,
                epoch: lease.epoch.clone(),
            },
        )
        .await
        .expect("SDK hands back");
    assert_eq!(hand_back.owner, ControlOwner::Agent);

    let audit = client
        .control_audit("worker-a", "session-a")
        .await
        .expect("SDK reads audit");
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, "acquire");

    let denied = sdk::Client::new(&base, "viewer-token")
        .acquire_control(
            "worker-a",
            &ControlLeaseRequest {
                session_id: "session-a".into(),
                ttl_ms: None,
            },
        )
        .await
        .expect_err("a caller without control scope is denied");
    assert_eq!(denied.code, "scope.denied");

    server.join().expect("fake worker joins");
    let tokens = tokens.lock().expect("token lock");
    assert!(!tokens.is_empty(), "control requests carried a delegation");
    for token in tokens.iter() {
        let claims = common::delegation::verify_delegation(
            b"test-control-secret",
            token,
            "worker-a",
            common::now_ms(),
        )
        .expect("control delegation verifies");
        assert_eq!(claims.scopes, vec![scopes::SESSION_CONTROL.to_string()]);
        assert_eq!(claims.principal, "caller");
        assert!(claims.capabilities.is_empty());
    }
}
