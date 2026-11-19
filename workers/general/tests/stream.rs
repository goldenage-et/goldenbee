//! Cross-process durable stream acceptance: replay, cursors, reconnect, and
//! bounded fan-out over the public Worker SSE route.

use common::{Limits, StreamEvent, SubmitTaskRequest};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod fixture;

const TOKEN: &str = "stream-token";
const FOREIGN_TOKEN: &str = "stream-foreign-token";

struct Worker {
    child: Child,
    #[allow(dead_code)]
    data: tempfile::TempDir,
    config: PathBuf,
    base: String,
    token: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(stream: fixture::StreamSettings) -> Worker {
    let data = tempfile::tempdir().expect("worker data directory");
    let fixture = fixture::write_stream(
        data.path(),
        fixture::Options {
            instance: "general-stream".into(),
            token: Some(TOKEN.into()),
            model: "deterministic".into(),
            principals: vec![(
                FOREIGN_TOKEN.into(),
                "tenant-b".into(),
                "tenant-b-caller".into(),
            )],
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
        stream,
    );
    let (child, base) = spawn(&fixture.config);
    Worker {
        child,
        data,
        config: fixture.config,
        base,
        token: TOKEN.into(),
    }
}

fn spawn(config: &Path) -> (Child, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("worker serves");
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("listener address is announced");
    let base = line
        .split("listening on ")
        .nth(1)
        .expect("address announcement")
        .trim()
        .to_string();
    (child, base)
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("stream client")
}

impl Worker {
    fn submit(&self, input: &str, key: &str) -> Value {
        let response = client()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(&self.token)
            .json(&SubmitTaskRequest {
                namespace: None,
                agent: "general".into(),
                session_id: format!("stream-{key}"),
                input: input.into(),
                idempotency_key: key.into(),
                profile: None,
                team: None,
                worker: None,
                limits: Limits::default(),
            })
            .send()
            .expect("submit responds");
        assert!(
            response.status().is_success(),
            "submit failed: {}",
            response.status()
        );
        response.json().expect("submit is JSON")
    }

    fn task(&self, id: &str) -> Value {
        client()
            .get(format!("{}/v1/tasks/{id}", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("task responds")
            .json()
            .expect("task is JSON")
    }

    fn wait(&self, id: &str) -> Value {
        for _ in 0..500 {
            let task = self.task(id);
            if matches!(
                task["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled" | "unknown")
            ) {
                return task;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("task {id} did not become terminal");
    }

    fn stream_response(&self, id: &str, after: u64, token: &str) -> reqwest::blocking::Response {
        client()
            .get(format!(
                "{}/v1/tasks/{id}/events/stream?after={after}",
                self.base
            ))
            .bearer_auth(token)
            .send()
            .expect("stream responds")
    }

    fn stream(&self, id: &str, after: u64) -> Sse {
        let response = self.stream_response(id, after, &self.token);
        assert_eq!(
            response.status(),
            reqwest::StatusCode::OK,
            "stream rejected: {}",
            response.status()
        );
        Sse {
            reader: BufReader::new(response),
        }
    }

    fn error(&self, path: &str, token: &str) -> (reqwest::StatusCode, Value) {
        let response = client()
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .expect("request responds");
        let status = response.status();
        let body = response.json().unwrap_or(Value::Null);
        (status, body)
    }

    fn events(&self, id: &str, after: u64) -> Value {
        client()
            .get(format!("{}/v1/tasks/{id}/events?after={after}", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("events responds")
            .json()
            .expect("events is JSON")
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (child, base) = spawn(&self.config);
        self.child = child;
        self.base = base;
    }
}

struct Sse {
    reader: BufReader<reqwest::blocking::Response>,
}

enum Frame {
    Event(Box<StreamEvent>),
    Comment,
}

impl Sse {
    fn next_frame(&mut self) -> Option<Frame> {
        let mut data = String::new();
        let mut comment = None;
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).ok()?;
            if read == 0 {
                return None;
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !data.is_empty() {
                    let event = serde_json::from_str(&data).expect("stream event is JSON");
                    return Some(Frame::Event(Box::new(event)));
                }
                if let Some(comment) = comment.take() {
                    let _ = comment;
                    return Some(Frame::Comment);
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("data:") {
                data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            } else if let Some(rest) = line.strip_prefix(':') {
                comment = Some(rest.trim().to_string());
            }
        }
    }

    fn next_event(&mut self) -> Option<StreamEvent> {
        loop {
            match self.next_frame()? {
                Frame::Event(event) => return Some(*event),
                Frame::Comment => continue,
            }
        }
    }

    fn until_terminal(&mut self) -> Vec<StreamEvent> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            let Some(event) = self.next_event() else {
                break;
            };
            let terminal = matches!(
                event.kind.as_str(),
                "task.succeeded" | "task.failed" | "task.cancelled" | "task.unknown"
            );
            events.push(event);
            if terminal {
                return events;
            }
        }
        panic!("stream did not reach a terminal event");
    }
}

fn error_code(body: &Value) -> &str {
    body["code"].as_str().unwrap_or_default()
}

#[test]
fn malformed_too_new_foreign_and_unauthenticated_cursors_are_typed() {
    let worker = start(fixture::StreamSettings::default());
    let submitted = worker.submit("cursor contract", "cursor-contract");
    let id = submitted["task_id"].as_str().unwrap();

    // No credentials never opens a stream.
    let response = client()
        .get(format!(
            "{}/v1/tasks/{id}/events/stream?after=0",
            worker.base
        ))
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);

    let (status, body) = worker.error(&format!("/v1/tasks/{id}/events/stream?after=abc"), TOKEN);
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "stream.cursor_invalid");
    assert!(!body["recovery"].as_str().unwrap_or_default().is_empty());

    // A cursor from another stream (or a future sequence) is not an empty 200.
    let (status, body) = worker.error(
        &format!("/v1/tasks/{id}/events/stream?after={}", u64::MAX),
        TOKEN,
    );
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "stream.cursor_invalid");
    assert!(body["details"]["newest_seq"].as_str().is_some());

    // Another tenant cannot read this stream; authorization is explicit and no
    // event payload is returned with the rejection.
    let (status, body) = worker.error(
        &format!("/v1/tasks/{id}/events/stream?after=0"),
        FOREIGN_TOKEN,
    );
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), "scope.namespace_denied");
    assert!(body["details"].get("events").is_none());
}

#[test]
fn trimmed_history_requires_explicit_resynchronization() {
    let worker = start(fixture::StreamSettings {
        event_retention: Some(2),
        ..fixture::StreamSettings::default()
    });
    let submitted = worker.submit("trimmed history", "trimmed-history");
    let id = submitted["task_id"].as_str().unwrap();
    worker.wait(id);

    let page = worker.events(id, 0);
    let oldest = page["oldest_seq"].as_u64().unwrap();
    assert!(oldest > 1, "fixture must trim at least one event: {page}");

    let (status, body) = worker.error(&format!("/v1/tasks/{id}/events/stream?after=0"), TOKEN);
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "stream.cursor_expired");
    assert!(!body["recovery"].as_str().unwrap_or_default().is_empty());

    // A cursor inside retention is accepted and replays only retained events.
    let events = worker.stream(id, oldest - 1).until_terminal();
    assert!(events.iter().all(|event| event.seq >= oldest));
    assert!(events.iter().any(|event| event.kind == "task.succeeded"));
}

#[test]
fn snapshot_to_live_handoff_has_no_durable_gap_or_duplicate() {
    let worker = start(fixture::StreamSettings::default());
    let submitted = worker.submit("delay:500 handoff", "handoff");
    let id = submitted["task_id"].as_str().unwrap().to_string();

    let events = worker.stream(&id, 0).until_terminal();
    let seqs: Vec<u64> = events.iter().map(|event| event.seq).collect();
    let mut unique = seqs.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        seqs.len(),
        "duplicate live sequence: {seqs:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "task.succeeded")
            .count(),
        1,
        "terminal event delivered exactly once"
    );
    assert!(events
        .iter()
        .all(|event| event.stream_id == format!("task:{id}")));

    // Every durable event retained by the canonical page reached the stream,
    // whether through replay or live delivery.
    let page = worker.events(&id, 0);
    let retained: Vec<&Value> = page["events"].as_array().unwrap().iter().collect();
    let delivered: Vec<&str> = events.iter().map(|event| event.event_id.as_str()).collect();
    for event in retained {
        assert!(
            delivered.contains(&event["event_id"].as_str().unwrap()),
            "durable event {} was lost across the handoff",
            event["event_id"]
        );
    }

    // Ephemeral model output is explicitly non-durable; terminal outcomes and
    // the canonical result are durable.
    assert!(events
        .iter()
        .filter(|event| event.kind == "model.delta")
        .all(|event| !event.durable));
    assert!(events
        .iter()
        .find(|event| event.kind == "task.succeeded")
        .is_some_and(|event| event.durable));
}

#[test]
fn completion_during_reconnect_is_recovered_without_duplicates_or_resubmission() {
    let worker = start(fixture::StreamSettings::default());
    let submitted = worker.submit("delay:400 reconnect completion", "reconnect-completion");
    let id = submitted["task_id"].as_str().unwrap().to_string();

    let mut first = worker.stream(&id, 0);
    let mut seen = Vec::new();
    let mut cursor = 0;
    loop {
        let event = first.next_event().expect("first stream stays open");
        cursor = cursor.max(event.seq);
        let running = event.kind == "task.running";
        seen.push(event);
        if running {
            break;
        }
    }
    drop(first);

    let terminal = worker.wait(&id);
    assert_eq!(terminal["state"], "succeeded");
    assert_eq!(terminal["id"].as_str(), Some(id.as_str()));
    let invocation = terminal["native_invocation_id"].clone();

    let mut second = worker.stream(&id, cursor);
    let replay = second.until_terminal();
    for event in &replay {
        assert!(
            !seen.iter().any(|prior| prior.seq == event.seq),
            "reconnect replayed sequence {} twice",
            event.seq
        );
    }
    let succeeded = replay
        .iter()
        .find(|event| event.kind == "task.succeeded")
        .expect("terminal outcome is recovered");
    assert!(succeeded.payload["output"]
        .as_str()
        .is_some_and(|output| output.contains("reconnect completion")));

    let tasks: Value = client()
        .get(format!("{}/v1/tasks", worker.base))
        .bearer_auth(&worker.token)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(
        tasks.as_array().unwrap().len(),
        1,
        "reconnect submitted work"
    );
    assert_eq!(
        worker.task(&id)["native_invocation_id"],
        invocation,
        "reconnect created a second native invocation"
    );
}

#[test]
fn worker_restart_replays_terminal_completion() {
    let mut worker = start(fixture::StreamSettings::default());
    let submitted = worker.submit("durable restart", "restart-terminal");
    let id = submitted["task_id"].as_str().unwrap().to_string();
    let terminal = worker.wait(&id);
    assert_eq!(terminal["state"], "succeeded");
    let output = terminal["output"].clone();

    worker.restart();

    let events = worker.stream(&id, 0).until_terminal();
    let succeeded = events
        .iter()
        .find(|event| event.kind == "task.succeeded")
        .expect("terminal outcome survives restart");
    assert_eq!(succeeded.payload["output"], output);
    assert_eq!(worker.task(&id)["id"].as_str(), Some(id.as_str()));
}

#[test]
fn worker_restart_exposes_truthful_recovery_without_a_second_task() {
    let mut worker = start(fixture::StreamSettings::default());
    let submitted = worker.submit("delay:3000 interrupted", "restart-running");
    let id = submitted["task_id"].as_str().unwrap().to_string();

    let mut first = worker.stream(&id, 0);
    first
        .next_event()
        .expect("the running task announces events");
    drop(first);

    worker.restart();

    let mut recovery = worker.stream(&id, 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut kinds = Vec::new();
    while Instant::now() < deadline {
        let Some(event) = recovery.next_event() else {
            break;
        };
        kinds.push(event.kind.clone());
        if event.kind == "task.recovery_required" {
            break;
        }
    }
    assert!(
        kinds.iter().any(|kind| kind == "task.recovery_required"),
        "{kinds:?}"
    );
    let task = worker.task(&id);
    assert!(
        matches!(task["state"].as_str(), Some("recovering" | "unknown")),
        "{task}"
    );
    let tasks: Value = client()
        .get(format!("{}/v1/tasks", worker.base))
        .bearer_auth(&worker.token)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(tasks.as_array().unwrap().len(), 1);
}

#[test]
fn secret_canary_is_redacted_over_real_sse() {
    let worker = start(fixture::StreamSettings::default());
    let submitted = worker.submit("delay:300 echo sk-stream-secret", "secret-canary");
    let id = submitted["task_id"].as_str().unwrap();
    let events = worker.stream(id, 0).until_terminal();

    let encoded = serde_json::to_string(&events).expect("events serialize");
    assert!(
        !encoded.contains("sk-stream-secret"),
        "secret leaked through the public stream: {encoded}"
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "model.delta" || event.kind == "model.delta.complete"),
        "the redacted event still arrives"
    );
}

#[test]
fn keepalive_is_bounded_and_configurable() {
    let worker = start(fixture::StreamSettings {
        stream_keepalive_ms: Some(25),
        ..fixture::StreamSettings::default()
    });
    let submitted = worker.submit("delay:400 keepalive", "keepalive");
    let id = submitted["task_id"].as_str().unwrap();
    let response = worker.stream_response(id, 0, &worker.token);
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.text().expect("stream text");
    assert!(
        body.contains(": goldenbee"),
        "keepalive frame missing: {body}"
    );
    assert!(body.contains("event: task.succeeded"), "{body}");
}

#[test]
fn subscriber_limit_is_bounded_released_and_never_stalls_execution() {
    let worker = start(fixture::StreamSettings {
        max_streams: Some(1),
        ..fixture::StreamSettings::default()
    });
    let long = worker.submit("delay:1500 held", "held-stream");
    let long_id = long["task_id"].as_str().unwrap();

    // Hold the only slot. The response is not read, so the connection stays open
    // until this value is dropped.
    let held = worker.stream_response(long_id, 0, &worker.token);
    assert_eq!(held.status(), reqwest::StatusCode::OK);

    let (status, body) = worker.error(&format!("/v1/tasks/{long_id}/events/stream?after=0"), TOKEN);
    assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_code(&body), "stream.subscriber_limit");
    assert_eq!(body["retryable"].as_bool(), Some(true));

    // Execution is independent of viewer capacity.
    let other = worker.submit("delay:0 still runs", "still-runs");
    let other_id = other["task_id"].as_str().unwrap();
    let other_task = worker.wait(other_id);
    assert_eq!(other_task["state"], "succeeded");

    drop(held);
    // The released permit is reusable for a later viewer.
    let mut released = None;
    for _ in 0..100 {
        let response = worker.stream_response(other_id, 0, &worker.token);
        if response.status() == reqwest::StatusCode::OK {
            released = Some(response);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(released.is_some(), "stream slot was not released");
}

#[test]
fn streams_are_scoped_to_one_task_without_global_ordering() {
    let worker = start(fixture::StreamSettings::default());
    let first = worker.submit("delay:0 first", "scoped-first");
    let second = worker.submit("delay:0 second", "scoped-second");
    for supplied in [first, second] {
        let id = supplied["task_id"].as_str().unwrap();
        let events = worker.stream(id, 0).until_terminal();
        assert_eq!(events.first().map(|event| event.seq), Some(1));
        assert!(events
            .iter()
            .all(|event| event.stream_id == format!("task:{id}")));
    }
}
