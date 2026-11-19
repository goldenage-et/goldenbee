//! The CLI decides and inspects approvals through the SDK contract only.
//!
//! A loopback Gateway records every request so the test can prove the CLI did
//! not add another transport or invent a digest.

use serde_json::{json, Value};
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

fn approval(task: &str, id: &str, digest: &str, status: &str) -> Value {
    json!({
        "task_id": task,
        "worker": "worker-a",
        "approval": {
            "id": id,
            "task_id": "worker-task",
            "namespace": "local",
            "operation": "ledger_write",
            "target": "invoice-7",
            "digest": digest,
            "risk": "external-write",
            "requested_by": "caller",
            "expires_at": 9_999_999_999_999_i64,
            "status": status,
            "created_at": 1
        }
    })
}

fn run(args: &[&str], base: &str, home: &std::path::Path) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_gb-client"))
        .env("GOLDENBEE_CLI_HOME", home)
        .args(args)
        .args(["--gateway", base, "--token", "caller-token", "--json"])
        .output()
        .expect("cli runs")
}

#[test]
fn cli_approval_retries_a_lost_acknowledgment_with_the_exact_digest() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fake gateway binds");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_server = seen.clone();
    let server = std::thread::spawn(move || {
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept().expect("fake gateway accepts");
            let (line, body) = read_request(&mut stream);
            assert!(line.contains("/approvals/approval-1/decision"), "{line}");
            seen_server.lock().expect("seen lock").push(body);
            if attempt == 0 {
                // The Gateway accepted the body but the acknowledgment was lost.
                drop(stream);
            } else {
                respond(
                    &mut stream,
                    &approval("task-1", "approval-1", "sha256:exact", "approved").to_string(),
                );
            }
        }
    });
    let home = tempfile::tempdir().expect("cli home");

    let approved = run(
        &[
            "approve",
            "task-1",
            "approval-1",
            "--digest",
            "sha256:exact",
        ],
        &base,
        home.path(),
    );
    assert!(
        approved.status.success(),
        "{}",
        String::from_utf8_lossy(&approved.stderr)
    );
    let approved: Value = serde_json::from_slice(&approved.stdout).expect("approval JSON");
    assert_eq!(approved["approval"]["status"], "approved");

    server.join().expect("fake gateway ends");
    let seen = seen.lock().expect("seen lock");
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0], seen[1], "retry changed the decision body");
    let decision: Value = serde_json::from_str(&seen[1]).expect("decision body");
    assert_eq!(decision["digest"], "sha256:exact");
    assert_eq!(decision["decision"], "approved");
}

#[test]
fn cli_lists_decides_and_revokes_worker_owned_approvals() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fake gateway binds");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let seen_server = seen.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..5 {
            let (mut stream, _) = listener.accept().expect("fake gateway accepts");
            let (line, body) = read_request(&mut stream);
            seen_server
                .lock()
                .expect("seen lock")
                .push((line.clone(), body.clone()));
            if line.contains("/approvals?pending=true") {
                respond(
                    &mut stream,
                    &json!([approval("task-1", "approval-1", "sha256:exact", "pending")])
                        .to_string(),
                );
            } else if line.starts_with("GET ") && line.contains("/approvals/approval-1") {
                respond(
                    &mut stream,
                    &approval("task-1", "approval-1", "sha256:exact", "pending").to_string(),
                );
            } else if line.contains("/approvals/approval-1/decision") {
                let decision: Value = serde_json::from_str(&body).expect("decision body");
                assert_eq!(decision["digest"], "sha256:exact");
                assert_eq!(decision["decision"], "approved");
                respond(
                    &mut stream,
                    &approval("task-1", "approval-1", "sha256:exact", "approved").to_string(),
                );
            } else if line.contains("/approvals/approval-2/decision") {
                let decision: Value = serde_json::from_str(&body).expect("decision body");
                assert_eq!(decision["digest"], "sha256:chosen");
                assert_eq!(decision["decision"], "rejected");
                respond(
                    &mut stream,
                    &approval("task-1", "approval-2", "sha256:chosen", "rejected").to_string(),
                );
            } else if line.contains("/approvals/approval-3/revoke") {
                respond(
                    &mut stream,
                    &approval("task-1", "approval-3", "sha256:third", "revoked").to_string(),
                );
            } else {
                respond(&mut stream, &json!({"code": "test.unexpected"}).to_string());
            }
        }
    });
    let home = tempfile::tempdir().expect("cli home");

    let listed = run(&["approvals", "task-1", "--pending"], &base, home.path());
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listed: Value = serde_json::from_slice(&listed.stdout).expect("approval list JSON");
    assert_eq!(listed[0]["task_id"], "task-1");
    assert_eq!(listed[0]["approval"]["digest"], "sha256:exact");

    let approved = run(&["approve", "task-1", "approval-1"], &base, home.path());
    assert!(
        approved.status.success(),
        "{}",
        String::from_utf8_lossy(&approved.stderr)
    );
    let approved: Value = serde_json::from_slice(&approved.stdout).expect("approval JSON");
    assert_eq!(approved["approval"]["status"], "approved");

    let rejected = run(
        &[
            "reject",
            "task-1",
            "approval-2",
            "--digest",
            "sha256:chosen",
        ],
        &base,
        home.path(),
    );
    assert!(
        rejected.status.success(),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );

    let revoked = run(&["revoke", "task-1", "approval-3"], &base, home.path());
    assert!(
        revoked.status.success(),
        "{}",
        String::from_utf8_lossy(&revoked.stderr)
    );

    server.join().expect("fake gateway ends");
    let seen = seen.lock().expect("seen lock");
    assert_eq!(seen.len(), 5, "{seen:?}");
    assert!(seen
        .iter()
        .all(|(line, _)| line.contains("/v1/tasks/task-1/approvals")));
}
