//! Two-replica acceptance for the shared Postgres adapter.
//!
//! The tests start a real Postgres container (or use
//! `GOLDENBEE_TEST_DATABASE_URL`) and in-process fake Worker HTTP endpoints.
//! They skip when neither is available; acceptance evidence must come from a
//! run where Postgres was available.

#![cfg(feature = "postgres")]

use common::{
    scopes, Capacity, GatewaySubmitRequest, Limits, Principal, StreamPage, TaskState,
    WorkerLeaseRequest, WorkerRegistrationRequest,
};
use gateway_core::{Caller, CoreConfig, Gateway, StoreConfig, WorkerConfig};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const SECRET: &str = "t56-shared-store-secret";
const CONTROL_TOKEN: &str = "t56-control-token";

struct PgFixture {
    name: String,
    url: String,
    external: bool,
}

impl PgFixture {
    fn start() -> Option<Self> {
        if let Ok(url) = std::env::var("GOLDENBEE_TEST_DATABASE_URL") {
            return Some(Self {
                name: String::new(),
                url,
                external: true,
            });
        }
        let name = format!("gb56-{}", uuid::Uuid::new_v4().simple());
        let output = Command::new("docker")
            .args([
                "run",
                "-d",
                "--rm",
                "--name",
                &name,
                "-e",
                "POSTGRES_PASSWORD=gb56",
                "-e",
                "POSTGRES_DB=postgres",
                "-p",
                "127.0.0.1::5432",
                "postgres:17-alpine",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            eprintln!(
                "skipping shared-store tests: docker could not start Postgres: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return None;
        }
        let port = loop {
            let output = Command::new("docker")
                .args(["port", &name, "5432/tcp"])
                .output()
                .ok()?;
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(port) = text.trim().rsplit(':').next() {
                if let Ok(port) = port.parse::<u16>() {
                    break port;
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        };
        let address = format!("127.0.0.1:{port}");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            let ready = Command::new("docker")
                .args(["exec", &name, "pg_isready", "-U", "postgres"])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);
            if ready && std::net::TcpStream::connect(&address).is_ok() {
                std::thread::sleep(Duration::from_millis(400));
                return Some(Self {
                    name,
                    url: format!("postgres://postgres:gb56@{address}/postgres"),
                    external: false,
                });
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        eprintln!("skipping shared-store tests: Postgres container did not become ready");
        None
    }
}

impl Drop for PgFixture {
    fn drop(&mut self) {
        if !self.external {
            let _ = Command::new("docker")
                .args(["rm", "-f", &self.name])
                .output();
        }
    }
}

#[derive(Default)]
struct WorkerState {
    tasks: Mutex<BTreeMap<String, Value>>,
    keys: Mutex<BTreeMap<String, String>>,
    next: AtomicU64,
    posts: AtomicUsize,
    stream_paths: Mutex<Vec<String>>,
    lost_ack: AtomicBool,
}

struct FakeWorker {
    base: String,
    instance: String,
    incarnation: String,
    state: Arc<WorkerState>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeWorker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeWorker {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("fake worker");
        let address = listener.local_addr().expect("fake worker address");
        let instance = format!("fake-worker-{}", uuid::Uuid::new_v4().simple());
        let incarnation = "inc-1".to_string();
        let state = Arc::new(WorkerState::default());
        let handler_state = state.clone();
        let handler_instance = instance.clone();
        let handler_incarnation = incarnation.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let state = handler_state.clone();
                let instance = handler_instance.clone();
                let incarnation = handler_incarnation.clone();
                tokio::spawn(async move {
                    let _ = handle(stream, state, &instance, &incarnation).await;
                });
            }
        });
        Self {
            base: format!("http://{address}"),
            instance,
            incarnation,
            state,
            task,
        }
    }

    fn posts(&self) -> usize {
        self.state.posts.load(Ordering::SeqCst)
    }
}

async fn handle(
    mut stream: TcpStream,
    state: Arc<WorkerState>,
    instance: &str,
    incarnation: &str,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = find(&buffer, b"\r\n\r\n") {
            break index + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.lines();
    let request = lines.next().unwrap_or_default();
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = &buffer[header_end..(header_end + content_length).min(buffer.len())];
    let path = target.split('?').next().unwrap_or(target);

    match (method, path) {
        ("GET", "/v1/health") => {
            respond_json(
                &mut stream,
                json!({
                    "status": "live",
                    "instance": instance,
                    "incarnation": incarnation,
                    "uptime_ms": 1,
                }),
            )
            .await?;
        }
        ("GET", "/v1/capabilities") => {
            respond_json(
                &mut stream,
                json!({
                    "package": "general",
                    "version": "0.1.0",
                    "protocol": common::PROTOCOL_VERSION,
                    "instance": instance,
                    "capabilities": [{
                        "id": "general",
                        "kind": "agent",
                        "description": "general",
                        "permissions": []
                    }]
                }),
            )
            .await?;
        }
        ("POST", "/v1/tasks") => {
            let request: common::SubmitTaskRequest =
                serde_json::from_slice(body).expect("worker submit body");
            state.posts.fetch_add(1, Ordering::SeqCst);
            let (task_id, replayed) = {
                let mut keys = state.keys.lock().unwrap();
                if let Some(task_id) = keys.get(&request.idempotency_key) {
                    (task_id.clone(), true)
                } else {
                    let id = format!("wtask-{}", state.next.fetch_add(1, Ordering::SeqCst));
                    keys.insert(request.idempotency_key.clone(), id.clone());
                    (id, false)
                }
            };
            {
                let mut tasks = state.tasks.lock().unwrap();
                tasks.entry(task_id.clone()).or_insert_with(|| {
                    json!({
                        "id": task_id,
                        "namespace": request.namespace.clone().unwrap_or_else(|| "local".into()),
                        "caller": "caller",
                        "agent": request.agent,
                        "session_id": request.session_id,
                        "input": request.input,
                        "idempotency_key": request.idempotency_key,
                        "request_digest": "sha256:worker",
                        "profile": "default@1",
                        "profile_revision": 1,
                        "state": "succeeded",
                        "attempt": 1,
                        "native_invocation_id": format!("inv-{task_id}"),
                        "output": "ok",
                        "limits": {},
                        "usage": {},
                        "created_at": common::now_ms(),
                        "updated_at": common::now_ms(),
                    })
                });
            }
            if state.lost_ack.swap(false, Ordering::SeqCst) {
                // The Worker recorded the task but the Gateway loses the ack.
                return Ok(());
            }
            respond_json(
                &mut stream,
                json!({
                    "task_id": task_id,
                    "state": "succeeded",
                    "replayed": replayed,
                    "agent": "general",
                    "profile": "default@1",
                }),
            )
            .await?;
        }
        ("GET", path) if path.starts_with("/v1/tasks/") && path.ends_with("/events/stream") => {
            state.stream_paths.lock().unwrap().push(target.to_string());
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                        Cache-Control: no-cache\r\nConnection: close\r\n\r\n";
            stream.write_all(head.as_bytes()).await?;
            stream
                .write_all(b"id: 1\nevent: task.updated\ndata: {\"seq\":1}\n\n")
                .await?;
            stream.shutdown().await?;
        }
        ("GET", path) if path.starts_with("/v1/tasks/") && path.ends_with("/events") => {
            let page = StreamPage {
                stream_id: "task:fake".into(),
                events: Vec::new(),
                next_seq: 1,
                oldest_seq: 0,
                cursor_expired: false,
            };
            respond_json(&mut stream, serde_json::to_value(page).unwrap()).await?;
        }
        ("GET", path) if path.starts_with("/v1/tasks/") => {
            let id = path.trim_start_matches("/v1/tasks/");
            let task = state.tasks.lock().unwrap().get(id).cloned();
            if let Some(task) = task {
                respond_json(&mut stream, task).await?;
            } else {
                respond_status(&mut stream, 404, "{\"code\":\"task.not_found\"}").await?;
            }
        }
        _ => respond_status(&mut stream, 404, "{\"code\":\"not_found\"}").await?,
    }
    Ok(())
}

async fn respond_json(stream: &mut TcpStream, value: Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(&value).unwrap();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.shutdown().await
}

async fn respond_status(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn config(
    fixture: &PgFixture,
    env_name: &str,
    worker: &FakeWorker,
    replica: &str,
    lease_ttl_ms: i64,
    worker_id: &str,
) -> CoreConfig {
    std::env::set_var(env_name, &fixture.url);
    CoreConfig {
        data_dir: tempfile::tempdir().unwrap().keep().display().to_string(),
        workers: vec![WorkerConfig {
            id: worker_id.into(),
            endpoint: worker.base.clone(),
            instance: Some(worker.instance.clone()),
            package: Some("general".into()),
            capabilities: None,
            control_token: Some(CONTROL_TOKEN.into()),
            preflight: None,
        }],
        hosts: Vec::new(),
        registry: None,
        lease_ttl_ms: Some(lease_ttl_ms),
        max_streams: 256,
        claim_ttl_ms: Some(5_000),
        replica_id: Some(replica.into()),
        store: Some(StoreConfig {
            kind: "postgres".into(),
            url_env: env_name.into(),
            max_connections: 4,
            migrate: true,
        }),
        vault: None,
        delegation_secret: Some(SECRET.into()),
        base_dir: None,
    }
}

fn caller(worker_id: &str) -> Caller {
    Caller::new(Principal::new(
        "caller",
        "local",
        &[
            scopes::TASK_SUBMIT,
            scopes::TASK_READ,
            scopes::TASK_CANCEL,
            scopes::ADMIN,
        ],
    ))
    .with_worker_allowlist([worker_id.to_string()])
}

fn request(worker_id: &str, key: &str) -> GatewaySubmitRequest {
    GatewaySubmitRequest {
        worker: Some(worker_id.into()),
        agent: "general".into(),
        session_id: "session".into(),
        input: "shared store input".into(),
        idempotency_key: key.into(),
        profile: None,
        team: None,
        team_version: None,
        limits: Limits {
            max_tokens: 10,
            ..Limits::default()
        },
    }
}

struct Setup {
    fixture: PgFixture,
    worker: FakeWorker,
    worker_id: String,
    env_name: String,
    _state: tempfile::TempDir,
}

impl Setup {
    async fn start(_lease_ttl_ms: i64) -> Option<Self> {
        let fixture = PgFixture::start()?;
        let worker = FakeWorker::start().await;
        let worker_id = format!("worker-{}", uuid::Uuid::new_v4().simple());
        let env_name = format!(
            "GB56_DSN_{}",
            uuid::Uuid::new_v4().simple().to_string().to_uppercase()
        );
        let state = tempfile::tempdir().unwrap();
        Some(Self {
            fixture,
            worker,
            worker_id,
            env_name,
            _state: state,
        })
    }

    async fn gateway(&self, replica: &str, lease_ttl_ms: i64) -> Gateway {
        let config = config(
            &self.fixture,
            &self.env_name,
            &self.worker,
            replica,
            lease_ttl_ms,
            &self.worker_id,
        );
        Gateway::prepare(config).await.expect("gateway prepares")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_same_key_dispatch_once_across_replicas() {
    let Some(setup) = Setup::start(60_000).await else {
        return;
    };
    let a = Arc::new(setup.gateway("a", 60_000).await);
    let b = Arc::new(setup.gateway("b", 60_000).await);
    let worker_id = setup.worker_id.clone();
    let caller_a = caller(&worker_id);
    let caller_b = caller(&worker_id);
    let request_a = request(&worker_id, "concurrent-key");
    let request_b = request_a.clone();
    let task_a = tokio::spawn(async move { a.submit(&caller_a, request_a).await });
    let task_b = tokio::spawn(async move { b.submit(&caller_b, request_b).await });
    let accepted_a = task_a.await.unwrap().expect("replica a accepts");
    let accepted_b = task_b.await.unwrap().expect("replica b accepts");
    assert_eq!(accepted_a.task_id, accepted_b.task_id);
    assert_eq!(setup.worker.posts(), 1, "only one remote submission");
    assert_eq!(setup.worker.state.tasks.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_restart_replays_without_dispatch() {
    let Some(setup) = Setup::start(60_000).await else {
        return;
    };
    let worker_id = setup.worker_id.clone();
    let caller = caller(&worker_id);
    let request = request(&worker_id, "restart-key");
    let accepted = {
        let a = setup.gateway("a", 60_000).await;
        a.submit(&caller, request.clone())
            .await
            .expect("first submit")
    };
    assert_eq!(setup.worker.posts(), 1);
    let b = setup.gateway("b", 60_000).await;
    let replayed = b.submit(&caller, request).await.expect("restart replay");
    assert_eq!(replayed.task_id, accepted.task_id);
    assert!(replayed.replayed);
    assert_eq!(setup.worker.posts(), 1, "replay does not re-dispatch");
    let inspected = b
        .task(&caller, &accepted.task_id)
        .await
        .expect("task visible");
    assert_eq!(inspected.task.state, TaskState::Succeeded);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lease_renewal_from_another_replica() {
    let Some(setup) = Setup::start(60_000).await else {
        return;
    };
    let a = setup.gateway("a", 60_000).await;
    let b = setup.gateway("b", 60_000).await;
    let request = WorkerRegistrationRequest {
        endpoint: setup.worker.base.clone(),
        incarnation: setup.worker.incarnation.clone(),
        capacity: Capacity::default(),
    };
    let first = a
        .register_worker(Some(CONTROL_TOKEN), &request)
        .await
        .expect("register through a");
    let renewal = WorkerLeaseRequest {
        incarnation: setup.worker.incarnation.clone(),
        capacity: Capacity::default(),
    };
    let renewed = b
        .renew_worker(Some(CONTROL_TOKEN), &setup.worker_id, &renewal)
        .await
        .expect("renew through b");
    assert!(renewed.lease_expires_at >= first.lease_expires_at);
    assert_eq!(renewed.incarnation, setup.worker.incarnation);

    let stale = WorkerLeaseRequest {
        incarnation: "old-incarnation".into(),
        capacity: Capacity::default(),
    };
    let error = b
        .renew_worker(Some(CONTROL_TOKEN), &setup.worker_id, &stale)
        .await
        .expect_err("mismatched incarnation is rejected");
    assert_eq!(error.code, "worker.incarnation_mismatch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_reattaches_from_another_replica_without_submitting() {
    let Some(setup) = Setup::start(60_000).await else {
        return;
    };
    let a = setup.gateway("a", 60_000).await;
    let b = setup.gateway("b", 60_000).await;
    let worker_id = setup.worker_id.clone();
    let caller = caller(&worker_id);
    let accepted = a
        .submit(&caller, request(&worker_id, "stream-key"))
        .await
        .expect("submit on a");
    assert_eq!(setup.worker.posts(), 1);
    let inspected = b
        .task(&caller, &accepted.task_id)
        .await
        .expect("task inspect on b");
    assert_eq!(inspected.task.id, "wtask-0");

    let mut stream = b
        .stream(&caller, &accepted.task_id, 7)
        .await
        .expect("stream on b");
    let chunk = tokio::time::timeout(
        Duration::from_secs(5),
        futures::StreamExt::next(&mut stream),
    )
    .await
    .expect("stream chunk before timeout")
    .expect("stream is not empty")
    .expect("stream chunk");
    assert!(String::from_utf8_lossy(&chunk).contains("task.updated"));
    let paths = setup.worker.state.stream_paths.lock().unwrap().clone();
    assert!(
        paths.iter().any(|path| path.contains("after=7")),
        "worker received the reattachment cursor: {paths:?}"
    );
    assert_eq!(setup.worker.posts(), 1, "reattachment never submits");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_acknowledgment_converges_on_one_worker_task() {
    let Some(setup) = Setup::start(60_000).await else {
        return;
    };
    let a = setup.gateway("a", 60_000).await;
    let b = setup.gateway("b", 60_000).await;
    let worker_id = setup.worker_id.clone();
    let caller = caller(&worker_id);
    setup.worker.state.lost_ack.store(true, Ordering::SeqCst);
    let error = a
        .submit(&caller, request(&worker_id, "lost-ack-key"))
        .await
        .expect_err("first attempt is ambiguous");
    assert_eq!(error.code, "task.unknown_submission");
    let execution_id = error
        .details
        .get("execution_id")
        .map(String::as_str)
        .expect("execution id")
        .to_string();
    let accepted = b
        .submit(&caller, request(&worker_id, "lost-ack-key"))
        .await
        .expect("second replica converges");
    assert_eq!(accepted.task_id, execution_id);
    assert_eq!(setup.worker.state.tasks.lock().unwrap().len(), 1);
    assert_eq!(setup.worker.posts(), 2, "one retry after the lost ack");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_change_propagates_immediately() {
    let Some(setup) = Setup::start(60_000).await else {
        return;
    };
    let a = setup.gateway("a", 60_000).await;
    let b = setup.gateway("b", 60_000).await;
    let worker_id = setup.worker_id.clone();
    let admin = caller(&worker_id);
    a.disable_worker(&admin, &worker_id)
        .expect("disable through a");
    let visible = b
        .discover(&admin, &common::WorkerDiscoveryFilter::default())
        .expect("discover through b");
    assert!(
        !visible.iter().any(|worker| worker.id == worker_id),
        "disable is immediately visible on b"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expiry_sweep_from_another_replica() {
    let Some(setup) = Setup::start(100).await else {
        return;
    };
    let a = setup.gateway("a", 100).await;
    let b = setup.gateway("b", 100).await;
    let request = WorkerRegistrationRequest {
        endpoint: setup.worker.base.clone(),
        incarnation: setup.worker.incarnation.clone(),
        capacity: Capacity::default(),
    };
    a.register_worker(Some(CONTROL_TOKEN), &request)
        .await
        .expect("register");
    tokio::time::sleep(Duration::from_millis(250)).await;
    b.health();
    let admin = caller(&setup.worker_id);
    let worker = b
        .admin_worker(&admin, &setup.worker_id)
        .expect("worker remains visible");
    assert_eq!(worker.status, "unreachable");
}
