//! Gateway control-plane unit tests.

use std::sync::Arc;

use common::delegation::verify_delegation;
use common::{
    scopes, CapabilityDescriptor, Capacity, GatewaySubmitRequest, Limits, Principal, WorkerSummary,
};

use super::submit::{request_digest, route};
use super::worker_client::issue_worker_delegation;
use super::{Gateway, State};
use crate::caller::Caller;
use crate::registry::{Approval, Clock, Registry, Verified};
use crate::store::{Control, FileDispatchStore, ReplicaId};

fn summary(instance: &str) -> WorkerSummary {
    WorkerSummary {
        id: "worker-a".into(),
        endpoint: "http://127.0.0.1:1".into(),
        instance: instance.into(),
        package: "general".into(),
        version: "0.1.0".into(),
        capabilities: vec![CapabilityDescriptor {
            id: "echo".into(),
            kind: "agent".into(),
            description: "echo".into(),
            permissions: Vec::new(),
        }],
        declared_capabilities: vec![CapabilityDescriptor {
            id: "echo".into(),
            kind: "agent".into(),
            description: "echo".into(),
            permissions: Vec::new(),
        }],
        approved_capabilities: vec!["echo".into()],
        protocol: common::PROTOCOL_VERSION.into(),
        incarnation: "inc-1".into(),
        incarnations: Vec::new(),
        locality: common::LOCALITY_LOCAL.into(),
        capacity: Capacity::default(),
        lease_expires_at: common::now_ms() + 1_000,
        renewed_at: common::now_ms(),
        status: "registered".into(),
        reachable: true,
    }
}

async fn state() -> (State, tempfile::TempDir) {
    state_with("http://127.0.0.1:1").await
}

async fn state_with(endpoint: &str) -> (State, tempfile::TempDir) {
    let clock: Clock = Arc::new(common::now_ms);
    let directory = tempfile::tempdir().expect("dispatch directory");
    let dispatch = std::sync::Arc::new(
        FileDispatchStore::open(directory.path())
            .await
            .expect("dispatch store opens"),
    );
    let registry = Control::File(
        Registry::open(
            None,
            clock,
            1_000,
            vec![Approval {
                id: "worker-a".into(),
                endpoint: endpoint.into(),
                instance: Some("worker-a".into()),
                package: Some("general".into()),
                capabilities: None,
                control_token: Some("control-a".into()),
            }],
        )
        .expect("registry opens"),
    );
    let state = State {
        secret: b"secret".to_vec(),
        registry,
        dispatch: dispatch.clone(),
        streams: dispatch,
        replica: ReplicaId("test".into()),
        claim_ttl_ms: 1_000,
        store_description: "file:test".into(),
        client: reqwest::Client::new(),
        stream_client: reqwest::Client::new(),
        lifecycle_client: reqwest::Client::new(),
        stream_slots: Arc::new(tokio::sync::Semaphore::new(256)),
        hosts: Vec::new(),
        lifecycle: crate::lifecycle::Lifecycle::open(directory.path())
            .expect("lifecycle store opens"),
        vault: None,
    };
    (state, directory)
}

#[test]
fn issued_delegation_narrows_scope_capability_and_audience() {
    let principal = Principal::new("caller-1", "app", &[scopes::TASK_SUBMIT]);
    let token = issue_worker_delegation(
        b"secret",
        &summary("worker-a"),
        &principal,
        "echo",
        scopes::TASK_SUBMIT,
        &Limits::default(),
    )
    .expect("delegation issues");
    let claims = verify_delegation(b"secret", &token, "worker-a", common::now_ms())
        .expect("worker verifies");
    assert_eq!(claims.audience, "worker-a");
    assert_eq!(claims.principal, "caller-1");
    assert_eq!(claims.namespace, "app");
    assert_eq!(claims.capabilities, vec!["echo".to_string()]);
    assert_eq!(claims.scopes, vec![scopes::TASK_SUBMIT.to_string()]);
    assert!(claims.depth < claims.max_depth);
    assert!(claims.expires_at > common::now_ms());
    assert!(verify_delegation(b"secret", &token, "worker-b", common::now_ms()).is_err());
}

#[tokio::test]
async fn worker_allow_list_and_capability_checks_are_explicit() {
    let (state, _directory) = state().await;
    let now = state.registry.now();
    let verified = Verified {
        instance: "worker-a".into(),
        incarnation: "inc-1".into(),
        package: "general".into(),
        version: "0.1.0".into(),
        protocol: common::PROTOCOL_VERSION.into(),
        capabilities: vec![CapabilityDescriptor {
            id: "echo".into(),
            kind: "agent".into(),
            description: "echo".into(),
            permissions: Vec::new(),
        }],
    };
    state
        .registry
        .bootstrap("worker-a", &verified, now)
        .expect("bootstrap accepted");
    let allowed = Caller::new(Principal::new("caller", "app", &[scopes::TASK_SUBMIT]))
        .with_worker_allowlist(["worker-a"]);
    let denied = Caller::new(Principal::new("caller", "app", &[scopes::TASK_SUBMIT]))
        .with_worker_allowlist(["worker-b"]);
    assert!(route(&state, &allowed, "worker-a", "echo").is_ok());
    assert_eq!(
        route(&state, &allowed, "worker-a", "general")
            .unwrap_err()
            .code,
        "scope.capability_denied"
    );
    assert_eq!(
        route(&state, &denied, "worker-a", "echo").unwrap_err().code,
        "scope.worker_denied"
    );
    assert_eq!(
        route(&state, &allowed, "worker-missing", "echo")
            .unwrap_err()
            .code,
        "route.worker.not_found"
    );
}

#[test]
fn caller_scoped_digest_changes_with_payload_and_worker() {
    let request = GatewaySubmitRequest {
        worker: Some("worker-a".into()),
        agent: "general".into(),
        session_id: "s".into(),
        input: "one".into(),
        idempotency_key: "k".into(),
        profile: None,
        team: None,
        team_version: None,
        limits: Limits::default(),
    };
    let original = request_digest(&request, "worker-a");
    let mut changed = request.clone();
    changed.input = "two".into();
    assert_ne!(original, request_digest(&changed, "worker-a"));
    assert_ne!(original, request_digest(&request, "worker-b"));
    assert_eq!(
        original,
        request_digest(&request, "worker-a"),
        "the same Gateway request digest is stable"
    );
}

fn read_request(stream: &mut std::net::TcpStream) -> (String, String) {
    use std::io::{Read, Write};
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
            let _ = stream.write_all(b"");
            return (line, body);
        }
    }
    (String::new(), String::new())
}

fn respond(stream: &mut std::net::TcpStream, body: &str) {
    use std::io::Write;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("response write");
}

#[tokio::test]
async fn control_routes_forward_the_worker_lease_without_a_second_authority() {
    use common::{
        ControlCommand, ControlCommandRequest, ControlHandBackRequest, ControlLeaseRequest,
        ControlOwner,
    };
    use std::io::Write;
    use std::sync::Mutex;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fake worker binds");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let seen_server = seen.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().expect("fake worker accepts");
            let (line, body) = read_request(&mut stream);
            seen_server
                .lock()
                .expect("seen lock")
                .push((line.clone(), body));
            let response = if line.contains("/control/commands") {
                serde_json::json!({
                    "session_id": "session-a",
                    "generation": 7,
                    "command": "click",
                    "output": "",
                    "page": {"url": "http://127.0.0.1/", "title": "fixture", "text": "count=1"}
                })
                .to_string()
            } else if line.contains("/control/handback") {
                serde_json::json!({
                    "session_id": "session-a",
                    "generation": 8,
                    "owner": "agent",
                    "epoch": "worker-epoch",
                    "expires_at_ms": 0
                })
                .to_string()
            } else {
                serde_json::json!({
                    "session_id": "session-a",
                    "generation": 7,
                    "owner": "human",
                    "principal": "caller",
                    "epoch": "worker-epoch",
                    "expires_at_ms": 123
                })
                .to_string()
            };
            respond(&mut stream, &response);
            let _ = stream.flush();
        }
    });

    let (state, _directory) = state_with(&endpoint).await;
    let now = state.registry.now();
    state
        .registry
        .bootstrap(
            "worker-a",
            &Verified {
                instance: "worker-a".into(),
                incarnation: "inc-1".into(),
                package: "general".into(),
                version: "0.1.0".into(),
                protocol: common::PROTOCOL_VERSION.into(),
                capabilities: Vec::new(),
            },
            now,
        )
        .expect("bootstrap");
    let gateway = Gateway {
        state: Arc::new(state),
    };
    let caller = Caller::new(Principal::new("caller", "app", &[scopes::SESSION_CONTROL]));

    let lease = gateway
        .acquire_control(
            &caller,
            "worker-a",
            ControlLeaseRequest {
                session_id: "session-a".into(),
                ttl_ms: Some(60_000),
            },
        )
        .await
        .expect("acquire forwards");
    assert_eq!(lease.generation, 7);
    assert_eq!(lease.owner, ControlOwner::Human);
    assert_eq!(lease.epoch, "worker-epoch");

    let result = gateway
        .control_command(
            &caller,
            "worker-a",
            ControlCommandRequest {
                session_id: "session-a".into(),
                generation: lease.generation,
                epoch: lease.epoch.clone(),
                command: ControlCommand::Click {
                    selector: "#go".into(),
                },
            },
        )
        .await
        .expect("command forwards");
    assert_eq!(result.generation, 7);
    assert_eq!(result.page.text, "count=1");

    let status = gateway
        .control_status(&caller, "worker-a", "session-a")
        .await
        .expect("status forwards");
    assert_eq!(status.owner, ControlOwner::Human);

    let hand_back = gateway
        .hand_back_control(
            &caller,
            "worker-a",
            ControlHandBackRequest {
                session_id: "session-a".into(),
                generation: 7,
                epoch: "worker-epoch".into(),
            },
        )
        .await
        .expect("hand-back forwards");
    assert_eq!(hand_back.generation, 8);
    assert_eq!(hand_back.owner, ControlOwner::Agent);

    {
        let seen = seen.lock().expect("seen lock");
        assert_eq!(seen.len(), 4);
        assert!(seen[0].0.starts_with("POST /v1/sessions/session-a/control"));
        assert!(seen[0].1.contains("\"session_id\":\"session-a\""));
        assert!(seen[1]
            .0
            .starts_with("POST /v1/sessions/session-a/control/commands"));
        assert!(seen[1].1.contains("\"generation\":7"));
        assert!(seen[1].1.contains("\"epoch\":\"worker-epoch\""));
        assert!(seen[3]
            .0
            .starts_with("POST /v1/sessions/session-a/control/handback"));
    }
    server.join().expect("fake worker joins");

    let denied = Caller::new(Principal::new("caller", "app", &[scopes::TASK_READ]));
    let error = gateway
        .acquire_control(
            &denied,
            "worker-a",
            ControlLeaseRequest {
                session_id: "session-a".into(),
                ttl_ms: None,
            },
        )
        .await
        .expect_err("control scope is required");
    assert_eq!(error.code, "scope.denied");
}
