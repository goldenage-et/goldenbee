//! Cross-process admission and cancellation evidence through the General
//! Worker's public HTTP control surface.
//!
//! These tests exercise finite capacity, per-principal quotas, queued and
//! running cancellation, foreign-cancellation denial, and permit release. They
//! use the real native ADK Runner with the deterministic model; no harness
//! internals are inspected.

use common::{Limits, SubmitTaskRequest};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Worker {
    child: Child,
    _data: tempfile::TempDir,
    base: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn env_value<'a>(env: &'a [(&str, String)], name: &str) -> Option<&'a str> {
    env.iter()
        .rev()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.as_str())
}

fn config_for(data: &Path, token: &str, env: &[(&str, String)]) -> std::path::PathBuf {
    let assets = data.join("assets");
    let agents = assets.join("agents");
    let prompts = assets.join("prompts");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::write(
        prompts.join("general.md"),
        "You are the Goldenbee General Worker. Answer usefully and concisely.",
    )
    .expect("general prompt");
    std::fs::write(
        prompts.join("echo.md"),
        "Repeat the user's message. Keep it short.",
    )
    .expect("echo prompt");
    let model = env_value(env, "GOLDENBEE_MODEL").unwrap_or("deterministic");
    std::fs::write(
        agents.join("general.toml"),
        format!(
            "name = \"general\"\ndescription = \"General Worker\"\nmodel = \"{model}\"\nprompt = \"general\"\n"
        ),
    )
    .expect("general agent");
    std::fs::write(
        agents.join("echo.toml"),
        format!(
            "name = \"echo\"\ndescription = \"Echo Worker\"\nmodel = \"{model}\"\nprompt = \"echo\"\n"
        ),
    )
    .expect("echo agent");

    let mut config = format!(
        "instance = \"queue-fixture-{}\"\nnamespace = \"local\"\ndefault_agent = \"general\"\ntoken = \"{token}\"\ntoken_scopes = [\"*\"]\n",
        data.file_name().unwrap().to_string_lossy()
    );
    for (name, field) in [
        ("GOLDENBEE_MAX_ACTIVE", "max_active"),
        ("GOLDENBEE_MAX_QUEUE", "max_queue"),
        ("GOLDENBEE_PER_PRINCIPAL", "per_principal"),
    ] {
        if let Some(value) = env_value(env, name) {
            config.push_str(&format!("{field} = {value}\n"));
        }
    }
    if let Some(raw) = env_value(env, "GOLDENBEE_PRINCIPALS") {
        let bindings: Vec<Value> = serde_json::from_str(raw).expect("principals are JSON");
        for binding in bindings {
            let scopes = binding["scopes"]
                .as_array()
                .map(|scopes| {
                    scopes
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|scope| format!("\"{scope}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_else(|| "\"*\"".into());
            config.push_str(&format!(
                "\n[[tokens]]\ntoken = \"{}\"\nnamespace = \"{}\"\nprincipal = \"{}\"\nscopes = [{scopes}]\n",
                binding["token"].as_str().expect("token"),
                binding["namespace"].as_str().expect("namespace"),
                binding["principal"].as_str().expect("principal"),
            ));
        }
    }
    config.push_str(&format!(
        "\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n",
        data.display()
    ));
    let path = data.join("worker.toml");
    std::fs::write(&path, config).expect("worker config");
    path
}

fn spawn_worker(data: &Path, token: &str, env: &[(&str, String)]) -> (Child, String) {
    let config = config_for(data, token, env);
    let mut command = Command::new(env!("CARGO_BIN_EXE_general"));
    command
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in env {
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
    (child, base)
}

impl Worker {
    fn start_with(token: &str, env: &[(&str, String)]) -> Self {
        let data = tempfile::tempdir().expect("temporary data directory");
        let (child, base) = spawn_worker(data.path(), token, env);
        Self {
            child,
            _data: data,
            base,
        }
    }

    fn client(&self) -> reqwest::blocking::Client {
        reqwest::blocking::Client::new()
    }

    fn request(session: &str, input: &str, key: &str) -> SubmitTaskRequest {
        SubmitTaskRequest {
            namespace: None,
            agent: "general".into(),
            session_id: session.into(),
            input: input.into(),
            idempotency_key: key.into(),
            profile: None,
            team: None,
            worker: None,
            limits: Limits::default(),
        }
    }

    fn post_task(&self, token: &str, body: &SubmitTaskRequest) -> reqwest::blocking::Response {
        self.client()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(token)
            .json(body)
            .send()
            .expect("submit request is attempted")
    }

    fn submit(&self, token: &str, session: &str, input: &str, key: &str) -> Value {
        let response = self.post_task(token, &Self::request(session, input, key));
        let status = response.status();
        let body = response.text().expect("submit body");
        assert!(status.is_success(), "submit failed: {status} {body}");
        serde_json::from_str(&body).expect("submit is JSON")
    }

    fn task(&self, token: &str, id: &str) -> Value {
        let response = self
            .client()
            .get(format!("{}/v1/tasks/{id}", self.base))
            .bearer_auth(token)
            .send()
            .expect("task request is attempted");
        let status = response.status();
        let body = response.text().expect("task body");
        assert!(status.is_success(), "task read failed: {status} {body}");
        serde_json::from_str(&body).expect("task is JSON")
    }

    fn cancel(&self, token: &str, id: &str) -> reqwest::blocking::Response {
        self.client()
            .post(format!("{}/v1/tasks/{id}/cancel", self.base))
            .bearer_auth(token)
            .send()
            .expect("cancel request is attempted")
    }

    fn status(&self, token: &str) -> Value {
        let response = self
            .client()
            .get(format!("{}/v1/status", self.base))
            .bearer_auth(token)
            .send()
            .expect("status responds");
        assert!(response.status().is_success(), "status read failed");
        response.json().expect("status is JSON")
    }

    fn wait(&self, token: &str, id: &str) -> Value {
        self.wait_for(token, id, |task| {
            matches!(
                task["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled" | "unknown")
            )
        })
    }

    fn wait_for(&self, token: &str, id: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(15) {
            let task = self.task(token, id);
            if predicate(&task) {
                return task;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "task {id} did not reach the expected state: {:?}",
            self.task(token, id)
        );
    }

    fn wait_capacity(&self, token: &str, active: u64, queued: u64) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(15) {
            let status = self.status(token);
            if status["capacity"]["active"].as_u64() == Some(active)
                && status["capacity"]["queued"].as_u64() == Some(queued)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "capacity did not reach active={active} queued={queued}: {}",
            self.status(token)
        );
    }

    fn run(&self, token: &str, session: &str, input: &str, key: &str) -> Value {
        let submitted = self.submit(token, session, input, key);
        self.wait(token, submitted["task_id"].as_str().expect("task id"))
    }
}

fn state(task: &Value) -> &str {
    task["state"].as_str().unwrap_or_default()
}

fn output(task: &Value) -> String {
    task["output"].as_str().unwrap_or_default().to_string()
}

fn event_kinds(worker: &Worker, token: &str, id: &str) -> Vec<String> {
    let page: Value = worker
        .client()
        .get(format!("{}/v1/tasks/{id}/events", worker.base))
        .bearer_auth(token)
        .send()
        .expect("events respond")
        .json()
        .expect("events page is JSON");
    page["events"]
        .as_array()
        .expect("events array")
        .iter()
        .map(|event| event["kind"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// A finite queue rejects excess work with explicit retry guidance. Lowering
/// the active limit through operator configuration changes observable capacity,
/// and completed work restores both counters.
#[test]
fn finite_admission_backpressures_and_recovers() {
    let worker = Worker::start_with(
        "dev-token",
        &[
            ("GOLDENBEE_MAX_ACTIVE", "1".into()),
            ("GOLDENBEE_MAX_QUEUE", "1".into()),
            ("GOLDENBEE_PER_PRINCIPAL", "8".into()),
        ],
    );
    assert_eq!(worker.status("dev-token")["capacity"]["limit"], 1);
    assert_eq!(worker.status("dev-token")["capacity"]["queue_limit"], 1);

    let first = worker.submit("dev-token", "queue-a", "delay:600 first", "queue-1");
    let first_id = first["task_id"].as_str().expect("task id").to_string();
    worker.wait_capacity("dev-token", 1, 0);

    let second = worker.submit("dev-token", "queue-b", "delay:100 second", "queue-2");
    let second_id = second["task_id"].as_str().expect("task id").to_string();
    worker.wait_for("dev-token", &second_id, |task| state(task) == "queued");
    assert_eq!(
        worker.task("dev-token", &first_id)["state"],
        "running",
        "the only active slot belongs to the first task"
    );

    let excess = worker.post_task(
        "dev-token",
        &Worker::request("queue-c", "delay:100 third", "queue-3"),
    );
    assert_eq!(excess.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let error: Value = excess.json().expect("error is JSON");
    assert_eq!(error["code"], "capacity.queue_full");
    assert_eq!(error["retryable"], true);
    assert!(
        error["recovery"]
            .as_str()
            .is_some_and(|recovery| recovery.to_ascii_lowercase().contains("wait")),
        "backpressure needs wait guidance: {error}"
    );
    assert!(
        error["details"]["retry_after_ms"]
            .as_str()
            .is_some_and(|value| value.parse::<u64>().is_ok_and(|value| value > 0)),
        "backpressure needs a retry interval: {error}"
    );

    assert_eq!(state(&worker.wait("dev-token", &first_id)), "succeeded");
    assert_eq!(state(&worker.wait("dev-token", &second_id)), "succeeded");
    worker.wait_capacity("dev-token", 0, 0);

    let after = worker.run("dev-token", "queue-d", "after capacity", "queue-4");
    assert_eq!(state(&after), "succeeded");
    assert!(output(&after).contains("after capacity"));
    worker.wait_capacity("dev-token", 0, 0);
}

/// Separate sessions execute concurrently when capacity allows. A global
/// execution lock would serialize them even though no session is shared.
#[test]
fn independent_sessions_overlap_without_a_global_execution_lock() {
    let (endpoint, observed) = slow_model(Duration::from_millis(350));
    let worker = Worker::start_with(
        "dev-token",
        &[
            ("GOLDENBEE_MODEL", format!("http+{endpoint}")),
            ("GOLDENBEE_MAX_ACTIVE", "2".into()),
            ("GOLDENBEE_MAX_QUEUE", "2".into()),
            ("GOLDENBEE_PER_PRINCIPAL", "4".into()),
        ],
    );
    let started = Instant::now();
    let first = worker.submit("dev-token", "overlap-a", "one", "overlap-1");
    let second = worker.submit("dev-token", "overlap-b", "two", "overlap-2");
    let first = worker.wait("dev-token", first["task_id"].as_str().unwrap());
    let second = worker.wait("dev-token", second["task_id"].as_str().unwrap());
    let elapsed = started.elapsed();

    assert_eq!(state(&first), "succeeded");
    assert_eq!(state(&second), "succeeded");
    assert_eq!(output(&first), "slow-ok");
    assert_eq!(output(&second), "slow-ok");
    assert!(
        observed.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "independent sessions must reach the native model concurrently"
    );
    assert!(
        elapsed < Duration::from_millis(650),
        "two 350ms sessions should overlap, took {elapsed:?}"
    );
    worker.wait_capacity("dev-token", 0, 0);
}

/// A daily-style model endpoint that records concurrent requests. The native
/// Runner remains the system under test; the endpoint only controls latency.
fn slow_model(delay: Duration) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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

/// Per-principal quota bounds one caller without consuming another caller's
/// capacity. Both callers are authenticated against the same Worker Instance.
#[test]
fn per_principal_quota_does_not_block_another_caller() {
    let principals = serde_json::json!([
        {"token": "mallory", "namespace": "local", "principal": "mallory",
         "scopes": ["task:submit", "task:read", "task:cancel"]}
    ])
    .to_string();
    let worker = Worker::start_with(
        "dev-token",
        &[
            ("GOLDENBEE_PRINCIPALS", principals),
            ("GOLDENBEE_MAX_ACTIVE", "2".into()),
            ("GOLDENBEE_MAX_QUEUE", "4".into()),
            ("GOLDENBEE_PER_PRINCIPAL", "1".into()),
        ],
    );

    let first = worker.submit("dev-token", "quota-a", "delay:500 first", "quota-1");
    worker.wait_for("dev-token", first["task_id"].as_str().unwrap(), |task| {
        state(task) == "running"
    });

    let excess = worker.post_task(
        "dev-token",
        &Worker::request("quota-b", "delay:100 second", "quota-2"),
    );
    assert_eq!(excess.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let error: Value = excess.json().expect("error is JSON");
    assert_eq!(error["code"], "capacity.principal_quota");
    assert_eq!(error["retryable"], true);
    assert!(
        error["details"]["retry_after_ms"]
            .as_str()
            .is_some_and(|value| value.parse::<u64>().is_ok_and(|value| value > 0)),
        "quota backpressure needs a retry interval: {error}"
    );

    // The other principal has its own quota and proceeds immediately.
    let other = worker.submit("mallory", "quota-c", "delay:100 other", "quota-3");
    let other = worker.wait("mallory", other["task_id"].as_str().unwrap());
    assert_eq!(state(&other), "succeeded");
    assert!(output(&other).contains("other"));

    let first = worker.wait("dev-token", first["task_id"].as_str().unwrap());
    assert_eq!(state(&first), "succeeded");
    worker.wait_capacity("dev-token", 0, 0);
}

/// Cancelling queued work releases its admission place without ever consuming
/// an execution slot. The worker remains able to admit and run later work.
#[test]
fn queued_cancellation_releases_admission() {
    let worker = Worker::start_with(
        "dev-token",
        &[
            ("GOLDENBEE_MAX_ACTIVE", "1".into()),
            ("GOLDENBEE_MAX_QUEUE", "2".into()),
            ("GOLDENBEE_PER_PRINCIPAL", "8".into()),
        ],
    );
    let first = worker.submit("dev-token", "cancel-hold", "delay:700 hold", "cancel-1");
    let first_id = first["task_id"].as_str().expect("task id").to_string();
    worker.wait_capacity("dev-token", 1, 0);

    let queued = worker.submit("dev-token", "cancel-queued", "delay:100 queued", "cancel-2");
    let queued_id = queued["task_id"].as_str().expect("task id").to_string();
    worker.wait_for("dev-token", &queued_id, |task| state(task) == "queued");

    let cancelled = worker.cancel("dev-token", &queued_id);
    assert!(cancelled.status().is_success(), "queued cancel failed");
    let cancelled: Value = cancelled.json().expect("cancel is JSON");
    assert_eq!(state(&cancelled), "cancelled");

    // Repeated cancellation of terminal work is idempotent.
    let repeated = worker.cancel("dev-token", &queued_id);
    assert!(repeated.status().is_success());
    assert_eq!(
        state(&repeated.json::<Value>().expect("cancel JSON")),
        "cancelled"
    );

    let kinds = event_kinds(&worker, "dev-token", &queued_id);
    assert!(
        kinds
            .iter()
            .any(|kind| kind == "task.cancellation_requested"),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "task.cancelled"),
        "{kinds:?}"
    );

    assert_eq!(state(&worker.wait("dev-token", &first_id)), "succeeded");
    worker.wait_capacity("dev-token", 0, 0);

    let later = worker.run("dev-token", "cancel-after", "later work", "cancel-3");
    assert_eq!(state(&later), "succeeded");
    worker.wait_capacity("dev-token", 0, 0);
}

/// Cancellation is scoped to the authenticated task. A peer with the same
/// session id in another namespace keeps its execution slot and finishes.
#[test]
fn running_cancellation_does_not_interrupt_a_same_named_peer_namespace() {
    let principals = serde_json::json!([
        {"token": "other-ns", "namespace": "other", "principal": "local",
         "scopes": ["task:submit", "task:read", "task:cancel"]}
    ])
    .to_string();
    let worker = Worker::start_with(
        "dev-token",
        &[
            ("GOLDENBEE_PRINCIPALS", principals),
            ("GOLDENBEE_MAX_ACTIVE", "2".into()),
            ("GOLDENBEE_MAX_QUEUE", "2".into()),
            ("GOLDENBEE_PER_PRINCIPAL", "4".into()),
        ],
    );

    let local = worker.submit("dev-token", "shared", "delay:900 local", "scope-1");
    let local_id = local["task_id"].as_str().expect("task id").to_string();
    let peer = worker.submit("other-ns", "shared", "delay:700 peer work", "scope-2");
    let peer_id = peer["task_id"].as_str().expect("task id").to_string();
    worker.wait_for("dev-token", &local_id, |task| state(task) == "running");
    worker.wait_for("other-ns", &peer_id, |task| state(task) == "running");

    let cancelled = worker.cancel("dev-token", &local_id);
    assert!(cancelled.status().is_success());
    let cancelled = worker.wait("dev-token", &local_id);
    assert_eq!(state(&cancelled), "cancelled");

    let peer = worker.wait("other-ns", &peer_id);
    assert_eq!(state(&peer), "succeeded");
    assert!(output(&peer).contains("peer work"), "{}", output(&peer));
    worker.wait_capacity("dev-token", 0, 0);
}

/// Another caller or namespace cannot cancel a task it does not own. The
/// original work remains unaffected and completes normally.
#[test]
fn foreign_cancellation_is_denied() {
    let principals = serde_json::json!([
        {"token": "mallory", "namespace": "local", "principal": "mallory",
         "scopes": ["task:submit", "task:read", "task:cancel"]},
        {"token": "other-ns", "namespace": "other", "principal": "local",
         "scopes": ["task:submit", "task:read", "task:cancel"]}
    ])
    .to_string();
    let worker = Worker::start_with(
        "dev-token",
        &[
            ("GOLDENBEE_PRINCIPALS", principals),
            ("GOLDENBEE_MAX_ACTIVE", "2".into()),
            ("GOLDENBEE_MAX_QUEUE", "2".into()),
            ("GOLDENBEE_PER_PRINCIPAL", "4".into()),
        ],
    );
    let task = worker.submit("dev-token", "owned", "delay:600 owned", "foreign-1");
    let id = task["task_id"].as_str().expect("task id").to_string();
    worker.wait_for("dev-token", &id, |task| state(task) == "running");

    let same_namespace = worker.cancel("mallory", &id);
    assert_eq!(same_namespace.status(), reqwest::StatusCode::FORBIDDEN);
    let error: Value = same_namespace.json().expect("error is JSON");
    assert_eq!(error["code"], "scope.task_owner");

    let other_namespace = worker.cancel("other-ns", &id);
    assert_eq!(other_namespace.status(), reqwest::StatusCode::FORBIDDEN);
    let error: Value = other_namespace.json().expect("error is JSON");
    assert_eq!(error["code"], "scope.namespace_denied");

    let finished = worker.wait("dev-token", &id);
    assert_eq!(
        state(&finished),
        "succeeded",
        "denied cancels must not stop the task"
    );
    worker.wait_capacity("dev-token", 0, 0);
}

/// Cancellation that races admission, before the task has a native invocation,
/// still becomes terminal without stealing the slot held by other work.
#[test]
fn cancellation_at_the_admission_boundary_is_precise() {
    let worker = Worker::start_with(
        "dev-token",
        &[
            ("GOLDENBEE_MAX_ACTIVE", "1".into()),
            ("GOLDENBEE_MAX_QUEUE", "4".into()),
            ("GOLDENBEE_PER_PRINCIPAL", "8".into()),
        ],
    );
    let blocker = worker.submit("dev-token", "race-blocker", "delay:1200 blocker", "race-0");
    let blocker_id = blocker["task_id"].as_str().expect("task id").to_string();
    worker.wait_capacity("dev-token", 1, 0);

    for index in 1..=4 {
        let candidate = worker.submit(
            "dev-token",
            &format!("race-{index}"),
            "queued candidate",
            &format!("race-{index}"),
        );
        let id = candidate["task_id"].as_str().expect("task id").to_string();
        // Cancels immediately, racing the spawned admission task.
        let response = worker.cancel("dev-token", &id);
        assert!(response.status().is_success(), "race cancel failed");
        let cancelled = worker.wait("dev-token", &id);
        assert_eq!(state(&cancelled), "cancelled");
        assert!(
            worker.task("dev-token", &blocker_id)["state"] == "running",
            "cancelling queued work must not cancel the active task"
        );
        worker.wait_capacity("dev-token", 1, 0);
    }

    assert_eq!(state(&worker.wait("dev-token", &blocker_id)), "succeeded");
    worker.wait_capacity("dev-token", 0, 0);
}

/// Terminal outcomes: timeout, provider failure, and normal completion all
/// return the execution permit. A leak would leave the only slot held.
#[test]
fn terminal_outcomes_restore_the_active_count() {
    let worker = Worker::start_with(
        "dev-token",
        &[
            ("GOLDENBEE_MAX_ACTIVE", "1".into()),
            ("GOLDENBEE_MAX_QUEUE", "2".into()),
            ("GOLDENBEE_PER_PRINCIPAL", "4".into()),
        ],
    );

    let mut timed_out = Worker::request("release-timeout", "delay:900 timeout", "release-1");
    timed_out.limits = Limits {
        max_duration_ms: 100,
        ..Limits::default()
    };
    let response = worker.post_task("dev-token", &timed_out);
    assert!(response.status().is_success());
    let response: Value = response.json().expect("submit is JSON");
    let terminal = worker.wait("dev-token", response["task_id"].as_str().unwrap());
    assert_eq!(state(&terminal), "failed");
    assert_eq!(terminal["error"]["code"], "budget.deadline_exceeded");
    worker.wait_capacity("dev-token", 0, 0);

    let failed = worker.run(
        "dev-token",
        "release-provider",
        "provider-fail",
        "release-2",
    );
    assert_eq!(state(&failed), "failed");
    worker.wait_capacity("dev-token", 0, 0);

    let succeeded = worker.run("dev-token", "release-normal", "normal work", "release-3");
    assert_eq!(state(&succeeded), "succeeded");
    assert!(output(&succeeded).contains("normal work"));
    worker.wait_capacity("dev-token", 0, 0);

    // One more task proves the permit is genuinely available, not merely that
    // the counter was decremented.
    let after = worker.run("dev-token", "release-after", "after release", "release-4");
    assert_eq!(state(&after), "succeeded");
}

/// Admission ceilings are process-instance state. Two Worker Instances each
/// admit their own active task even when both have a limit of one.
#[test]
fn admission_limits_are_isolated_per_worker_instance() {
    let first = Worker::start_with(
        "dev-token",
        &[
            ("GOLDENBEE_MAX_ACTIVE", "1".into()),
            ("GOLDENBEE_MAX_QUEUE", "1".into()),
        ],
    );
    let second = Worker::start_with(
        "dev-token",
        &[
            ("GOLDENBEE_MAX_ACTIVE", "1".into()),
            ("GOLDENBEE_MAX_QUEUE", "1".into()),
        ],
    );

    let first_task = first.submit(
        "dev-token",
        "instance-first",
        "delay:500 first",
        "instance-1",
    );
    let second_task = second.submit(
        "dev-token",
        "instance-second",
        "delay:500 second",
        "instance-2",
    );
    first.wait_capacity("dev-token", 1, 0);
    second.wait_capacity("dev-token", 1, 0);

    assert_eq!(
        state(&first.wait("dev-token", first_task["task_id"].as_str().unwrap())),
        "succeeded"
    );
    assert_eq!(
        state(&second.wait("dev-token", second_task["task_id"].as_str().unwrap())),
        "succeeded"
    );
    first.wait_capacity("dev-token", 0, 0);
    second.wait_capacity("dev-token", 0, 0);
}
