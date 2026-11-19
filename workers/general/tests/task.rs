//! Cross-process task evidence: durable idempotent handles, caller-scoped
//! denial, restart durability, acceptance boundaries, and native execution
//! mapping through the General Worker.

use common::{Limits, SubmitTaskRequest};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
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

    /// Wait for the worker to exit because an injected fault fired.
    fn wait_for_fault_exit(&mut self) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            match self.child.try_wait().expect("worker status is observable") {
                Some(status) => {
                    assert!(
                        !status.success(),
                        "the injected fault must terminate the process"
                    );
                    return;
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("the injected fault did not terminate the worker");
    }

    fn client(&self) -> reqwest::blocking::Client {
        reqwest::blocking::Client::new()
    }

    fn post_task(
        &self,
        token: &str,
        body: &SubmitTaskRequest,
    ) -> Result<reqwest::blocking::Response, reqwest::Error> {
        self.client()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(token)
            .json(body)
            .send()
    }

    fn submit(&self, token: &str, session: &str, input: &str, key: &str) -> Value {
        let response = self
            .post_task(token, &request(session, input, key))
            .expect("submit request is attempted");
        let status = response.status();
        let body = response.text().expect("submit body");
        assert!(status.is_success(), "submit failed: {status} {body}");
        serde_json::from_str(&body).expect("submit is JSON")
    }

    fn try_task(&self, token: &str, id: &str) -> reqwest::blocking::Response {
        self.client()
            .get(format!("{}/v1/tasks/{id}", self.base))
            .bearer_auth(token)
            .send()
            .expect("task request is attempted")
    }

    fn task(&self, token: &str, id: &str) -> Value {
        let response = self.try_task(token, id);
        let status = response.status();
        let body = response.text().expect("task body");
        assert!(status.is_success(), "task read failed: {status} {body}");
        serde_json::from_str(&body).expect("task is JSON")
    }

    fn events(&self, token: &str, id: &str) -> Value {
        let response = self
            .client()
            .get(format!("{}/v1/tasks/{id}/events", self.base))
            .bearer_auth(token)
            .send()
            .expect("events respond");
        assert!(response.status().is_success(), "events read failed");
        response.json().expect("events page is JSON")
    }

    fn list(&self, token: &str) -> Vec<Value> {
        self.client()
            .get(format!("{}/v1/tasks", self.base))
            .bearer_auth(token)
            .send()
            .expect("task list responds")
            .json::<Vec<Value>>()
            .expect("task list is JSON")
    }

    fn status(&self, token: &str, path: &str) -> reqwest::StatusCode {
        self.client()
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .expect("request responds")
            .status()
    }

    fn wait(&self, token: &str, id: &str) -> Value {
        for _ in 0..500 {
            let task = self.task(token, id);
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
}

fn output(task: &Value) -> String {
    task["output"].as_str().unwrap_or_default().to_string()
}

fn event_kinds(page: &Value) -> Vec<String> {
    page["events"]
        .as_array()
        .expect("events array")
        .iter()
        .map(|event| event["kind"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Acknowledgment is only safe after acceptance metadata is durable. The
/// process is terminated between the durable commit and the HTTP response; a
/// retry after restart must recover the same handle and exactly one task.
#[test]
fn acceptance_is_durable_before_it_is_acknowledged() {
    let faults = tempfile::tempdir().expect("fault directory");
    let fault_dir = faults.path().display().to_string();
    let mut worker = Worker::start_with("dev-token", &[("GOLDENBEE_FAULT_DIR", fault_dir.clone())]);
    std::fs::write(faults.path().join("before-acknowledge"), b"once")
        .expect("fault marker is writable");

    // No HTTP response can be trusted here: the process dies after committing.
    let _ = worker.post_task("dev-token", &request("ack-session", "ack me", "ack-key"));
    worker.wait_for_fault_exit();
    worker.restart();

    let tasks = worker.list("dev-token");
    assert_eq!(tasks.len(), 1, "the committed task must survive: {tasks:?}");
    let id = tasks[0]["id"].as_str().expect("task id").to_string();

    // Recovery resumes an accepted-but-never-dispatched task.
    let recovered = worker.wait("dev-token", &id);
    assert_eq!(recovered["state"], "succeeded");
    assert!(output(&recovered).contains("ack me"));

    let retried = worker.submit("dev-token", "ack-session", "ack me", "ack-key");
    assert_eq!(retried["task_id"], id);
    assert_eq!(retried["replayed"], true);
    assert_eq!(worker.list("dev-token").len(), 1);
}

/// A key is scoped to caller and namespace. Changed payloads conflict without
/// overwriting the accepted request, and another caller with the same key gets
/// its own handle instead of the original task.
#[test]
fn idempotency_is_payload_checked_and_caller_scoped_across_restart() {
    let principals = serde_json::json!([
        {"token": "mallory", "namespace": "local", "principal": "mallory",
         "scopes": ["task:submit", "task:read"]}
    ])
    .to_string();
    let mut worker = Worker::start_with("dev-token", &[("GOLDENBEE_PRINCIPALS", principals)]);

    let first = worker.submit(
        "dev-token",
        "idem-session",
        "original payload",
        "stable-key",
    );
    let id = first["task_id"].as_str().expect("task id").to_string();
    assert_eq!(first["replayed"], false);
    let done = worker.wait("dev-token", &id);
    assert!(output(&done).contains("original payload"));
    let invocation = done["native_invocation_id"]
        .as_str()
        .expect("native invocation id")
        .to_string();

    let conflict = worker
        .post_task(
            "dev-token",
            &request("idem-session", "changed payload", "stable-key"),
        )
        .expect("conflict responds");
    assert_eq!(conflict.status(), reqwest::StatusCode::CONFLICT);
    let error: Value = conflict.json().expect("error is JSON");
    assert_eq!(error["code"], "task.idempotency_conflict");

    // The accepted request is unchanged by the rejected reuse.
    let untouched = worker.task("dev-token", &id);
    assert_eq!(untouched["input"], "original payload");
    assert_eq!(untouched["state"], "succeeded");
    assert_eq!(untouched["native_invocation_id"], invocation.as_str());

    // Another authenticated caller cannot receive or read the original handle.
    let foreign = worker.submit("mallory", "idem-session", "original payload", "stable-key");
    let foreign_id = foreign["task_id"].as_str().expect("task id").to_string();
    assert_ne!(foreign_id, id);
    assert_eq!(foreign["replayed"], false);
    assert_eq!(
        worker.status("mallory", &format!("/v1/tasks/{id}")),
        reqwest::StatusCode::FORBIDDEN
    );
    assert_eq!(
        worker.status("mallory", &format!("/v1/tasks/{id}/events")),
        reqwest::StatusCode::FORBIDDEN
    );
    let foreign_done = worker.wait("mallory", &foreign_id);
    assert_eq!(foreign_done["caller"], "mallory");
    assert_ne!(
        foreign_done["native_invocation_id"],
        invocation.as_str(),
        "a foreign caller must not inherit the original native invocation"
    );
    assert_eq!(worker.list("mallory").len(), 1);

    // Restart preserves both handles and the denial boundary.
    let first_incarnation = worker.incarnation.clone();
    worker.restart();
    assert_ne!(first_incarnation, worker.incarnation);

    let retry = worker.submit(
        "dev-token",
        "idem-session",
        "original payload",
        "stable-key",
    );
    assert_eq!(retry["task_id"], id.as_str());
    assert_eq!(retry["replayed"], true);
    assert_eq!(
        worker.task("dev-token", &id)["native_invocation_id"],
        invocation.as_str()
    );

    let foreign_retry = worker.submit("mallory", "idem-session", "original payload", "stable-key");
    assert_eq!(foreign_retry["task_id"], foreign_id.as_str());
    assert_eq!(foreign_retry["replayed"], true);
    assert_eq!(
        worker.status("mallory", &format!("/v1/tasks/{id}")),
        reqwest::StatusCode::FORBIDDEN
    );
}

/// The task record pins admission and native identities. Retained harness
/// events stay management projections: they never copy native ADK event bodies,
/// so canonical session history remains the only conversation record.
#[test]
fn task_record_pins_native_identity_without_duplicating_adk_history() {
    let mut worker = Worker::start("dev-token");
    let first = worker.run("dev-token", "pinned-session", "first pinned turn", "pin-1");
    let id = first["id"].as_str().expect("task id").to_string();

    assert_eq!(first["agent"], "general");
    assert_eq!(first["profile"], "default");
    assert_eq!(first["profile_revision"], 1);
    assert_eq!(first["caller"], "local");
    assert_eq!(first["namespace"], "local");
    assert_eq!(first["session_id"], "pinned-session");
    assert_eq!(first["state"], "succeeded");
    assert_eq!(first["attempt"], 1);
    assert!(
        first["request_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:") && digest.len() == 71),
        "canonical request digest is recorded"
    );
    let invocation = first["native_invocation_id"]
        .as_str()
        .expect("native invocation id")
        .to_string();
    assert!(invocation.starts_with("inv-"), "{invocation}");
    assert!(output(&first).contains("first pinned turn"));

    let page = worker.events("dev-token", &id);
    let kinds = event_kinds(&page);
    for expected in [
        "task.accepted",
        "task.queued",
        "task.running",
        "task.succeeded",
    ] {
        assert!(
            kinds.iter().any(|kind| kind == expected),
            "missing {expected} in {kinds:?}"
        );
    }
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "task.accepted").count(),
        1,
        "acceptance is recorded once"
    );
    let serialized = page.to_string();
    assert!(
        !serialized.contains("llm_response") && !serialized.contains("state_delta"),
        "ADK event bodies must not be copied into the task stream"
    );
    assert!(
        page["events"]
            .as_array()
            .expect("events")
            .iter()
            .all(|event| event["stream_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("task:"))),
        "task events are management projections"
    );

    // A follow-up turn observes the canonical native history, not the task
    // record: context grows from two events to four.
    let second = worker.run("dev-token", "pinned-session", "second pinned turn", "pin-2");
    assert!(
        output(&second).contains("[context:4]"),
        "follow-up must use canonical ADK history: {}",
        output(&second)
    );
    assert_ne!(second["id"], first["id"]);
    assert_ne!(second["native_invocation_id"], invocation.as_str());

    worker.restart();
    let recovered = worker.task("dev-token", &id);
    assert_eq!(recovered["state"], "succeeded");
    assert_eq!(recovered["output"], first["output"]);
    assert_eq!(recovered["native_invocation_id"], invocation.as_str());
    assert_eq!(recovered["attempt"], 1);
    assert_eq!(recovered["profile_revision"], 1);
}

/// Acceptance is durable or absent. An injected metadata-write failure and a
/// real filesystem write failure both leave no task, and the retry with the
/// same key admits exactly one task.
#[test]
fn persistence_failure_never_produces_false_acceptance() {
    let faults = tempfile::tempdir().expect("fault directory");
    let fault_dir = faults.path().display().to_string();
    let worker = Worker::start_with("dev-token", &[("GOLDENBEE_FAULT_DIR", fault_dir.clone())]);
    let baseline = worker.run("dev-token", "persist-session", "baseline", "persist-0");
    assert_eq!(output(&baseline), "echo: baseline [context:2]");

    std::fs::write(faults.path().join("persist-failure"), b"once")
        .expect("fault marker is writable");
    let failed = worker
        .post_task(
            "dev-token",
            &request("persist-session", "injected", "persist-1"),
        )
        .expect("injected failure responds");
    assert_eq!(failed.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let error: Value = failed.json().expect("error is JSON");
    assert_eq!(error["code"], "storage.unavailable");
    assert!(error["retryable"].as_bool().unwrap_or(false));
    assert_eq!(
        worker.list("dev-token").len(),
        1,
        "a failed metadata write must not record a task"
    );

    let accepted = worker.submit("dev-token", "persist-session", "injected", "persist-1");
    assert_eq!(accepted["replayed"], false);
    assert_eq!(worker.list("dev-token").len(), 2);

    // The same failure class occurs when the filesystem refuses the atomic
    // state write. A full disk takes the same error path.
    let data = worker.data.path().to_path_buf();
    let mut permissions = std::fs::metadata(&data)
        .expect("data metadata")
        .permissions();
    let original = permissions.clone();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o555);
    std::fs::set_permissions(&data, permissions).expect("data directory becomes read-only");

    let refused = worker
        .post_task(
            "dev-token",
            &request("persist-session", "refused", "persist-2"),
        )
        .expect("write refusal responds");
    assert_eq!(refused.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let error: Value = refused.json().expect("error is JSON");
    assert_eq!(error["code"], "storage.unavailable");
    assert_eq!(worker.list("dev-token").len(), 2);

    std::fs::set_permissions(&data, original).expect("data directory is writable again");
    let accepted = worker.submit("dev-token", "persist-session", "refused", "persist-2");
    assert_eq!(accepted["replayed"], false);
    assert_eq!(worker.list("dev-token").len(), 3);
    assert_eq!(
        worker.task("dev-token", baseline["id"].as_str().unwrap())["state"],
        "succeeded",
        "earlier accepted work is untouched"
    );
}

/// A crash around slot reservation or native start leaves the handle durable but
/// the native outcome unknown. Restart reconciles that uncertainty instead of
/// dispatching the effect again under the same task.
#[test]
fn uncertain_native_start_is_reconciled_without_auto_dispatch() {
    for point in [
        "after-reservation",
        "before-native-start",
        "after-native-start",
        "after-native-event",
    ] {
        let faults = tempfile::tempdir().expect("fault directory");
        let fault_dir = faults.path().display().to_string();
        let mut worker = Worker::start_with("dev-token", &[("GOLDENBEE_FAULT_DIR", fault_dir)]);
        std::fs::write(faults.path().join(point), b"once").expect("fault marker is writable");

        let body = request("uncertain-session", point, "uncertain-key");
        let submitted = worker
            .post_task("dev-token", &body)
            .ok()
            .and_then(|response| {
                if response.status().is_success() {
                    response.json::<Value>().ok()
                } else {
                    None
                }
            });
        worker.wait_for_fault_exit();
        worker.restart();

        let id = match submitted {
            Some(value) => value["task_id"].as_str().expect("task id").to_string(),
            None => {
                let tasks = worker.list("dev-token");
                assert_eq!(tasks.len(), 1, "{point}: {tasks:?}");
                tasks[0]["id"].as_str().expect("task id").to_string()
            }
        };

        let task = worker.task("dev-token", &id);
        assert_eq!(task["state"], "recovering", "{point}: {task}");
        assert!(
            task["uncertainty"]
                .as_str()
                .is_some_and(|reason| reason.contains("restarted")),
            "{point}: explicit uncertainty is required: {task}"
        );
        assert_eq!(task["attempt"], 1, "{point}");
        if point == "after-native-event" {
            assert!(
                task["native_invocation_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("inv-")),
                "{point}: the invocation identity is durable before the outcome: {task}"
            );
        } else {
            assert!(task["native_invocation_id"].is_null(), "{point}");
        }
        assert!(task["output"].is_null(), "{point}");

        let page = worker.events("dev-token", &id);
        let kinds = event_kinds(&page);
        assert!(
            kinds.iter().any(|kind| kind == "task.recovery_required"),
            "{point}: {kinds:?}"
        );
        assert_eq!(
            kinds.iter().filter(|kind| *kind == "task.running").count(),
            1,
            "{point}: {kinds:?}"
        );
        assert!(!kinds.iter().any(|kind| kind == "task.succeeded"));

        // No automatic second attempt appears after reconciliation.
        std::thread::sleep(Duration::from_millis(300));
        let steady = worker.task("dev-token", &id);
        assert_eq!(steady["state"], "recovering", "{point}");
        assert_eq!(steady["attempt"], 1, "{point}");
        assert_eq!(
            event_kinds(&worker.events("dev-token", &id))
                .iter()
                .filter(|kind| *kind == "task.running")
                .count(),
            1,
            "{point}"
        );

        // The external handle and idempotent identity remain stable.
        let retry = worker.submit("dev-token", "uncertain-session", point, "uncertain-key");
        assert_eq!(retry["task_id"], id.as_str(), "{point}");
        assert_eq!(retry["replayed"], true, "{point}");
        assert_eq!(retry["state"], "recovering", "{point}");
        assert_eq!(worker.list("dev-token").len(), 1, "{point}");
    }
}

/// Two instances of the same package have distinct process identity, task
/// handles, and stores. A handle from one instance is not silently resolved by
/// the other.
#[test]
fn separate_worker_instances_keep_independent_task_storage() {
    let mut first = Worker::start("dev-token");
    let mut second = Worker::start("dev-token");
    assert_ne!(first.instance, second.instance);

    let first_task = first.run("dev-token", "shared-session", "shared input", "shared-key");
    let second_task = second.run("dev-token", "shared-session", "shared input", "shared-key");
    let first_id = first_task["id"].as_str().expect("task id").to_string();
    let second_id = second_task["id"].as_str().expect("task id").to_string();
    assert_ne!(
        first_id, second_id,
        "instances allocate independent handles"
    );
    assert_ne!(
        first_task["native_invocation_id"],
        second_task["native_invocation_id"]
    );

    assert_eq!(first.list("dev-token").len(), 1);
    assert_eq!(second.list("dev-token").len(), 1);
    assert_eq!(
        first.status("dev-token", &format!("/v1/tasks/{second_id}")),
        reqwest::StatusCode::NOT_FOUND,
        "another instance's handle is not resolvable"
    );
    assert_eq!(
        second.status("dev-token", &format!("/v1/tasks/{first_id}")),
        reqwest::StatusCode::NOT_FOUND
    );

    let first_incarnation = first.incarnation.clone();
    let second_incarnation = second.incarnation.clone();
    first.restart();
    second.restart();
    assert_ne!(first.incarnation, first_incarnation);
    assert_ne!(second.incarnation, second_incarnation);

    let replay_first = first.submit("dev-token", "shared-session", "shared input", "shared-key");
    let replay_second = second.submit("dev-token", "shared-session", "shared input", "shared-key");
    assert_eq!(replay_first["task_id"], first_id.as_str());
    assert_eq!(replay_second["task_id"], second_id.as_str());
    assert_eq!(replay_first["replayed"], true);
    assert_eq!(replay_second["replayed"], true);
    assert_eq!(
        first.task("dev-token", &first_id)["native_invocation_id"],
        first_task["native_invocation_id"]
    );
    assert_eq!(
        second.task("dev-token", &second_id)["native_invocation_id"],
        second_task["native_invocation_id"]
    );
}
