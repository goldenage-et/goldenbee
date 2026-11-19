//! Public duplex socket acceptance against real Gateway and Worker processes.
//!
//! The tests use the reusable SDK adapter for typed commands and a raw client
//! only to prove handshake, frame, and backpressure rejection. All task,
//! stream, cancellation, and approval outcomes reuse the published HTTP
//! semantics; the socket never owns a second journal.

mod support;

use common::{
    ApiError, GatewaySubmitRequest, GatewayTask, Limits, SocketCommand, SocketCommandFrame,
    SocketOutcome, SocketScope, SocketServerFrame, TaskState,
};
use sdk::{Client, Socket};
use serde_json::{json, Value};
use support::{binary, Options, Stack, GATEWAY_TOKEN, OTHER_CALLER_TOKEN};
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

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

fn submit_frame(id: &str, request: &GatewaySubmitRequest) -> SocketCommandFrame {
    SocketCommandFrame::new(
        id,
        SocketScope {
            worker: request.worker.clone(),
            task_id: None,
        },
        SocketCommand::Submit {
            request: request.clone(),
        },
    )
}

fn subscribe_frame(id: &str, task_id: &str, after: u64) -> SocketCommandFrame {
    SocketCommandFrame::new(
        id,
        SocketScope {
            worker: None,
            task_id: Some(task_id.into()),
        },
        SocketCommand::Subscribe {
            task_id: task_id.into(),
            after,
        },
    )
}

fn wait_task(runtime: &tokio::runtime::Runtime, client: &Client, id: &str) -> GatewayTask {
    let started = std::time::Instant::now();
    loop {
        match runtime.block_on(client.task(id)) {
            Ok(task) if task.task.state.is_terminal() => return task,
            Ok(_) => {}
            Err(error) => panic!("task {id} failed: {error:?}"),
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(25),
            "task {id} did not reach a terminal state"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn ws_url(base: &str, ticket: &str) -> String {
    let authority = base.trim_start_matches("http://");
    format!("ws://{authority}/v1/socket?ticket={ticket}")
}

async fn raw_connect(
    base: &str,
    token: &str,
) -> WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>> {
    let socket = Socket::new(base, token);
    let ticket = socket.ticket().await.expect("ticket").ticket;
    connect_async(ws_url(base, &ticket))
        .await
        .expect("raw socket upgrade")
        .0
}

async fn next_server_frame(
    stream: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
) -> SocketServerFrame {
    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(10), stream.next())
            .await
            .expect("socket frame deadline")
            .expect("socket remains open")
            .expect("socket frame");
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).expect("server frame");
        }
    }
}

use futures::{SinkExt, StreamExt};

fn error_code(error: &ApiError) -> &str {
    &error.code
}

#[test]
fn socket_ticket_is_short_lived_one_use_and_never_leaks_the_bearer() {
    let stack = Stack::start_with_options(Options {
        socket: Some(json!({ "ticket_ttl_ms": 250 })),
        ..Options::default()
    });
    let runtime = runtime();
    let socket = Socket::new(&stack.gateway_base, GATEWAY_TOKEN);

    let ticket = runtime.block_on(socket.ticket()).expect("ticket minted");
    assert_ne!(ticket.ticket, GATEWAY_TOKEN);
    assert_eq!(ticket.path, "/v1/socket");
    assert!(!ticket.ticket.contains(GATEWAY_TOKEN));
    let url = ws_url(&stack.gateway_base, &ticket.ticket);
    assert!(
        !url.contains(GATEWAY_TOKEN),
        "bearer leaked into URL: {url}"
    );

    // One use: redeem the exact same ticket twice.
    let one_use = runtime.block_on(socket.ticket()).expect("one-use ticket");
    let url = ws_url(&stack.gateway_base, &one_use.ticket);
    let first = runtime
        .block_on(connect_async(url.clone()))
        .expect("first redemption succeeds")
        .0;
    drop(first);
    let reused = runtime
        .block_on(connect_async(url))
        .expect_err("a ticket cannot be reused");
    let reused = match reused {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            serde_json::from_slice::<ApiError>(response.body().as_deref().expect("typed body"))
                .expect("typed error")
        }
        other => panic!("unexpected reuse error: {other}"),
    };
    assert_eq!(error_code(&reused), "auth.socket_ticket");

    // Short lived.
    let expiring = runtime.block_on(socket.ticket()).expect("fresh ticket");
    std::thread::sleep(std::time::Duration::from_millis(350));
    let expired = runtime
        .block_on(connect_async(ws_url(&stack.gateway_base, &expiring.ticket)))
        .expect_err("expired ticket is rejected");
    let expired_error = match expired {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            serde_json::from_slice::<ApiError>(response.body().as_deref().expect("typed body"))
                .expect("typed error")
        }
        other => panic!("unexpected handshake error: {other}"),
    };
    assert_eq!(error_code(&expired_error), "auth.socket_ticket");
    assert!(!format!("{expired_error:?}").contains(GATEWAY_TOKEN));

    // No credential at all.
    let unauthorized = runtime
        .block_on(connect_async(format!(
            "ws://{}/v1/socket",
            stack.gateway_base.trim_start_matches("http://")
        )))
        .expect_err("missing ticket is rejected");
    match unauthorized {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 401);
        }
        other => panic!("unexpected unauthenticated error: {other}"),
    }
}

#[test]
fn socket_submit_matches_http_and_reconnect_never_duplicates_the_prompt() {
    let stack = Stack::start();
    let runtime = runtime();
    let client = Client::new(&stack.gateway_base, GATEWAY_TOKEN);
    let socket = Socket::new(&stack.gateway_base, GATEWAY_TOKEN);
    let input = request("socket duplicate guard", "socket-dup", "socket-dup-key");

    let mut session = runtime.block_on(socket.connect()).expect("connect");
    let outcome = runtime
        .block_on(session.command(&submit_frame("submit-1", &input)))
        .expect("submit acknowledged");
    let SocketOutcome::Submitted { response } = outcome else {
        panic!("unexpected submit outcome: {outcome:?}");
    };
    assert_eq!(response.worker, "worker-a");
    assert!(!response.replayed);

    // Same outcome as the canonical HTTP projection.
    let http = runtime
        .block_on(client.task(&response.task_id))
        .expect("http task");
    assert_eq!(http.task_id, response.task_id);
    assert_eq!(http.worker, response.worker);
    assert_eq!(http.task.agent, response.agent);
    drop(session);

    // Repeating the acknowledged mutation after reconnect replays the task.
    let mut again = runtime.block_on(socket.connect()).expect("reconnect");
    let replay = runtime
        .block_on(again.command(&submit_frame("submit-1", &input)))
        .expect("replay acknowledged");
    let SocketOutcome::Submitted { response: replay } = replay else {
        panic!("unexpected replay outcome");
    };
    assert_eq!(replay.task_id, response.task_id);
    assert!(replay.replayed);

    // A dropped acknowledgment is safe too: send, close before reading, then
    // repeat the same idempotent command on a fresh connection.
    let dropped = request("socket dropped ack", "socket-drop", "socket-drop-key");
    let mut drop_session = runtime.block_on(socket.connect()).expect("connect");
    runtime
        .block_on(drop_session.send(&submit_frame("submit-drop", &dropped)))
        .expect("frame sent");
    drop(drop_session);
    let mut recovered = runtime.block_on(socket.connect()).expect("reconnect");
    let recovered = runtime
        .block_on(recovered.command(&submit_frame("submit-drop", &dropped)))
        .expect("repeated command");
    let SocketOutcome::Submitted {
        response: recovered,
    } = recovered
    else {
        panic!("unexpected recovery outcome");
    };
    assert!(recovered.replayed);

    wait_task(&runtime, &client, &response.task_id);
    wait_task(&runtime, &client, &recovered.task_id);
    assert_eq!(
        stack.worker_a.tasks().len(),
        2,
        "a reconnect or dropped acknowledgment created a duplicate task"
    );
}

#[test]
fn socket_subscription_resumes_durable_cursor_and_http_stays_canonical() {
    let stack = Stack::start();
    let runtime = runtime();
    let client = Client::new(&stack.gateway_base, GATEWAY_TOKEN);
    let socket = Socket::new(&stack.gateway_base, GATEWAY_TOKEN);
    let submitted = runtime
        .block_on(client.submit(&request(
            "delay:700 socket cursor",
            "socket-cursor",
            "socket-cursor-key",
        )))
        .expect("submit");
    let task_id = submitted.task_id;

    let mut first = runtime.block_on(socket.connect()).expect("connect");
    let ack = runtime
        .block_on(first.command(&subscribe_frame("sub-1", &task_id, 0)))
        .expect("subscribe ack");
    assert!(matches!(ack, SocketOutcome::Subscribed { .. }));
    let mut cursor = 0;
    loop {
        let event = runtime.block_on(first.next_event()).expect("event");
        cursor = cursor.max(event.seq);
        if event.kind == "task.running" {
            break;
        }
    }
    drop(first);

    // The canonical HTTP projection remains available while the socket is
    // gone, and the Worker still owns the one task.
    let canonical = runtime.block_on(client.task(&task_id)).expect("http task");
    assert_eq!(canonical.task_id, task_id);

    let mut second = runtime.block_on(socket.connect()).expect("reconnect");
    runtime
        .block_on(second.command(&subscribe_frame("sub-2", &task_id, cursor)))
        .expect("cursor subscription ack");
    loop {
        let event = runtime
            .block_on(second.next_event())
            .expect("resumed event");
        assert!(event.seq > cursor, "replayed an already consumed event");
        if event.kind == "task.succeeded" {
            break;
        }
    }
    let task = wait_task(&runtime, &client, &task_id);
    assert_eq!(task.task.state, TaskState::Succeeded);
    assert_eq!(
        stack.worker_a.tasks().len(),
        1,
        "resume submitted a new task"
    );

    // Expired retained history produces resynchronization guidance while the
    // canonical HTTP status remains available.
    let trimmed = Stack::start_with_worker_env(&[("GOLDENBEE_EVENT_RETENTION", "2".into())]);
    let client = Client::new(&trimmed.gateway_base, GATEWAY_TOKEN);
    let socket = Socket::new(&trimmed.gateway_base, GATEWAY_TOKEN);
    let submitted = runtime
        .block_on(client.submit(&request("trimmed socket", "socket-trim", "socket-trim-key")))
        .expect("submit");
    wait_task(&runtime, &client, &submitted.task_id);
    let mut session = runtime.block_on(socket.connect()).expect("connect");
    let expired = runtime
        .block_on(session.command(&subscribe_frame("sub-expired", &submitted.task_id, 0)))
        .expect_err("cursor outside retention");
    assert_eq!(expired.code, "stream.cursor_expired");
    assert!(!expired.recovery.is_empty());
    let canonical = runtime
        .block_on(client.task(&submitted.task_id))
        .expect("canonical status");
    assert_eq!(canonical.task.state, TaskState::Succeeded);
}

#[test]
fn socket_cancel_and_foreign_scope_match_http_semantics() {
    let stack = Stack::start();
    let runtime = runtime();
    let client = Client::new(&stack.gateway_base, GATEWAY_TOKEN);
    let socket = Socket::new(&stack.gateway_base, GATEWAY_TOKEN);
    let submitted = runtime
        .block_on(client.submit(&request(
            "delay:2000 socket cancel",
            "socket-cancel",
            "socket-cancel-key",
        )))
        .expect("submit");

    let mut session = runtime.block_on(socket.connect()).expect("connect");
    let frame = SocketCommandFrame::new(
        "cancel-1",
        SocketScope {
            worker: None,
            task_id: Some(submitted.task_id.clone()),
        },
        SocketCommand::Cancel {
            task_id: submitted.task_id.clone(),
        },
    );
    let outcome = runtime
        .block_on(session.command(&frame))
        .expect("cancel ack");
    let SocketOutcome::Cancelled { task } = outcome else {
        panic!("unexpected cancel outcome: {outcome:?}");
    };
    assert!(matches!(
        task.task.state,
        TaskState::Cancelling | TaskState::Cancelled
    ));
    let http = runtime
        .block_on(client.task(&submitted.task_id))
        .expect("http task");
    assert_eq!(task.task_id, http.task_id);
    let terminal = wait_task(&runtime, &client, &submitted.task_id);
    assert_eq!(terminal.task.state, TaskState::Cancelled);
    assert_eq!(http.task_id, terminal.task_id);

    // A foreign task is not visible through the socket either.
    let foreign: Value = stack
        .submit(
            OTHER_CALLER_TOKEN,
            "worker-b",
            "general",
            "foreign socket task",
            "foreign",
            "foreign-socket",
        )
        .json()
        .expect("foreign submit");
    let foreign_id = foreign["task_id"].as_str().expect("foreign id");
    let denied = runtime
        .block_on(session.command(&subscribe_frame("sub-foreign", foreign_id, 0)))
        .expect_err("foreign task rejected");
    assert_eq!(denied.code, "route.task.not_found");

    // A frame that lies about its task scope is rejected before dispatch.
    let wrong = SocketCommandFrame::new(
        "scope-wrong",
        SocketScope {
            worker: None,
            task_id: Some(submitted.task_id.clone()),
        },
        SocketCommand::Cancel {
            task_id: foreign_id.into(),
        },
    );
    let mismatch = runtime
        .block_on(session.command(&wrong))
        .expect_err("scope mismatch");
    assert_eq!(mismatch.code, "socket.scope_mismatch");
}

#[test]
fn socket_rejects_invalid_oversized_and_binary_frames() {
    let stack = Stack::start();
    let runtime = runtime();
    runtime.block_on(async {
        let mut stream = raw_connect(&stack.gateway_base, GATEWAY_TOKEN).await;

        stream
            .send(Message::Text("not json".into()))
            .await
            .expect("send invalid");
        let error = next_server_frame(&mut stream).await;
        assert!(matches!(
            error,
            SocketServerFrame::Error { error, .. } if error.code == "socket.invalid_frame"
        ));

        // The connection remains usable after one invalid frame.
        stream
            .send(Message::Text(
                serde_json::to_string(&subscribe_frame("sub-raw", "exec-missing", 0))
                    .expect("frame")
                    .into(),
            ))
            .await
            .expect("send command");
        let error = next_server_frame(&mut stream).await;
        assert!(matches!(
            error,
            SocketServerFrame::Error { error, .. } if error.code == "route.task.not_found"
        ));

        stream
            .send(Message::Binary(vec![1, 2, 3].into()))
            .await
            .expect("send binary");
        let error = next_server_frame(&mut stream).await;
        assert!(matches!(
            error,
            SocketServerFrame::Error { error, .. } if error.code == "socket.invalid_frame"
        ));

        // Oversized text is rejected at the published bound and closes.
        let mut stream = raw_connect(&stack.gateway_base, GATEWAY_TOKEN).await;
        let oversized = "x".repeat(common::SOCKET_MAX_FRAME_BYTES + 1024);
        stream
            .send(Message::Text(oversized.into()))
            .await
            .expect("send oversized");
        let error = next_server_frame(&mut stream).await;
        assert!(matches!(
            error,
            SocketServerFrame::Error { error, .. } if error.code == "socket.frame_too_large"
        ));
        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("close deadline");
        assert!(
            closed.is_none() || matches!(closed, Some(Ok(Message::Close(_))) | Some(Err(_))),
            "oversized connection stayed open"
        );
    });
}

#[test]
fn socket_is_idle_bounded_and_secret_safe() {
    let stack = Stack::start_with_options(Options {
        socket: Some(json!({
            "heartbeat_ms": 100,
            "idle_timeout_ms": 250,
            "send_timeout_ms": 50
        })),
        ..Options::default()
    });
    let runtime = runtime();

    runtime.block_on(async {
        let mut stream = raw_connect(&stack.gateway_base, GATEWAY_TOKEN).await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let error = loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let message = tokio::time::timeout(remaining, stream.next())
                .await
                .expect("idle close deadline");
            let Some(message) = message else {
                panic!("socket closed without a typed idle error");
            };
            let Ok(message) = message else {
                panic!("socket failed without a typed idle error");
            };
            if let Message::Text(text) = message {
                let frame: SocketServerFrame = serde_json::from_str(text.as_str()).expect("frame");
                if let SocketServerFrame::Error { error, .. } = frame {
                    break error;
                }
            }
        };
        assert_eq!(error.code, "socket.idle_timeout");
    });

    // A secret canary in task input is not echoed by socket events.
    let stack = Stack::start();
    let client = Client::new(&stack.gateway_base, GATEWAY_TOKEN);
    let socket = Socket::new(&stack.gateway_base, GATEWAY_TOKEN);
    let canary = "sk-socket-secret-canary";
    let submitted = runtime
        .block_on(client.submit(&request(
            &format!("delay:100 echo {canary}"),
            "socket-secret",
            "socket-secret-key",
        )))
        .expect("submit");
    let mut session = runtime.block_on(socket.connect()).expect("connect");
    runtime
        .block_on(session.command(&subscribe_frame("sub-secret", &submitted.task_id, 0)))
        .expect("subscribe");
    let mut encoded = String::new();
    loop {
        let event = runtime.block_on(session.next_event()).expect("event");
        encoded.push_str(&serde_json::to_string(&event).expect("event JSON"));
        if event.kind == "task.succeeded" {
            break;
        }
    }
    assert!(
        !encoded.contains(canary),
        "secret leaked through socket events: {encoded}"
    );
    assert!(!encoded.contains(GATEWAY_TOKEN));
}

#[test]
fn socket_bounds_backpressure_for_a_slow_consumer() {
    let stack = Stack::start_with_options(Options {
        socket: Some(json!({
            "subscriber_queue": 1,
            "max_subscriptions": 8,
            "max_frame_bytes": 1048576,
            "send_timeout_ms": 20,
            "heartbeat_ms": 100,
            "idle_timeout_ms": 1000
        })),
        ..Options::default()
    });
    let runtime = runtime();
    let task_id = {
        let response = stack.submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            "delay:1500 socket backpressure",
            "slow",
            "slow",
        );
        let status = response.status();
        let text = response.text().expect("submit body");
        assert!(status.is_success(), "submit failed: {status} {text}");
        let body: Value = serde_json::from_str(&text).expect("submit JSON");
        body["task_id"]
            .as_str()
            .unwrap_or_else(|| panic!("submit body: {text}"))
            .to_string()
    };

    runtime.block_on(async {
        let mut stream = raw_connect(&stack.gateway_base, GATEWAY_TOKEN).await;
        // Make the kernel receive window deliberately small so the test can
        // deterministically reach the Gateway's bounded subscriber queue
        // without depending on host socket autotuning.
        if let MaybeTlsStream::Plain(tcp) = stream.get_ref() {
            socket2::SockRef::from(tcp)
                .set_recv_buffer_size(4096)
                .expect("small receive buffer");
        }
        for index in 0..8 {
            let frame = subscribe_frame(&format!("slow-sub-{index}"), &task_id, 0);
            stream
                .send(Message::Text(
                    serde_json::to_string(&frame).expect("frame").into(),
                ))
                .await
                .expect("subscribe sent");
        }
        // Do not read while multiple subscriptions replay and stream events.
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        // A slow consumer is bounded: the Gateway either cannot drain its
        // subscriber queue or closes the silent connection at the idle bound.
        // It never buffers without limit.
        let _ = stream.send(Message::Ping(Vec::new().into())).await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let message = tokio::time::timeout(remaining, stream.next())
                .await
                .expect("slow consumer close deadline");
            match message {
                None => break,
                Some(Err(_)) => break,
                Some(Ok(Message::Close(_))) => break,
                Some(Ok(Message::Text(text))) => {
                    let frame: SocketServerFrame =
                        serde_json::from_str(text.as_str()).expect("frame");
                    if let SocketServerFrame::Error { error, .. } = frame {
                        assert!(
                            matches!(
                                error.code.as_str(),
                                "capacity.socket_slow_consumer" | "socket.idle_timeout"
                            ),
                            "unexpected bound: {error:?}"
                        );
                        break;
                    }
                }
                Some(Ok(_)) => {}
            }
        }
    });
}

#[test]
fn socket_client_fixture_consumes_the_sdk_adapter_without_raw_transport() {
    let stack = Stack::start();
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
            "artifact:socket fixture evidence",
            "--session",
            "socket-fixture",
            "--key",
            "socket-fixture-key",
            "--socket",
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
    assert_eq!(value["transport"], "socket");
    assert_eq!(value["state"], "succeeded");
    assert_eq!(value["artifact"]["verified"], true);
    assert!(value["streamed_events"].as_u64().unwrap_or_default() > 0);
    assert!(!String::from_utf8_lossy(&output.stdout).contains(GATEWAY_TOKEN));
    assert_eq!(stack.worker_a.tasks().len(), 1);
}
