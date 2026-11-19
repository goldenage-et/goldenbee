//! Client construction, error decoding, and SSE block parsing.

use super::retry::{RetryPolicy, MAX_ATTEMPTS, MAX_DELAY};
use super::transport::decode_bytes;
use super::watch::{parse_block, take_block, StreamCursor};
use super::Client;
use crate::{
    cancellation_outcome, classify, retry_after, CancellationOutcome, ErrorKind, MAX_STREAM_BUFFER,
};
use common::{
    ApiError, Artifact, GatewaySubmitRequest, GatewayTask, Limits, StreamEvent, Task, TaskState,
};
use reqwest::StatusCode;
use std::time::Duration;

#[test]
fn construction_trims_trailing_separator() {
    let client = Client::new("http://gateway.invalid/", "token");
    assert_eq!(client.base, "http://gateway.invalid");
    assert_eq!(client.token, "token");
}

#[test]
fn error_status_decodes_typed_error() {
    let body = serde_json::to_vec(&ApiError::new("worker.disabled", "worker is disabled")).unwrap();
    let error = decode_bytes::<serde_json::Value>(StatusCode::CONFLICT, &body).unwrap_err();
    assert_eq!(error.code, "worker.disabled");
    assert!(!error.retryable);
}

#[test]
fn untyped_error_status_is_invalid_response() {
    let error =
        decode_bytes::<serde_json::Value>(StatusCode::BAD_GATEWAY, b"upstream").unwrap_err();
    assert_eq!(error.code, "sdk.invalid_response");
}

#[test]
fn success_family_typed_error_stays_uncertain() {
    let body =
        serde_json::to_vec(&ApiError::new("task.unknown_submission", "outcome unknown")).unwrap();
    let error = decode_bytes::<serde_json::Value>(StatusCode::ACCEPTED, &body).unwrap_err();
    assert_eq!(error.code, "task.unknown_submission");
}

#[test]
fn malformed_success_is_invalid_response() {
    let error = decode_bytes::<serde_json::Value>(StatusCode::OK, b"not json").unwrap_err();
    assert_eq!(error.code, "sdk.invalid_response");
}

#[test]
fn sse_blocks_split_on_lf_and_crlf() {
    let mut buffer = b"data: one\n\ndata: two\r\n\r\nleftover".to_vec();
    assert_eq!(take_block(&mut buffer).unwrap(), b"data: one".to_vec());
    assert_eq!(take_block(&mut buffer).unwrap(), b"data: two".to_vec());
    assert_eq!(buffer, b"leftover".to_vec());
    assert!(take_block(&mut buffer).is_none());
}

#[test]
fn sse_block_reads_multiline_data() {
    let block = br#"data: {"event_id":"e1","source":"gateway","incarnation":"i1","stream_id":"task:t1","seq":1,"schema_version":1,"kind":"task.state","durable":true,"payload":{},"at":10}"#;
    let event = parse_block(block).unwrap().unwrap();
    assert_eq!(event.event_id, "e1");
    assert_eq!(event.seq, 1);

    assert!(parse_block(b"event: ping").unwrap().is_none());
    let error = parse_block(b"data: {}").unwrap_err();
    assert_eq!(error.code, "sdk.stream_invalid");
}

fn event(stream: &str, seq: u64, kind: &str, durable: bool) -> StreamEvent {
    StreamEvent {
        event_id: format!("evt-{seq}-{kind}"),
        source: "worker".into(),
        incarnation: "inc-1".into(),
        stream_id: stream.into(),
        seq,
        schema_version: 1,
        kind: kind.into(),
        durable,
        payload: serde_json::json!({}),
        trace_id: None,
        at: 1,
    }
}

#[test]
fn cursor_drops_duplicate_sequences_and_tracks_durable_resume_point() {
    let mut cursor = StreamCursor::new(1);
    assert!(cursor
        .accept(event("task:t1", 1, "task.accepted", true))
        .unwrap()
        .is_none());
    assert!(cursor
        .accept(event("task:t1", 2, "model.delta", false))
        .unwrap()
        .is_some());
    assert_eq!(cursor.last_seq(), 2);
    assert_eq!(cursor.last_durable_seq(), 1);
    assert!(cursor
        .accept(event("task:t1", 3, "task.succeeded", true))
        .unwrap()
        .is_some());
    assert_eq!(cursor.last_durable_seq(), 3);
    // A replayed durable event below the highest observed sequence is dropped.
    assert!(cursor
        .accept(event("task:t1", 2, "task.running", true))
        .unwrap()
        .is_none());
}

#[test]
fn cursor_rejects_a_foreign_stream_and_keeps_control_notices() {
    let mut cursor = StreamCursor::new(4);
    let lagged = cursor
        .accept(event("task:t1", 4, "stream.lagged", false))
        .unwrap();
    assert_eq!(lagged.unwrap().kind, "stream.lagged");
    assert_eq!(cursor.last_seq(), 4);
    let error = cursor
        .accept(event("task:t2", 5, "task.running", true))
        .unwrap_err();
    assert_eq!(error.code, "sdk.stream_foreign");
}

#[test]
fn retry_policy_is_bounded_and_backs_off_exponentially() {
    let policy = RetryPolicy::new(u32::MAX, Duration::from_secs(60), Duration::from_secs(60));
    assert_eq!(policy.max_attempts(), MAX_ATTEMPTS);
    assert_eq!(policy.max_delay(), MAX_DELAY);
    assert_eq!(policy.initial_delay(), MAX_DELAY);
    assert_eq!(policy.delay_after(1), MAX_DELAY);
    assert_eq!(policy.delay_after(4), MAX_DELAY);

    let policy = RetryPolicy::new(4, Duration::from_millis(50), Duration::from_millis(300));
    assert_eq!(policy.delay_after(1), Duration::from_millis(50));
    assert_eq!(policy.delay_after(2), Duration::from_millis(100));
    assert_eq!(policy.delay_after(3), Duration::from_millis(200));
    assert_eq!(policy.delay_after(4), Duration::from_millis(300));

    let zero = RetryPolicy::new(0, Duration::ZERO, Duration::ZERO);
    assert_eq!(zero.max_attempts(), 1);
}

#[test]
fn client_errors_are_classified_with_actionable_guidance() {
    assert_eq!(
        classify(&ApiError::new("auth.unauthenticated", "missing")),
        ErrorKind::Authentication
    );
    assert_eq!(
        classify(&ApiError::new("delegation.expired", "expired")),
        ErrorKind::Authentication
    );
    assert_eq!(
        classify(&ApiError::new("scope.denied", "denied")),
        ErrorKind::Authorization
    );
    assert_eq!(
        classify(&ApiError::new("stream.cursor_expired", "expired")),
        ErrorKind::CursorExpired
    );
    assert_eq!(
        classify(&ApiError::new("stream.cursor_invalid", "bad")),
        ErrorKind::CursorInvalid
    );
    assert_eq!(
        classify(&ApiError::new("task.unknown_submission", "unknown")),
        ErrorKind::Uncertain
    );
    assert_eq!(
        classify(&ApiError::new("task.idempotency_conflict", "conflict")),
        ErrorKind::IdempotencyConflict
    );
    assert_eq!(
        classify(&ApiError::new("sdk.invalid_response", "protocol")),
        ErrorKind::Protocol
    );
    assert_eq!(
        classify(&ApiError::new("artifact.not_found", "missing")),
        ErrorKind::NotFound
    );
    let limited = ApiError::new("capacity.queue_full", "full").with("retry_after_ms", "250");
    assert_eq!(classify(&limited), ErrorKind::RateLimited);
    assert_eq!(retry_after(&limited), Some(Duration::from_millis(250)));
    assert_eq!(
        retry_after(&ApiError::new("capacity.queue_full", "full")),
        None
    );
}

fn task_projection(state: TaskState) -> GatewayTask {
    GatewayTask {
        task_id: "exec-1".into(),
        worker: "worker-a".into(),
        worker_instance: "general-a".into(),
        worker_incarnation: "inc-1".into(),
        connectivity: common::GatewayConnectivity::Online,
        task: Task {
            id: "task-1".into(),
            namespace: "local".into(),
            caller: "caller-1".into(),
            agent: "general".into(),
            session_id: "s".into(),
            input: "input".into(),
            idempotency_key: "key".into(),
            request_digest: "digest".into(),
            profile: "default".into(),
            profile_revision: 1,
            asset_revision: 1,
            trace_id: "0123456789abcdef0123456789abcdef".into(),
            traffic: "product".into(),
            resolved_model: None,
            team: None,
            team_version: None,
            team_roster: None,
            scopes: Vec::new(),
            delegation_path: Vec::new(),
            authority_expires_at: None,
            state,
            attempt: 1,
            native_invocation_id: None,
            native_thread_id: None,
            native_a2a_task_id: None,
            output: None,
            evidence: None,
            error: None,
            uncertainty: None,
            limits: common::Limits::default(),
            usage: common::Usage::default(),
            created_at: 1,
            updated_at: 1,
        },
    }
}

#[tokio::test]
async fn stream_buffer_rejects_an_unterminated_block_above_the_bound() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let server = std::thread::spawn(move || {
        let Ok((mut socket, _)) = listener.accept() else {
            return;
        };
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request);
        let _ = socket.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        );
        let payload = vec![b'x'; MAX_STREAM_BUFFER + 1];
        let _ = write!(socket, "{:x}\r\n", payload.len());
        let _ = socket.write_all(&payload);
        let _ = socket.write_all(b"\r\n");
        std::thread::sleep(Duration::from_millis(200));
    });

    let client = Client::new(base, "token");
    let mut watch = client.watch("task-1", 0).await.expect("watch opens");
    let error = watch
        .next()
        .await
        .expect_err("unterminated block is bounded");
    assert_eq!(error.code, "sdk.stream_overflow");
    server.join().expect("server thread");
}

#[test]
fn public_wire_contracts_round_trip_without_runtime_types() {
    let request = GatewaySubmitRequest {
        worker: Some("worker-a".into()),
        agent: "general".into(),
        session_id: "session".into(),
        input: "contract round trip".into(),
        idempotency_key: "contract-key".into(),
        profile: None,
        team: None,
        team_version: None,
        limits: Limits::default(),
    };
    let encoded = serde_json::to_string(&request).expect("request serializes");
    let decoded: GatewaySubmitRequest =
        serde_json::from_str(&encoded).expect("request deserializes");
    assert_eq!(decoded.worker.as_deref(), Some("worker-a"));
    assert_eq!(decoded.idempotency_key, "contract-key");

    let artifact = Artifact {
        id: "art-1".into(),
        task_id: "task-1".into(),
        native_invocation_id: "inv-1".into(),
        attempt: 1,
        trace_id: "0123456789abcdef0123456789abcdef".into(),
        version: 1,
        namespace: "local".into(),
        owner: "caller-1".into(),
        name: "evidence.txt".into(),
        content_type: "text/plain; charset=utf-8".into(),
        size: 5,
        digest: common::bytes_digest(b"hello"),
        finalized: true,
        created_at: 1,
    };
    let encoded = serde_json::to_string(&artifact).expect("artifact serializes");
    assert!(!encoded.contains("adk"));
    let decoded: Artifact = serde_json::from_str(&encoded).expect("artifact deserializes");
    assert_eq!(decoded.digest, artifact.digest);
    assert_eq!(decoded.task_id, "task-1");

    let error =
        ApiError::new("stream.cursor_expired", "cursor expired").with("stream_id", "task:task-1");
    let encoded = serde_json::to_string(&error).expect("error serializes");
    let decoded: ApiError = serde_json::from_str(&encoded).expect("error deserializes");
    assert_eq!(decoded.code, "stream.cursor_expired");
    assert_eq!(
        decoded.details.get("stream_id").map(String::as_str),
        Some("task:task-1")
    );
}

#[test]
fn cancellation_outcomes_are_typed() {
    assert_eq!(
        cancellation_outcome(&task_projection(TaskState::Cancelled)),
        CancellationOutcome::Confirmed
    );
    assert_eq!(
        cancellation_outcome(&task_projection(TaskState::Cancelling)),
        CancellationOutcome::Pending
    );
    assert_eq!(
        cancellation_outcome(&task_projection(TaskState::Running)),
        CancellationOutcome::Pending
    );
    assert_eq!(
        cancellation_outcome(&task_projection(TaskState::Succeeded)),
        CancellationOutcome::AlreadyTerminal
    );
    assert_eq!(
        cancellation_outcome(&task_projection(TaskState::Unknown)),
        CancellationOutcome::Uncertain
    );
}
