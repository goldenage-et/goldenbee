//! Cross-process session evidence: durable native history, caller isolation,
//! and writing-turn ordering through the General Worker.

use common::{Limits, SubmitTaskRequest};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod fixture;

struct Worker {
    child: Child,
    data: tempfile::TempDir,
    base: String,
    instance: String,
    incarnation: String,
    token: String,
    env: Vec<(String, String)>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_worker(
    data: &Path,
    token: &str,
    env: &[(&str, String)],
) -> (Child, String, String, String) {
    let mut model = "deterministic".to_string();
    let mut principals = Vec::new();
    for (key, value) in env {
        if *key == "GOLDENBEE_MODEL" {
            model = value.clone();
        }
        if *key == "GOLDENBEE_PRINCIPALS" {
            let bindings: Vec<Value> = serde_json::from_str(value).expect("principal bindings");
            for binding in bindings {
                principals.push((
                    binding["token"].as_str().expect("token").to_string(),
                    binding["namespace"]
                        .as_str()
                        .expect("namespace")
                        .to_string(),
                    binding["principal"]
                        .as_str()
                        .expect("principal")
                        .to_string(),
                ));
            }
        }
    }
    let fixture = fixture::write(
        data,
        fixture::Options {
            instance: format!("general-{}", data.file_name().unwrap().to_string_lossy()),
            token: Some(token.to_string()),
            model,
            principals,
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_general"));
    command
        .args([
            "serve",
            "--config",
            fixture.config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in env {
        if *key == "GOLDENBEE_MODEL" || *key == "GOLDENBEE_PRINCIPALS" {
            continue;
        }
        command.env(key, value);
    }
    let mut child = command.spawn().expect("worker serves");
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
    assert!(base.starts_with("http://127.0.0.1:"), "{line}");
    let health = reqwest::blocking::Client::new()
        .get(format!("{base}/v1/health"))
        .send()
        .expect("health responds")
        .json::<Value>()
        .expect("health is JSON");
    let instance = health["instance"]
        .as_str()
        .expect("instance identity")
        .to_string();
    let incarnation = health["incarnation"]
        .as_str()
        .expect("process incarnation")
        .to_string();
    (child, base, instance, incarnation)
}

impl Worker {
    fn start(token: &str) -> Self {
        Self::start_with(token, &[])
    }

    fn start_with(token: &str, env: &[(&str, String)]) -> Self {
        let data = tempfile::tempdir().expect("temporary data directory");
        let (child, base, instance, incarnation) = spawn_worker(data.path(), token, env);
        Self {
            child,
            data,
            base,
            instance,
            incarnation,
            token: token.to_string(),
            env: env
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect(),
        }
    }

    /// Stop the process and start a new incarnation against the same data
    /// directory, as an operator restart would.
    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let env: Vec<(&str, String)> = self
            .env
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect();
        let (child, base, instance, incarnation) =
            spawn_worker(self.data.path(), &self.token, &env);
        self.child = child;
        self.base = base;
        self.instance = instance;
        self.incarnation = incarnation;
    }

    fn submit(&self, token: &str, session: &str, input: &str, key: &str) -> Value {
        let response = reqwest::blocking::Client::new()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(token)
            .json(&SubmitTaskRequest {
                namespace: None,
                agent: "general".into(),
                session_id: session.into(),
                input: input.into(),
                idempotency_key: key.into(),
                profile: None,
                team: None,
                worker: None,
                limits: Limits::default(),
            })
            .send()
            .expect("submit responds");
        let status = response.status();
        let body = response.text().expect("submit body");
        assert!(status.is_success(), "submit failed: {status} {body}");
        serde_json::from_str(&body).expect("submit is JSON")
    }

    fn wait(&self, token: &str, task_id: &str) -> Value {
        for _ in 0..500 {
            let response = reqwest::blocking::Client::new()
                .get(format!("{}/v1/tasks/{task_id}", self.base))
                .bearer_auth(token)
                .send()
                .expect("task responds");
            assert!(
                response.status().is_success(),
                "task read failed: {}",
                response.text().unwrap_or_default()
            );
            let task = response.json::<Value>().expect("task is JSON");
            if matches!(
                task["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled" | "unknown")
            ) {
                return task;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("task did not reach a terminal state");
    }

    fn run(&self, token: &str, session: &str, input: &str, key: &str) -> Value {
        let submitted = self.submit(token, session, input, key);
        self.wait(token, submitted["task_id"].as_str().expect("task id"))
    }

    fn status(&self, token: &str, path: &str) -> reqwest::StatusCode {
        reqwest::blocking::Client::new()
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .expect("request responds")
            .status()
    }
}

fn output(task: &Value) -> String {
    task["output"].as_str().unwrap_or_default().to_string()
}

/// Deterministic model endpoint that records how many requests it serves at
/// the same time. The native Runner, not this fixture, is the system under
/// test.
fn slow_model(delay: Duration) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind model endpoint");
    let endpoint = format!("http://{}", listener.local_addr().expect("model address"));
    let active = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(AtomicUsize::new(0));
    let observed_for_thread = observed.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let active = active.clone();
            let observed = observed_for_thread.clone();
            std::thread::spawn(move || {
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let read = stream.read(&mut chunk).unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if let Some(head) = request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..head]).to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request.len() >= head + 4 + length {
                            break;
                        }
                    }
                }
                let concurrent = active.fetch_add(1, Ordering::SeqCst) + 1;
                observed.fetch_max(concurrent, Ordering::SeqCst);
                std::thread::sleep(delay);
                active.fetch_sub(1, Ordering::SeqCst);
                let body = r#"{"text":"slow-ok"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            });
        }
    });
    (endpoint, observed)
}

#[test]
fn session_identity_is_namespace_principal_and_session_id() {
    let bindings = serde_json::json!([
        {"token": "other-ns", "namespace": "other", "principal": "local",
         "scopes": ["task:submit", "task:read"]},
        {"token": "mallory", "namespace": "local", "principal": "mallory",
         "scopes": ["task:submit", "task:read"]}
    ])
    .to_string();
    let worker = Worker::start_with("dev-token", &[("GOLDENBEE_PRINCIPALS", bindings)]);

    let alpha = worker.run("dev-token", "shared", "alpha-secret", "iso-1");
    assert!(
        output(&alpha).contains("[context:2]"),
        "original history starts empty: {}",
        output(&alpha)
    );

    // Same principal id, different application namespace.
    let beta = worker.run("other-ns", "shared", "beta-secret", "iso-2");
    assert!(
        output(&beta).contains("[context:2]"),
        "namespace must separate history: {}",
        output(&beta)
    );
    assert!(!output(&beta).contains("alpha-secret"));

    // Same namespace, different authenticated principal.
    let gamma = worker.run("mallory", "shared", "gamma-secret", "iso-3");
    assert!(
        output(&gamma).contains("[context:2]"),
        "principal must separate history: {}",
        output(&gamma)
    );
    assert!(!output(&gamma).contains("alpha-secret"));

    // Another caller's writes cannot modify the original identity.
    let delta = worker.run("dev-token", "shared", "delta-secret", "iso-4");
    let text = output(&delta);
    assert!(text.contains("[context:4]"), "{text}");
    assert!(!text.contains("beta-secret"));
    assert!(!text.contains("gamma-secret"));

    // Another caller cannot read the original task or its retained events.
    let task_id = alpha["id"].as_str().expect("task id");
    assert_eq!(
        worker.status("mallory", &format!("/v1/tasks/{task_id}")),
        reqwest::StatusCode::FORBIDDEN
    );
    assert_eq!(
        worker.status("other-ns", &format!("/v1/tasks/{task_id}")),
        reqwest::StatusCode::FORBIDDEN
    );
    assert_eq!(
        worker.status("mallory", &format!("/v1/tasks/{task_id}/events")),
        reqwest::StatusCode::FORBIDDEN
    );

    let visible: Value = reqwest::blocking::Client::new()
        .get(format!("{}/v1/tasks", worker.base))
        .bearer_auth("mallory")
        .send()
        .expect("task list responds")
        .json()
        .expect("task list is JSON");
    let visible = visible.as_array().expect("task list");
    assert_eq!(visible.len(), 1, "only the caller's own task is visible");
    assert_eq!(visible[0]["id"], gamma["id"]);
}

#[test]
fn distinct_session_identities_run_concurrently() {
    let (endpoint, observed) = slow_model(Duration::from_millis(300));
    let worker = Worker::start_with(
        "dev-token",
        &[("GOLDENBEE_MODEL", format!("http+{endpoint}"))],
    );

    let first = worker.submit("dev-token", "concurrent-a", "one", "concurrent-1");
    let second = worker.submit("dev-token", "concurrent-b", "two", "concurrent-2");
    let first = worker.wait("dev-token", first["task_id"].as_str().unwrap());
    let second = worker.wait("dev-token", second["task_id"].as_str().unwrap());

    assert_eq!(first["state"], "succeeded");
    assert_eq!(second["state"], "succeeded");
    assert_eq!(output(&first), "slow-ok");
    assert_eq!(output(&second), "slow-ok");
    assert!(
        observed.load(Ordering::SeqCst) >= 2,
        "independent sessions must overlap in the native Runner"
    );

    let tasks: Value = reqwest::blocking::Client::new()
        .get(format!("{}/v1/tasks", worker.base))
        .bearer_auth("dev-token")
        .send()
        .expect("task list responds")
        .json()
        .expect("task list is JSON");
    assert_eq!(
        tasks.as_array().expect("task list").len(),
        2,
        "reading or waiting must not submit extra turns"
    );
}

#[test]
fn concurrent_writes_to_one_identity_are_serialized() {
    let worker = Worker::start("dev-token");
    let started = Instant::now();
    let first = worker.submit(
        "dev-token",
        "serialized",
        "delay:300 first-write",
        "serialized-1",
    );
    let second = worker.submit(
        "dev-token",
        "serialized",
        "delay:300 second-write",
        "serialized-2",
    );
    let first = worker.wait("dev-token", first["task_id"].as_str().unwrap());
    let second = worker.wait("dev-token", second["task_id"].as_str().unwrap());
    let elapsed = started.elapsed();

    assert_eq!(first["state"], "succeeded");
    assert_eq!(second["state"], "succeeded");
    let mut contexts = vec![output(&first), output(&second)];
    contexts.sort();
    assert!(
        contexts[0].contains("[context:2]") && contexts[1].contains("[context:4]"),
        "writes to one identity must be ordered, got {contexts:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(550),
        "one writing turn must wait for the other, took {elapsed:?}"
    );
}

#[test]
fn unavailable_or_corrupt_session_storage_fails_before_serving() {
    let root = tempfile::tempdir().expect("temporary directory");
    let config_root = tempfile::tempdir().expect("config directory");
    let fixture = fixture::write(
        config_root.path(),
        fixture::Options {
            instance: "storage-failure".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let run = |data: &Path| {
        let mut text = std::fs::read_to_string(&fixture.config).expect("config");
        let marker = format!("directory = \"{}\"", config_root.path().display());
        let replacement = format!("directory = \"{}\"", data.display());
        text = text.replace(&marker, &replacement);
        let config = config_root.path().join(format!(
            "worker-{}.toml",
            data.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&config, text).expect("config variant");
        Command::new(env!("CARGO_BIN_EXE_general"))
            .args([
                "serve",
                "--config",
                config.to_str().expect("config path"),
                "--addr",
                "127.0.0.1:0",
            ])
            .output()
            .expect("general exits truthfully")
    };

    let blocked = root.path().join("blocked");
    std::fs::write(&blocked, "not a directory").expect("blocking file");
    let output = run(&blocked);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(!stdout.contains("listening on"), "{stdout}");
    assert!(
        stderr.contains("session.storage_unavailable"),
        "storage failure must be truthful: {stderr}"
    );

    let corrupt = root.path().join("corrupt");
    std::fs::create_dir_all(&corrupt).expect("data directory");
    std::fs::write(corrupt.join("native-sessions.json"), "{ not json").expect("corrupt store");
    let output = run(&corrupt);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(!stdout.contains("listening on"), "{stdout}");
    assert!(
        stderr.contains("session.storage_corrupt"),
        "a corrupt store must not be replaced silently: {stderr}"
    );

    let unsupported = root.path().join("unsupported");
    std::fs::create_dir_all(&unsupported).expect("data directory");
    std::fs::write(
        unsupported.join("native-sessions.json"),
        r#"{"schema": 999, "sessions": {}}"#,
    )
    .expect("unsupported store");
    let output = run(&unsupported);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(!stdout.contains("listening on"), "{stdout}");
    assert!(
        stderr.contains("session.storage_unsupported"),
        "a newer store must not be rewritten: {stderr}"
    );
}

#[test]
fn distinct_worker_instances_keep_separate_canonical_stores() {
    let first = Worker::start("dev-token");
    let second = Worker::start("dev-token");

    let first_one = first.run("dev-token", "same-id", "one", "instance-a1");
    assert!(output(&first_one).contains("[context:2]"));
    let first_two = first.run("dev-token", "same-id", "two", "instance-a2");
    assert!(output(&first_two).contains("[context:4]"));

    // The same package and session id on another instance start empty.
    let second_one = second.run("dev-token", "same-id", "one", "instance-b1");
    assert!(
        output(&second_one).contains("[context:2]"),
        "instances must not share history: {}",
        output(&second_one)
    );
    assert_ne!(first_one["id"], second_one["id"]);
}

#[test]
fn native_history_survives_worker_restart() {
    let mut worker = Worker::start("dev-token");
    let first = worker.run("dev-token", "restart-session", "first turn", "restart-1");
    assert_eq!(first["state"], "succeeded");
    assert!(
        output(&first).contains("[context:2]"),
        "unexpected first output: {}",
        output(&first)
    );

    // Read-only inspection must not add turns or corrupt history.
    for _ in 0..3 {
        let page = reqwest::blocking::Client::new()
            .get(format!(
                "{}/v1/tasks/{}/events",
                worker.base,
                first["id"].as_str().unwrap()
            ))
            .bearer_auth("dev-token")
            .send()
            .expect("events respond");
        assert!(page.status().is_success());
    }

    let first_incarnation = worker.incarnation.clone();
    worker.restart();
    assert_ne!(
        first_incarnation, worker.incarnation,
        "a restart is new incarnation"
    );

    let second = worker.run("dev-token", "restart-session", "second turn", "restart-2");
    assert_eq!(second["state"], "succeeded");
    let text = output(&second);
    assert!(text.contains("second turn"), "{text}");
    assert!(
        text.contains("[context:4]"),
        "follow-up must see persisted prior history: {text}"
    );
}
