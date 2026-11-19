//! The CLI uses the reusable SDK for fenced control instead of its own transport.

use serde_json::Value;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

fn read_request(stream: &mut std::net::TcpStream) -> (String, String) {
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
                    let (name, value) = line.split_once(':')?;
                    if name.eq_ignore_ascii_case("content-length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
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
            return (line, body);
        }
    }
    (String::new(), String::new())
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

#[test]
fn cli_control_acquire_uses_the_sdk_gateway_contract() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fake gateway binds");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let seen = Arc::new(Mutex::new(Vec::<(String, String, String)>::new()));
    let seen_server = seen.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("fake gateway accepts");
            let (line, body) = read_request(&mut stream);
            let head = format!("{line}\n{body}");
            seen_server
                .lock()
                .expect("seen lock")
                .push((line.clone(), body, String::new()));
            let response = serde_json::json!({
                "session_id": "session-a",
                "generation": 3,
                "owner": "human",
                "principal": "caller",
                "epoch": "worker-epoch",
                "expires_at_ms": 123
            });
            respond(&mut stream, &response.to_string());
            let _ = head;
        }
    });

    let home = tempfile::tempdir().expect("cli home");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gb-client"))
        .env("GOLDENBEE_CLI_HOME", home.path())
        .args([
            "control",
            "acquire",
            "--gateway",
            &base,
            "--token",
            "caller-token",
            "--worker",
            "worker-a",
            "--session",
            "session-a",
            "--json",
        ])
        .output()
        .expect("cli runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lease: Value = serde_json::from_slice(&output.stdout).expect("lease JSON");
    assert_eq!(lease["generation"], 3);
    assert_eq!(lease["owner"], "human");
    assert_eq!(lease["epoch"], "worker-epoch");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gb-client"))
        .env("GOLDENBEE_CLI_HOME", home.path())
        .args([
            "control",
            "status",
            "--gateway",
            &base,
            "--token",
            "caller-token",
            "--worker",
            "worker-a",
            "--session",
            "session-a",
            "--json",
        ])
        .output()
        .expect("cli runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join().expect("fake gateway joins");

    let seen = seen.lock().expect("seen lock");
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[0].0,
        "POST /v1/workers/worker-a/sessions/session-a/control HTTP/1.1"
    );
    assert!(seen[0].1.contains("\"session_id\":\"session-a\""));
    assert_eq!(
        seen[1].0,
        "GET /v1/workers/worker-a/sessions/session-a/control HTTP/1.1"
    );
}
