//! Public SDK acceptance against real Gateway and Worker processes.
//!
//! The tests consume only `sdk::Client` and the published HTTP/SSE protocol.
//! They cover the separately compiled use-case app boundary, bounded retry
//! identity, artifact relay and verification, cancellation isolation, typed
//! recovery, and secret-safe errors.

mod support;

use common::{GatewaySubmitRequest, GatewayTask, Limits};
use sdk::{CancellationOutcome, ErrorKind};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{Stack, GATEWAY_TOKEN, OTHER_CALLER_TOKEN};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn request(input: &str, session: &str, key: &str) -> GatewaySubmitRequest {
    GatewaySubmitRequest {
        worker: Some("worker-a".into()),
        agent: "general".into(),
        session_id: session.into(),
        input: input.into(),
        idempotency_key: key.into(),
        profile: None,
        team: None,
        team_version: None,
        limits: Limits::default(),
    }
}

fn wait_task(runtime: &tokio::runtime::Runtime, client: &sdk::Client, id: &str) -> GatewayTask {
    let started = Instant::now();
    loop {
        match runtime.block_on(client.task(id)) {
            Ok(task) => {
                if task.task.state.is_terminal() {
                    return task;
                }
            }
            Err(error) => panic!("task {id} failed: {error:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "task {id} did not reach a terminal state"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn sdk_use_case_journey_inspects_resumes_fetches_and_cancels() {
    let stack = Stack::start();
    let runtime = runtime();
    let client = sdk::Client::new(&stack.gateway_base, GATEWAY_TOKEN);

    let first = runtime
        .block_on(client.submit(&request(
            "artifact:sdk public evidence",
            "sdk-journey",
            "sdk-journey-artifact",
        )))
        .expect("submit is accepted");
    assert!(!first.replayed);
    assert_eq!(first.worker, "worker-a");

    // The stable external handle is returned for the same caller identity.
    let replay = runtime
        .block_on(client.submit(&request(
            "artifact:sdk public evidence",
            "sdk-journey",
            "sdk-journey-artifact",
        )))
        .expect("the same request reuses its handle");
    assert_eq!(replay.task_id, first.task_id);
    assert!(replay.replayed);

    let task = wait_task(&runtime, &client, &first.task_id);
    assert_eq!(task.task_id, first.task_id);
    assert_eq!(task.task.state, common::TaskState::Succeeded);

    // Retained events paginate through the published cursor and never exceed
    // the requested page size.
    let page = runtime
        .block_on(client.events(&first.task_id, 0, 1))
        .expect("first event page");
    assert!(page.events.len() <= 1);
    assert!(!page.cursor_expired);
    let first_seq = page.events.first().map(|event| event.seq);
    let next = runtime
        .block_on(client.events(&first.task_id, page.next_seq, 1))
        .expect("next event page");
    assert!(next.events.len() <= 1);
    assert_ne!(
        next.events.first().map(|event| event.seq),
        first_seq,
        "pagination returned the same retained event"
    );

    let kinds = runtime.block_on(async {
        let mut watch = client.watch(&first.task_id, 0).await.expect("watch opens");
        let mut kinds = Vec::new();
        while let Some(event) = watch.next().await.expect("stream event") {
            kinds.push(event.kind);
        }
        kinds
    });
    assert!(
        kinds.iter().any(|kind| kind == "task.succeeded"),
        "{kinds:?}"
    );

    let artifacts = runtime
        .block_on(client.artifacts(&first.task_id))
        .expect("artifact list");
    let artifact = artifacts
        .iter()
        .find(|artifact| artifact.name == "evidence.txt")
        .expect("native invocation finalized evidence");
    let download = runtime
        .block_on(client.download_artifact(&first.task_id, &artifact.id))
        .expect("artifact download");
    assert_eq!(download.bytes, b"sdk public evidence");
    assert_eq!(download.artifact.digest, artifact.digest);
    assert_eq!(download.artifact.size, download.bytes.len() as u64);

    // A foreign caller cannot fetch another caller's artifact through the same
    // public route.
    let foreign = sdk::Client::new(&stack.gateway_base, OTHER_CALLER_TOKEN);
    let denied = runtime
        .block_on(foreign.download_artifact(&first.task_id, &artifact.id))
        .expect_err("foreign artifact access is denied");
    assert_eq!(denied.code, "route.task.not_found");
    assert_eq!(sdk::classify(&denied), ErrorKind::NotFound);

    // Cancel a different authorized task without touching the completed one.
    let second = runtime
        .block_on(client.submit(&request(
            "delay:2000 sdk cancel me",
            "sdk-cancel",
            "sdk-journey-cancel",
        )))
        .expect("second submit");
    let cancelled = runtime
        .block_on(client.cancel(&second.task_id))
        .expect("cancel is accepted");
    assert!(matches!(
        sdk::cancellation_outcome(&cancelled),
        CancellationOutcome::Confirmed | CancellationOutcome::Pending
    ));
    let second = wait_task(&runtime, &client, &second.task_id);
    assert_eq!(second.task.state, common::TaskState::Cancelled);
    assert_eq!(stack.worker_a.tasks().len(), 2, "retry created a new task");
}

#[test]
fn sdk_connection_cancellation_does_not_cancel_server_execution() {
    let stack = Stack::start();
    let runtime = runtime();
    let client = sdk::Client::new(&stack.gateway_base, GATEWAY_TOKEN);
    let submitted = runtime
        .block_on(client.submit(&request(
            "delay:900 connection drop",
            "sdk-drop",
            "sdk-drop-key",
        )))
        .expect("submit");
    let task_id = submitted.task_id;

    runtime.block_on(async {
        let mut watch = client.watch(&task_id, 0).await.expect("watch opens");
        loop {
            let event = watch.next().await.expect("event").expect("stream event");
            if event.kind == "task.running" {
                break;
            }
        }
        // Dropping Watch closes the HTTP connection and releases the Gateway
        // permit. It must not send an implicit cancel to the Worker.
        drop(watch);
    });

    let task = wait_task(&runtime, &client, &task_id);
    assert_eq!(task.task.state, common::TaskState::Succeeded);
    let events = runtime
        .block_on(client.events(&task_id, 0, 1000))
        .expect("retained events");
    assert!(
        events
            .events
            .iter()
            .all(|event| event.kind != "task.cancelled"),
        "dropping a viewer cancelled server execution"
    );
}

/// A loopback proxy that drops the first accepted response after forwarding.
///
/// This reproduces a lost acknowledgment after the Gateway may have accepted
/// the request, without killing the Gateway or Worker processes.
struct LossyProxy {
    base: String,
    connections: Arc<AtomicUsize>,
    _listener: std::thread::JoinHandle<()>,
}

impl LossyProxy {
    fn start(target: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("proxy listener");
        let base = format!("http://{}", listener.local_addr().expect("proxy address"));
        let connections = Arc::new(AtomicUsize::new(0));
        let counter = connections.clone();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let target = target.clone();
                let first = counter.fetch_add(1, Ordering::SeqCst) == 0;
                std::thread::spawn(move || {
                    let _ = relay(stream, &target, first);
                });
            }
        });
        Self {
            base,
            connections,
            _listener: handle,
        }
    }
}

impl Drop for LossyProxy {
    fn drop(&mut self) {
        // Connecting to the listener wakes the accept loop so the test process
        // does not retain a background accept thread.
        let address = self.base.trim_start_matches("http://");
        let _ = TcpStream::connect(address);
    }
}

fn relay(mut client: TcpStream, target: &str, drop_response: bool) -> std::io::Result<()> {
    let request = read_request(&mut client)?;
    let mut upstream = TcpStream::connect(target.trim_start_matches("http://"))?;
    upstream.write_all(&request)?;
    upstream.flush()?;
    if drop_response {
        // The Gateway may still commit acceptance; the client sees a lost
        // acknowledgment and must retry the same request identity.
        return Ok(());
    }
    client.set_read_timeout(Some(Duration::from_secs(15)))?;
    upstream.set_read_timeout(Some(Duration::from_secs(15)))?;
    std::io::copy(&mut upstream, &mut client)?;
    Ok(())
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "request ended before headers",
            ));
        }
        request.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_headers_end(&request) {
            break index;
        }
    };
    let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let expected = header_end + content_length;
    while request.len() < expected {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
    }
    // Make the upstream close after its response so a simple copy terminates.
    let insertion = header_end - 2;
    request.splice(
        insertion..insertion,
        b"Connection: close\r\n".iter().copied(),
    );
    Ok(request)
}

fn find_headers_end(request: &[u8]) -> Option<usize> {
    request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

#[test]
fn sdk_retry_after_lost_acknowledgment_reuses_the_same_task_identity() {
    let stack = Stack::start();
    let proxy = LossyProxy::start(stack.gateway_base.clone());
    let runtime = runtime();
    let client = sdk::Client::new(&proxy.base, GATEWAY_TOKEN);

    let response = runtime
        .block_on(client.submit(&request("lost acknowledgment", "sdk-loss", "sdk-loss-key")))
        .expect("bounded retry resolves the original task");
    assert!(
        proxy.connections.load(Ordering::SeqCst) >= 2,
        "the proxy did not force a retry"
    );

    // A direct client observes the same stable handle after retry.
    let direct = sdk::Client::new(&stack.gateway_base, GATEWAY_TOKEN);
    let replay = runtime
        .block_on(direct.submit(&request("lost acknowledgment", "sdk-loss", "sdk-loss-key")))
        .expect("same request identity replays");
    assert_eq!(replay.task_id, response.task_id);
    assert!(replay.replayed);
    assert_eq!(stack.worker_a.tasks().len(), 1, "retry created a new task");

    let task = wait_task(&runtime, &direct, &response.task_id);
    assert_eq!(task.task.state, common::TaskState::Succeeded);
}

#[test]
fn sdk_cursor_expiry_rate_limit_and_protocol_errors_stay_typed() {
    // Retention gap forwards a typed cursor error unchanged.
    let trimmed = Stack::start_with_worker_env(&[("GOLDENBEE_EVENT_RETENTION", "2".into())]);
    let runtime = runtime();
    let client = sdk::Client::new(&trimmed.gateway_base, GATEWAY_TOKEN);
    let submitted = runtime
        .block_on(client.submit(&request("trimmed sdk", "sdk-trim", "sdk-trim-key")))
        .expect("submit");
    wait_task(&runtime, &client, &submitted.task_id);
    let expired = match runtime.block_on(client.watch(&submitted.task_id, 0)) {
        Ok(_) => panic!("cursor falls outside retention"),
        Err(error) => error,
    };
    assert_eq!(expired.code, "stream.cursor_expired");
    assert_eq!(sdk::classify(&expired), ErrorKind::CursorExpired);

    // Bounded admission advertises a retry interval instead of a raw provider
    // exception.
    let limited = Stack::start_with_worker_env(&[
        ("GOLDENBEE_MAX_ACTIVE", "1".into()),
        ("GOLDENBEE_MAX_QUEUE", "0".into()),
        ("GOLDENBEE_PER_PRINCIPAL", "4".into()),
    ]);
    let client = sdk::Client::new(&limited.gateway_base, GATEWAY_TOKEN);
    let held = runtime
        .block_on(client.submit(&request("delay:800 hold", "sdk-rate", "sdk-rate-1")))
        .expect("first submit");
    let started = Instant::now();
    let rejected = loop {
        match runtime.block_on(client.submit(&request(
            "delay:50 excess",
            "sdk-rate-2",
            "sdk-rate-2",
        ))) {
            Ok(_) => {
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "excess work was never rejected"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => break error,
        }
    };
    assert_eq!(rejected.code, "capacity.queue_full");
    assert_eq!(sdk::classify(&rejected), ErrorKind::RateLimited);
    assert_eq!(
        sdk::retry_after(&rejected),
        Some(Duration::from_millis(250))
    );
    wait_task(&runtime, &client, &held.task_id);

    // A peer that does not implement the versioned envelope is a protocol
    // error, not an untyped provider exception.
    let incompatible = incompatible_gateway();
    let client = sdk::Client::new(incompatible, "token");
    let protocol = runtime
        .block_on(client.task("exec-incompatible"))
        .expect_err("incompatible payload is rejected");
    assert_eq!(protocol.code, "sdk.invalid_response");
    assert_eq!(sdk::classify(&protocol), ErrorKind::Protocol);
}

#[test]
fn sdk_authentication_and_transport_errors_are_secret_safe() {
    let stack = Stack::start();
    let runtime = runtime();
    let canary = "sk-sdk-secret-canary";
    let client = sdk::Client::new(&stack.gateway_base, canary);
    let error = runtime
        .block_on(client.submit(&request("secret safe", "sdk-secret", "sdk-secret")))
        .expect_err("unknown token is rejected");
    assert_eq!(error.code, "auth.unauthenticated");
    assert_eq!(sdk::classify(&error), ErrorKind::Authentication);
    assert!(!format!("{error:?}").contains(canary));

    // Stop the Gateway: transport failure is typed and retryable, and no token
    // is included in the error.
    let mut stack = stack;
    stack.kill_gateway();
    let error = runtime
        .block_on(client.health())
        .expect_err("Gateway is down");
    assert_eq!(error.code, "sdk.transport");
    assert_eq!(sdk::classify(&error), ErrorKind::Transport);
    assert!(!format!("{error:?}").contains(canary));
}

fn incompatible_gateway() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("incompatible listener");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    std::thread::spawn(move || {
        for stream in listener.incoming().take(1) {
            let Ok(mut stream) = stream else { continue };
            let _ = read_request(&mut stream);
            let body = br#"{"not":"a gateway task"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    base
}

#[test]
fn sdk_client_app_uses_the_public_library_for_all_journey_steps() {
    let stack = Stack::start();
    let runtime = runtime();
    let client = sdk::Client::new(&stack.gateway_base, GATEWAY_TOKEN);

    // A second authorized task exists for the application to cancel.
    let cancel = runtime
        .block_on(client.submit(&request(
            "delay:2500 app cancel",
            "sdk-app-cancel",
            "sdk-app-cancel",
        )))
        .expect("cancel target submit");

    let output = std::process::Command::new(binary("sdk-client"))
        .args([
            "--gateway",
            &stack.gateway_base,
            "--token",
            GATEWAY_TOKEN,
            "--worker",
            "worker-a",
            "--agent",
            "general",
            "--input",
            "artifact:sdk-client evidence",
            "--session",
            "sdk-client-session",
            "--key",
            "sdk-client-journey",
            "--cancel-task",
            &cancel.task_id,
        ])
        .output()
        .expect("sdk-client runs");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("sdk-client prints JSON summary");
    assert_eq!(value["state"], "succeeded");
    assert_eq!(value["artifact"]["verified"], true);
    assert_eq!(value["artifact"]["text"], "sdk-client evidence");
    assert_eq!(value["cancellation"]["task_id"], cancel.task_id.as_str());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(GATEWAY_TOKEN));
}

fn binary(name: &str) -> std::path::PathBuf {
    if let Ok(path) = std::env::var(format!("CARGO_BIN_EXE_{name}")) {
        return std::path::PathBuf::from(path);
    }
    let current = std::env::current_exe().expect("test executable path");
    let mut directory = current.parent();
    while let Some(candidate_directory) = directory {
        let candidate = candidate_directory.join(name);
        if candidate.is_file() {
            return candidate;
        }
        directory = candidate_directory.parent();
    }
    panic!("{name} is missing; build the workspace example binaries before running this test");
}
