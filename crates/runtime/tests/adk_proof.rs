//! Ticket 01 executable compatibility proof.

use runtime::native::model::{model_for, DeterministicModel, HttpModel};
use runtime::native::proof::ProofHarness;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn native_runner_streams_and_reuses_canonical_session_history() {
    let harness = ProofHarness::new("proof-test", Arc::new(DeterministicModel::new()))
        .expect("native runner builds");
    let first = harness
        .run_turn("user-1", "session-1", "hello")
        .await
        .expect("first native turn");
    assert_eq!(first.output, "echo: hello [context:2]");
    assert_eq!(first.events.len(), 2, "user and model events are streamed");
    assert!(
        first.events.iter().any(|event| event.partial),
        "the model emits a partial stream chunk"
    );
    assert_eq!(first.history_len, 2, "canonical history has the first turn");

    let second = harness
        .run_turn("user-1", "session-1", "second")
        .await
        .expect("follow-up native turn");
    assert_eq!(second.output, "echo: second [context:4]");
    assert_eq!(second.history_len, 4, "follow-up retained native context");
    assert!(second
        .history
        .iter()
        .any(|event| event.text.contains("hello")));
}

#[tokio::test]
async fn provider_failure_is_observable_not_fabricated() {
    let harness = ProofHarness::new("proof-failure", Arc::new(DeterministicModel::new()))
        .expect("native runner builds");
    let error = harness
        .run_turn("user-1", "session-failure", "provider-fail")
        .await
        .expect_err("provider failure must surface");
    assert!(
        format!("{error}").contains("deterministic provider failure"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn invalid_model_configuration_fails_before_execution() {
    let error = match model_for("not-configured") {
        Ok(_) => panic!("unknown model must fail"),
        Err(error) => error,
    };
    assert!(format!("{error}").contains("not configured"));
}

#[tokio::test]
async fn native_runner_works_against_deterministic_http_endpoint() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind deterministic endpoint");
    let address = listener.local_addr().expect("local address");
    let server = tokio::spawn(async move {
        for _ in 0..1 {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = socket.read(&mut chunk).await.expect("read request");
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    let text = String::from_utf8_lossy(&buffer);
                    if let Some(length) = text
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                    {
                        let header_end = buffer
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                            .expect("header end")
                            + 4;
                        if buffer.len() >= header_end + length {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
            let body = r#"{"text":"endpoint: deterministic"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
    });
    let model = Arc::new(HttpModel::new(format!("http://{address}/generate"), "http"));
    let harness = ProofHarness::new("proof-http", model).expect("native runner builds");
    let turn = harness
        .run_turn("user-http", "session-http", "hello endpoint")
        .await
        .expect("native turn against deterministic endpoint");
    assert_eq!(turn.output, "endpoint: deterministic");
    assert_eq!(turn.history_len, 2);
    server.await.expect("server task");
}
