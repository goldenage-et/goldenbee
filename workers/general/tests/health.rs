//! Cross-process Worker health, readiness, drain, and shutdown evidence.
//!
//! These tests drive the real executable. They assert observable process,
//! listener, stream, and child-process behavior instead of internal callback
//! ordering alone.

use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

mod fixture;

struct Worker {
    child: Child,
    base: String,
    token: String,
    data: PathBuf,
    config: PathBuf,
    log: PathBuf,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn options(instance: &str) -> fixture::Options {
    fixture::Options {
        instance: instance.into(),
        token: Some("dev-token".into()),
        model: "deterministic".into(),
        principals: Vec::new(),
        settings: Vec::new(),
        token_scopes: vec!["*".into()],
    }
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client")
}

fn spawn_config(config: &Path, data: &Path, token: &str) -> Worker {
    let log = data.join("worker.log");
    let stderr = File::create(&log).expect("stderr log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .env("GOLDENBEE_DATA_DIR", data)
        .env("GOLDENBEE_SANDBOX_DIR", data.join("workspace"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
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
        .unwrap_or_else(|| panic!("no listener announcement: {line}"))
        .trim()
        .to_string();
    Worker {
        child,
        base,
        token: token.to_string(),
        data: data.to_path_buf(),
        config: config.to_path_buf(),
        log,
    }
}

fn start(data: &Path, options: fixture::Options) -> Worker {
    let fixture = fixture::write(data, options);
    spawn_config(&fixture.config, data, "dev-token")
}

fn start_on(data: &Path, instance: &str, drain_timeout_ms: u64) -> Worker {
    let fixture = fixture::write(data, options(instance));
    fixture::set_top_level(&fixture, &format!("drain_timeout_ms = {drain_timeout_ms}"));
    spawn_config(&fixture.config, data, "dev-token")
}

impl Worker {
    fn authed(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        client()
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
    }

    fn status(&self) -> Value {
        self.authed(reqwest::Method::GET, "/v1/status")
            .send()
            .expect("status responds")
            .json()
            .expect("status is JSON")
    }

    fn try_status(&self) -> Option<Value> {
        self.authed(reqwest::Method::GET, "/v1/status")
            .send()
            .ok()
            .filter(|response| response.status().is_success())
            .and_then(|response| response.json().ok())
    }

    fn submit(&self, input: &str, key: &str) -> (reqwest::StatusCode, Value) {
        let response = self
            .authed(reqwest::Method::POST, "/v1/tasks")
            .json(&serde_json::json!({
                "agent": "general",
                "session_id": format!("session-{key}"),
                "input": input,
                "idempotency_key": key,
            }))
            .send()
            .expect("submit responds");
        let status = response.status();
        let body = response.text().unwrap_or_default();
        (status, serde_json::from_str(&body).unwrap_or(Value::Null))
    }

    fn task(&self, id: &str) -> Value {
        self.authed(reqwest::Method::GET, &format!("/v1/tasks/{id}"))
            .send()
            .expect("task responds")
            .json()
            .expect("task is JSON")
    }

    fn events(&self, id: &str) -> Vec<Value> {
        let page: Value = self
            .authed(reqwest::Method::GET, &format!("/v1/tasks/{id}/events"))
            .send()
            .expect("events respond")
            .json()
            .expect("events JSON");
        page["events"].as_array().cloned().unwrap_or_default()
    }

    fn wait_state(&self, id: &str, wanted: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let task = self.task(id);
            if task["state"] == wanted {
                return task;
            }
            if matches!(
                task["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled")
            ) && wanted != task["state"].as_str().unwrap_or_default()
            {
                panic!("task reached {} before {wanted}: {task}", task["state"]);
            }
            assert!(
                Instant::now() < deadline,
                "task stayed at {} instead of {wanted}: {task}",
                task["state"]
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_phase(&self, wanted: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.try_status() {
                if status["phase"] == wanted {
                    return status;
                }
            }
            assert!(Instant::now() < deadline, "status did not reach {wanted}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn drain(&self) -> (reqwest::StatusCode, Value) {
        self.drain_as(&self.token)
    }

    fn drain_as(&self, token: &str) -> (reqwest::StatusCode, Value) {
        let response = client()
            .post(format!("{}/v1/drain", self.base))
            .bearer_auth(token)
            .send()
            .expect("drain responds");
        let status = response.status();
        let body = response.text().unwrap_or_default();
        (status, serde_json::from_str(&body).unwrap_or(Value::Null))
    }

    fn open_events(&self, task_id: &str) -> Receiver<String> {
        let url = format!("{}/v1/tasks/{}/events/stream", self.base, task_id);
        let token = self.token.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let response = client()
                .get(url)
                .bearer_auth(token)
                .send()
                .expect("stream connects");
            let mut reader = BufReader::new(response);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if let Some(name) = line.trim_end().strip_prefix("event:") {
                            if sender.send(name.trim().to_string()).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
            let _ = sender.send("<closed>".into());
        });
        receiver
    }

    fn signal_term(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
    }

    fn wait_exit(&mut self, budget: Duration) -> ExitStatus {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(status) = self.child.try_wait().expect("child status") {
                return status;
            }
            assert!(Instant::now() < deadline, "worker did not exit in time");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn stop(&mut self) -> ExitStatus {
        self.signal_term();
        self.wait_exit(Duration::from_secs(15))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn expect_event(events: &Receiver<String>, wanted: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(remaining) {
            Ok(event) if event == wanted => return,
            Ok(event) => assert_ne!(event, "<closed>", "stream closed before {wanted}"),
            Err(error) => panic!("stream did not deliver {wanted}: {error}"),
        }
    }
}

#[test]
fn status_separates_lifecycle_readiness_and_provider_health() {
    let data = tempfile::tempdir().expect("data");
    let mut worker = start_on(data.path(), "health-status", 10_000);

    let health: Value = client()
        .get(format!("{}/v1/health", worker.base))
        .send()
        .expect("liveness responds")
        .json()
        .expect("liveness JSON");
    assert_eq!(health["status"], "live");
    assert!(health.get("capabilities").is_none(), "liveness is minimal");
    assert!(
        health.get("ready").is_none(),
        "liveness carries no readiness"
    );

    let status = worker.status();
    assert_eq!(status["phase"], "ready");
    assert_eq!(status["lifecycle"], "ready");
    assert_eq!(status["ready"], true);
    assert_eq!(status["admission"], "open");
    assert_eq!(status["instance"], "health-status");
    assert_eq!(status["package"], "general");
    assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
    assert!(status["incarnation"]
        .as_str()
        .unwrap_or_default()
        .starts_with("inc-"));
    assert_eq!(status["config_revision"], 1);
    assert!(status["capabilities"]
        .as_array()
        .expect("capabilities")
        .iter()
        .any(|capability| capability == "general"));
    assert!(status["capacity"]["limit"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(status["subsystems"]["storage"], "ready");
    assert_eq!(
        status["subsystems"]["provider"], "unknown",
        "no provider probe exists, so readiness must not fabricate degradation"
    );
    assert_eq!(status["degraded"], serde_json::json!([]));
    let history: Vec<&str> = status["phase_history"]
        .as_array()
        .expect("phase history")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for expected in ["initializing", "recovering", "warming", "ready"] {
        assert!(
            history.contains(&expected),
            "phase history missing {expected}: {history:?}"
        );
    }

    // Provider failure is a task outcome, not a liveness or readiness change.
    let (accepted, submitted) = worker.submit("provider-fail now", "provider-failure");
    assert!(accepted.is_success(), "{accepted} {submitted}");
    let task = worker.wait_state(submitted["task_id"].as_str().unwrap(), "failed");
    assert_eq!(task["state"], "failed");

    let liveness = client()
        .get(format!("{}/v1/health", worker.base))
        .send()
        .expect("liveness still responds");
    assert_eq!(liveness.status(), reqwest::StatusCode::OK);
    let after = worker.status();
    assert_eq!(after["phase"], "ready");
    assert_eq!(after["ready"], true);
    assert_eq!(after["subsystems"]["provider"], "unknown");
    assert_eq!(after["incarnation"], status["incarnation"]);
    assert_eq!(after["config_revision"], status["config_revision"]);
    assert_eq!(after["capacity"]["active"], 0);

    let exit = worker.stop();
    assert!(exit.success(), "clean stop: {exit}");
}

#[test]
fn unauthorized_drain_is_rejected_without_changing_readiness() {
    let data = tempfile::tempdir().expect("data");
    let mut options = options("health-unauthorized");
    options
        .principals
        .push(("reader-token".into(), "local".into(), "reader".into()));
    let mut worker = start(data.path(), options);

    let (status, denied) = worker.drain_as("reader-token");
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "{denied}");
    assert_eq!(denied["code"], "scope.denied");

    let unauthenticated = client()
        .post(format!("{}/v1/drain", worker.base))
        .send()
        .expect("drain responds");
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let health = worker.status();
    assert_eq!(health["phase"], "ready");
    assert_eq!(health["ready"], true);
    assert_eq!(health["admission"], "open");

    let (accepted, submitted) = worker.submit("still admitted", "after-denied-drain");
    assert!(accepted.is_success(), "{accepted} {submitted}");
    let task = worker.wait_state(submitted["task_id"].as_str().unwrap(), "succeeded");
    assert_eq!(task["state"], "succeeded");

    worker.stop();
}

#[test]
fn drain_closes_admission_and_streams_and_leaves_forced_work_for_reconciliation() {
    let data = tempfile::tempdir().expect("data");
    let mut worker = start_on(data.path(), "health-drain", 250);

    let (accepted, submitted) = worker.submit("delay:5000 held work", "drain-held");
    assert!(accepted.is_success(), "{accepted} {submitted}");
    let task_id = submitted["task_id"].as_str().unwrap().to_string();
    worker.wait_state(&task_id, "running");

    let events = worker.open_events(&task_id);
    expect_event(&events, "task.running");

    let started = Instant::now();
    let (status, drained) = worker.drain();
    let elapsed = started.elapsed();
    assert!(status.is_success(), "{status} {drained}");
    assert_eq!(drained["phase"], "draining");
    assert_eq!(drained["ready"], false);
    assert_eq!(drained["admission"], "closed");
    assert_eq!(drained["timed_out"], true, "{drained}");
    assert_eq!(drained["forced_cancelled"], true, "{drained}");
    assert!(elapsed >= Duration::from_millis(200), "{elapsed:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "drain was not bounded: {elapsed:?}"
    );

    // New admission is refused with a retryable lifecycle error.
    let (rejected, error) = worker.submit("must not start", "drain-rejected");
    assert_eq!(
        rejected,
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "{error}"
    );
    assert_eq!(error["code"], "lifecycle.draining");
    assert_eq!(error["retryable"], true);
    assert!(error["recovery"]
        .as_str()
        .unwrap_or_default()
        .contains("draining"));

    let health = worker.status();
    assert_eq!(health["phase"], "draining");
    assert_eq!(health["ready"], false);
    assert_eq!(health["admission"], "closed");
    assert!(health["phase_history"]
        .as_array()
        .expect("phase history")
        .iter()
        .any(|phase| phase == "draining"));

    // An already connected viewer sees the drain event and then closure
    // instead of holding axum's graceful shutdown open.
    expect_event(&events, "stream.draining");
    expect_event(&events, "<closed>");

    let task = worker.wait_state(&task_id, "recovering");
    assert_eq!(task["state"], "recovering");
    assert!(
        task["output"].is_null(),
        "forced stop fabricated output: {task}"
    );
    assert!(
        task["error"].is_null(),
        "forced stop fabricated failure: {task}"
    );
    assert!(task["uncertainty"].is_string(), "{task}");

    let (status, repeated) = worker.drain();
    assert!(status.is_success(), "{status} {repeated}");
    assert_eq!(repeated["already_draining"], true);
    assert_eq!(repeated["phase"], "draining");

    let exit = worker.stop();
    assert_eq!(exit.code(), Some(1), "degraded stop must not look clean");

    // Restart exposes the unfinished work for reconciliation and does not
    // invent a successful resume.
    let mut restarted = spawn_config(&worker.config, &worker.data, "dev-token");
    let status = restarted.status();
    assert_eq!(status["phase"], "ready");
    assert_eq!(status["ready"], true);
    let task = restarted.task(&task_id);
    assert_eq!(task["state"], "recovering");
    assert!(task["uncertainty"].is_string());
    let events = restarted.events(&task_id);
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "task.recovery_required"),
        "{events:?}"
    );
    restarted.stop();
}

#[test]
fn drain_allows_admitted_work_to_complete_within_its_bound() {
    let data = tempfile::tempdir().expect("data");
    let mut worker = start_on(data.path(), "health-drain-complete", 5_000);

    let (accepted, submitted) = worker.submit("delay:150 bounded work", "drain-complete");
    assert!(accepted.is_success(), "{accepted} {submitted}");
    let task_id = submitted["task_id"].as_str().unwrap().to_string();
    worker.wait_state(&task_id, "running");

    let (status, drained) = worker.drain();
    assert!(status.is_success(), "{status} {drained}");
    assert_eq!(drained["timed_out"], false, "{drained}");
    assert_eq!(drained["forced_cancelled"], false, "{drained}");
    assert_eq!(drained["unfinished"], 0, "{drained}");
    let task = worker.task(&task_id);
    assert_eq!(task["state"], "succeeded");
    assert!(task["output"]
        .as_str()
        .unwrap_or_default()
        .contains("bounded work"));

    let (rejected, error) = worker.submit("must not start", "drain-complete-rejected");
    assert_eq!(rejected, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["code"], "lifecycle.draining");

    let exit = worker.stop();
    assert!(exit.success(), "clean bounded drain: {exit}");
}

#[test]
fn sigterm_drains_before_listener_close_without_waiting_for_long_work() {
    let data = tempfile::tempdir().expect("data");
    let mut worker = start_on(data.path(), "health-sigterm", 1_500);

    let (accepted, submitted) = worker.submit("delay:30000 long work", "sigterm-held");
    assert!(accepted.is_success(), "{accepted} {submitted}");
    let task_id = submitted["task_id"].as_str().unwrap().to_string();
    worker.wait_state(&task_id, "running");
    let events = worker.open_events(&task_id);
    expect_event(&events, "task.running");

    let started = Instant::now();
    worker.signal_term();
    // The listener remains reachable while draining, but readiness is false.
    let draining = worker.wait_phase("draining");
    assert_eq!(draining["ready"], false);
    assert_eq!(draining["admission"], "closed");

    let exit = worker.wait_exit(Duration::from_secs(10));
    let elapsed = started.elapsed();
    assert_eq!(exit.code(), Some(1), "bounded forced stop is not clean");
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    assert!(elapsed >= Duration::from_millis(1_000), "{elapsed:?}");
    expect_event(&events, "stream.draining");

    // The listener is gone, not merely idle.
    let refused = client().get(format!("{}/v1/health", worker.base)).send();
    assert!(refused.is_err(), "listener still accepted a connection");

    // The forced attempt is uncertain on the next start.
    let mut restarted = spawn_config(&worker.config, &worker.data, "dev-token");
    let task = restarted.task(&task_id);
    assert_eq!(task["state"], "recovering");
    assert!(task["uncertainty"].is_string());
    restarted.stop();
}

#[test]
fn shutdown_hook_failure_cannot_skip_plugin_close_or_report_clean_stop() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "health-hook-failure".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: vec![
                ("shutdown_fail".into(), "true".into()),
                ("shutdown_delay_ms".into(), "50".into()),
            ],
            token_scopes: vec!["*".into()],
        },
    );
    let mut worker = spawn_config(&fixture.config, data.path(), "dev-token");
    let (accepted, submitted) = worker.submit("short work", "hook-failure-task");
    assert!(accepted.is_success(), "{accepted} {submitted}");
    worker.wait_state(submitted["task_id"].as_str().unwrap(), "succeeded");

    let exit = worker.stop();
    assert_eq!(exit.code(), Some(1), "failed shutdown hook exited cleanly");
    let log = worker.log();
    assert_eq!(
        log.matches("hook:shutdown").count(),
        1,
        "shutdown hook must run once: {log}"
    );
    assert!(
        log.contains("plugin:closed"),
        "plugin close was skipped: {log}"
    );
    assert!(log.contains("shutdown: package hook failed"), "{log}");
    assert!(log.contains("stopped_degraded"), "{log}");
    assert!(!log.contains("sk-shutdown-secret"), "secret leaked: {log}");
}

#[test]
fn startup_hook_failure_never_reports_readiness_or_binds_a_listener() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "health-startup-failure".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: vec![("startup_fail".into(), "true".into())],
            token_scopes: vec!["*".into()],
        },
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            fixture.config.to_str().unwrap(),
            "--addr",
            "127.0.0.1:0",
        ])
        .env("GOLDENBEE_DATA_DIR", data.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("worker starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("child status") {
            break status;
        }
        assert!(Instant::now() < deadline, "startup failure hung");
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut stdout)
        .expect("stdout text");
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("stderr text");
    assert_eq!(status.code(), Some(1), "{stdout}\n{stderr}");
    assert!(!stdout.contains("listening on"), "{stdout}");
    assert!(stderr.contains("policy.startup_failed"), "{stderr}");
    assert!(!stderr.contains("sk-startup-secret"), "{stderr}");
}

#[test]
fn shutdown_terminates_owned_sandbox_children() {
    let data = tempfile::tempdir().expect("data");
    let mut worker = start_on(data.path(), "health-child-cleanup", 200);
    let marker = format!("gb-drain-child-{}", worker.child.id());
    let (accepted, submitted) = worker.submit(
        &format!("sandbox:/bin/bash -c 'exec -a {marker} sleep 300'"),
        "child-cleanup",
    );
    assert!(accepted.is_success(), "{accepted} {submitted}");
    let task_id = submitted["task_id"].as_str().unwrap();
    worker.wait_state(task_id, "running");

    let deadline = Instant::now() + Duration::from_secs(10);
    while processes_containing(&marker).is_empty() {
        assert!(Instant::now() < deadline, "sandbox child never started");
        std::thread::sleep(Duration::from_millis(20));
    }

    let _exit = worker.stop();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = processes_containing(&marker);
        if remaining.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sandbox child survived Worker shutdown: {remaining:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_shutdown_runs_hook_and_cleanup_once() {
    let data = tempfile::tempdir().expect("data");
    let marker = data.path().join("shutdown-marker.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "health-repeated-shutdown".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: vec![("shutdown_marker".into(), marker.display().to_string())],
            token_scopes: vec!["*".into()],
        },
    );
    let instance = general::worker()
        .expect("package composes")
        .start(&fixture.config)
        .await
        .expect("instance starts");
    let first = instance.shutdown().await;
    let second = instance.shutdown().await;
    assert!(first.clean, "{first:?}");
    assert_eq!(first.phase, "stopped");
    assert!(!first.repeated);
    assert!(second.repeated, "{second:?}");
    assert_eq!(second.phase, "stopped");
    let marker = std::fs::read_to_string(&marker).expect("shutdown marker");
    assert_eq!(marker.matches("shutdown").count(), 1, "{marker}");
}

fn processes_containing(marker: &str) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if cmdline
            .split(|byte| *byte == 0)
            .any(|argument| argument == marker.as_bytes())
        {
            found.push(pid);
        }
    }
    found
}

#[test]
fn independent_instances_drain_independently_and_dev_launcher_shuts_down() {
    let first_data = tempfile::tempdir().expect("first data");
    let second_data = tempfile::tempdir().expect("second data");

    let first_fixture = fixture::write(first_data.path(), options("instance-a"));
    fixture::set_top_level(&first_fixture, "max_active = 1");
    let second_fixture = fixture::write(second_data.path(), options("instance-b"));
    fixture::set_top_level(&second_fixture, "max_active = 2");
    fixture::set_top_level(&second_fixture, "config_revision = 7");

    let mut first = spawn_config(&first_fixture.config, first_data.path(), "dev-token");
    let mut second = spawn_config(&second_fixture.config, second_data.path(), "dev-token");

    let first_status = first.status();
    let second_status = second.status();
    assert_eq!(first_status["instance"], "instance-a");
    assert_eq!(second_status["instance"], "instance-b");
    assert_eq!(first_status["package"], "general");
    assert_eq!(second_status["package"], "general");
    assert_eq!(first_status["capacity"]["limit"], 1);
    assert_eq!(second_status["capacity"]["limit"], 2);
    assert_eq!(second_status["config_revision"], 7);
    assert_ne!(first_status["incarnation"], second_status["incarnation"]);

    let (status, drained) = first.drain();
    assert!(status.is_success(), "{drained}");
    assert_eq!(drained["phase"], "draining");

    let (rejected, error) = first.submit("blocked", "instance-a-blocked");
    assert_eq!(rejected, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["code"], "lifecycle.draining");

    let (accepted, submitted) = second.submit("independent work", "instance-b-work");
    assert!(accepted.is_success(), "{accepted} {submitted}");
    let task = second.wait_state(submitted["task_id"].as_str().unwrap(), "succeeded");
    assert_eq!(task["state"], "succeeded");
    assert_eq!(second.status()["ready"], true);

    first.stop();
    second.stop();

    // The same package is independently startable and drainable in native
    // Launcher development mode with no Gateway configuration.
    let dev_data = tempfile::tempdir().expect("dev data");
    let dev_fixture = fixture::write(dev_data.path(), options("dev-launcher"));
    let dev_log = dev_data.path().join("dev.log");
    let stderr = File::create(&dev_log).expect("dev log");
    let mut dev = Command::new(env!("CARGO_BIN_EXE_general"))
        .args(["dev", "--config", dev_fixture.config.to_str().unwrap()])
        .env("GOLDENBEE_DATA_DIR", dev_data.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("dev launcher starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let log = std::fs::read_to_string(&dev_log).unwrap_or_default();
        if log.contains("hook:startup") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "dev launcher did not reach startup: {log}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = Command::new("kill")
        .args(["-TERM", &dev.id().to_string()])
        .status();
    let status = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = dev.try_wait().expect("dev status") {
                break status;
            }
            assert!(Instant::now() < deadline, "dev launcher did not drain");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let log = std::fs::read_to_string(&dev_log).unwrap_or_default();
    assert!(status.success(), "dev launcher failed: {log}");
    assert!(log.contains("hook:shutdown"), "dev shutdown skipped: {log}");
    assert!(
        log.contains("plugin:closed"),
        "dev plugin close skipped: {log}"
    );
}
