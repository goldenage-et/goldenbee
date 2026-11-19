//! Cross-process evidence for native graph checkpoint resume.
//!
//! These tests drive the real `graph` Worker Package and its public `/v1`
//! routes. The pinned ADK `GraphAgent` and `SqliteCheckpointer` do the work; the
//! package supplies only the graph definition.

use common::{Limits, SubmitTaskRequest};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Worker {
    child: Child,
    data: tempfile::TempDir,
    config: PathBuf,
    base: String,
    token: String,
    env: Vec<(String, String)>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn config_for(data: &Path, instance: &str, scenario: &str, principals: bool) -> PathBuf {
    let mut config = format!(
        "instance = \"{instance}\"\nnamespace = \"local\"\ndefault_agent = \"branching\"\ntoken = \"dev-token\"\ntoken_scopes = [\"*\"]\n"
    );
    if principals {
        config.push_str(
            "\n[[tokens]]\ntoken = \"other\"\nnamespace = \"local\"\nprincipal = \"other\"\nscopes = [\"task:submit\", \"task:read\"]\n",
        );
        config.push_str(
            "\n[[tokens]]\ntoken = \"outsider\"\nnamespace = \"other-ns\"\nprincipal = \"outsider\"\nscopes = [\"task:submit\", \"task:read\"]\n",
        );
    }
    config.push_str(&format!(
        "\n[storage]\ndirectory = \"{}\"\n\n[settings]\nscenario = \"{scenario}\"\n",
        data.display()
    ));
    let path = data.join("worker.toml");
    std::fs::write(&path, config).expect("worker config");
    path
}

fn spawn_worker(config: &Path, env: &[(String, String)]) -> (Child, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_graph"));
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
    let mut child = command.spawn().expect("graph worker serves");
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
    fn start(scenario: &str) -> Self {
        Self::start_with(scenario, false, &[])
    }

    fn start_with(scenario: &str, principals: bool, env: &[(&str, String)]) -> Self {
        let data = tempfile::tempdir().expect("temporary data directory");
        let instance = format!(
            "graph-{}",
            data.path().file_name().unwrap().to_string_lossy()
        );
        let config = config_for(data.path(), &instance, scenario, principals);
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect();
        let (child, base) = spawn_worker(&config, &env);
        Self {
            child,
            data,
            config,
            base,
            token: "dev-token".into(),
            env,
        }
    }

    fn rewrite_scenario(&mut self, scenario: &str, principals: bool) {
        let instance = format!(
            "graph-{}",
            self.data.path().file_name().unwrap().to_string_lossy()
        );
        self.config = config_for(self.data.path(), &instance, scenario, principals);
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (child, base) = spawn_worker(&self.config, &self.env);
        self.child = child;
        self.base = base;
    }

    fn wait_for_fault_exit(&mut self) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            match self.child.try_wait().expect("worker status is observable") {
                Some(status) => {
                    assert!(
                        !status.success(),
                        "the injected boundary must terminate the process"
                    );
                    return;
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("the injected boundary did not terminate the worker");
    }

    fn client(&self) -> reqwest::blocking::Client {
        reqwest::blocking::Client::new()
    }

    fn submit_with_token(
        &self,
        token: &str,
        session: &str,
        input: &str,
        key: &str,
    ) -> reqwest::blocking::Response {
        self.client()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(token)
            .json(&SubmitTaskRequest {
                namespace: None,
                agent: "branching".into(),
                session_id: session.into(),
                input: input.into(),
                idempotency_key: key.into(),
                profile: None,
                team: None,
                worker: None,
                limits: Limits::default(),
            })
            .send()
            .expect("submit request is attempted")
    }

    fn submit(&self, session: &str, input: &str, key: &str) -> Value {
        let response = self.submit_with_token(&self.token, session, input, key);
        let status = response.status();
        let body = response.text().expect("submit body");
        assert!(status.is_success(), "submit failed: {status} {body}");
        serde_json::from_str(&body).expect("submit is JSON")
    }

    fn task_with_token(&self, token: &str, id: &str) -> reqwest::blocking::Response {
        self.client()
            .get(format!("{}/v1/tasks/{id}", self.base))
            .bearer_auth(token)
            .send()
            .expect("task request is attempted")
    }

    fn task(&self, token: &str, id: &str) -> Value {
        let response = self.task_with_token(token, id);
        let status = response.status();
        let body = response.text().expect("task body");
        assert!(status.is_success(), "task read failed: {status} {body}");
        serde_json::from_str(&body).expect("task is JSON")
    }

    fn events(&self, token: &str, id: &str) -> Value {
        self.client()
            .get(format!("{}/v1/tasks/{id}/events", self.base))
            .bearer_auth(token)
            .send()
            .expect("events respond")
            .json()
            .expect("events page is JSON")
    }

    fn resume(&self, token: &str, id: &str) -> reqwest::blocking::Response {
        self.client()
            .post(format!("{}/v1/tasks/{id}/resume", self.base))
            .bearer_auth(token)
            .send()
            .expect("resume responds")
    }

    fn wait(&self, token: &str, id: &str) -> Value {
        for _ in 0..1_000 {
            let task = self.task(token, id);
            if matches!(
                task["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled" | "unknown")
            ) {
                return task;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("task did not reach an observed boundary");
    }

    fn run(&self, session: &str, input: &str, key: &str) -> Value {
        let submitted = self.submit(session, input, key);
        self.wait(&self.token, submitted["task_id"].as_str().expect("task id"))
    }

    fn wait_for_state(&self, token: &str, id: &str, wanted: &str) -> Value {
        for _ in 0..1_000 {
            let task = self.task(token, id);
            if task["state"].as_str() == Some(wanted) {
                return task;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("task did not reach {wanted}");
    }

    fn approvals(&self, token: &str) -> Value {
        self.client()
            .get(format!("{}/v1/approvals?pending=true", self.base))
            .bearer_auth(token)
            .send()
            .expect("approval list responds")
            .json()
            .expect("approval list is JSON")
    }

    fn approval_by_id(&self, token: &str, approval_id: &str) -> Value {
        self.client()
            .get(format!("{}/v1/approvals/{approval_id}", self.base))
            .bearer_auth(token)
            .send()
            .expect("approval responds")
            .json()
            .expect("approval is JSON")
    }

    fn decide(
        &self,
        token: &str,
        approval_id: &str,
        decision: &str,
        digest: &str,
    ) -> reqwest::blocking::Response {
        self.client()
            .post(format!("{}/v1/approvals/{approval_id}/decision", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({"decision": decision, "digest": digest}))
            .send()
            .expect("approval decision is attempted")
    }

    /// A decision whose process may terminate before the HTTP response closes.
    fn decide_ignoring_disconnect(&self, token: &str, approval_id: &str, digest: &str) {
        let _ = self
            .client()
            .post(format!("{}/v1/approvals/{approval_id}/decision", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({"decision": "approved", "digest": digest}))
            .send();
    }

    fn effects(&self) -> Vec<String> {
        std::fs::read_to_string(self.data.path().join("graph-effects.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn checkpoint_path(&self) -> PathBuf {
        self.data.path().join("native-graph.sqlite3")
    }

    fn checkpoint_rows(&self) -> Vec<(String, String, String)> {
        let path = self.checkpoint_path();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("temporary runtime");
        runtime.block_on(async move {
            let url = format!("sqlite://{}?mode=rwc", path.display());
            let pool = sqlx::sqlite::SqlitePool::connect(&url)
                .await
                .expect("native checkpoint store opens");
            let rows = sqlx::query_as::<_, (String, String, String)>(
                "SELECT metadata, pending_nodes, state FROM graph_checkpoints ORDER BY created_at ASC",
            )
            .fetch_all(&pool)
            .await
            .expect("checkpoints are readable");
            pool.close().await;
            rows
        })
    }

    fn execute_sql(&self, sql: &'static str, thread: &str) {
        let path = self.checkpoint_path();
        let thread = thread.to_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("temporary runtime");
        runtime.block_on(async move {
            let url = format!("sqlite://{}?mode=rwc", path.display());
            let pool = sqlx::sqlite::SqlitePool::connect(&url)
                .await
                .expect("native checkpoint store opens");
            sqlx::query(sql)
                .bind(thread)
                .execute(&pool)
                .await
                .expect("checkpoint maintenance succeeds");
            pool.close().await;
        });
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

fn count(effects: &[String], node: &str) -> usize {
    effects
        .iter()
        .filter(|entry| entry.as_str() == node)
        .count()
}

/// A branching workflow executes through the native graph API, binds the public
/// task to the native session thread, and commits definition-bound checkpoints.
#[test]
fn branching_graph_binds_public_thread_and_persists_definition() {
    let worker = Worker::start("branching");
    let submitted = worker.submit("graph-session", "go left", "branch-key");
    let id = submitted["task_id"].as_str().expect("task id").to_string();
    let task = worker.wait(&worker.token, &id);

    assert_eq!(task["state"], "succeeded", "{task:?}");
    assert_eq!(output(&task), "left:complete");
    assert_eq!(
        task["native_thread_id"].as_str(),
        Some("graph-session"),
        "the public binding is the native session thread"
    );
    assert!(task.get("native_invocation_id").is_none() || task["native_invocation_id"].is_null());
    assert_eq!(worker.effects(), ["a", "b", "c"]);

    let rows = worker.checkpoint_rows();
    assert!(!rows.is_empty(), "native SQLite checkpoint rows exist");
    let (metadata, pending, _state) = &rows[rows.len() - 1];
    let metadata: Value = serde_json::from_str(metadata).expect("metadata is JSON");
    assert_eq!(metadata["goldenbee.graph_id"], "branching");
    assert_eq!(metadata["goldenbee.graph_version"], 1);
    assert_eq!(metadata["goldenbee.package"], "graph");
    assert!(metadata["goldenbee.instance"].as_str().is_some());
    assert!(metadata["goldenbee.graph_digest"]
        .as_str()
        .is_some_and(|digest| digest.len() == 64));
    assert_eq!(pending, "[]", "the latest checkpoint is terminal");

    let kinds = event_kinds(&worker.events(&worker.token, &id));
    assert!(
        kinds.contains(&"graph.thread_bound".to_string()),
        "{kinds:?}"
    );
    assert!(kinds.contains(&"task.succeeded".to_string()), "{kinds:?}");
}

/// A process killed inside an uncommitted node resumes the same thread after
/// restart. The committed `a` node is not repeated; the uncommitted `b` node is
/// re-executed, which is native frontier semantics rather than exactly-once.
#[test]
fn restart_resumes_committed_checkpoint_without_repeating_committed_nodes() {
    let faults = tempfile::tempdir().expect("fault directory");
    let fault_dir = faults.path().display().to_string();
    let mut worker = Worker::start_with("crash", false, &[("GOLDENBEE_FAULT_DIR", fault_dir)]);
    std::fs::write(worker.data.path().join("crash-b.once"), b"once")
        .expect("crash marker is writable");

    let submitted = worker.submit("restart-session", "go left", "restart-key");
    let id = submitted["task_id"].as_str().expect("task id").to_string();
    worker.wait_for_fault_exit();
    assert_eq!(worker.effects(), ["a", "b"]);

    worker.restart();
    let recovering = worker.task(&worker.token, &id);
    assert_eq!(recovering["state"], "recovering", "{recovering:?}");
    assert_eq!(recovering["native_thread_id"], "restart-session");
    assert_eq!(count(&worker.effects(), "a"), 1);

    let resumed = worker.resume(&worker.token, &id);
    let status = resumed.status();
    let body = resumed.text().expect("resume body");
    assert!(status.is_success(), "resume failed: {status} {body}");
    let task = worker.wait(&worker.token, &id);

    assert_eq!(task["state"], "succeeded", "{task:?}");
    assert_eq!(output(&task), "left:complete");
    assert_eq!(task["native_thread_id"], "restart-session");
    let effects = worker.effects();
    assert_eq!(
        count(&effects, "a"),
        1,
        "committed node must not repeat: {effects:?}"
    );
    assert_eq!(
        count(&effects, "b"),
        2,
        "uncommitted node re-executes at the native frontier: {effects:?}"
    );
    assert_eq!(count(&effects, "c"), 1, "{effects:?}");

    let page = worker.events(&worker.token, &id);
    let kinds = event_kinds(&page);
    assert!(
        kinds.contains(&"task.resume_accepted".to_string()),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&"task.recovery_required".to_string()),
        "{kinds:?}"
    );
    let incarnations: std::collections::BTreeSet<_> = page["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter_map(|event| event["incarnation"].as_str())
        .collect();
    assert!(
        incarnations.len() >= 2,
        "restart evidence must span incarnations: {incarnations:?}"
    );
}

/// A terminal checkpoint discovered after a restart is completed from its
/// committed state. The graph does not run again and no node effect repeats.
#[test]
fn terminal_checkpoint_resume_preserves_evidence_without_rerunning() {
    let faults = tempfile::tempdir().expect("fault directory");
    let fault_dir = faults.path().display().to_string();
    let mut worker = Worker::start_with("branching", false, &[("GOLDENBEE_FAULT_DIR", fault_dir)]);
    std::fs::write(faults.path().join("after-native-event"), b"once")
        .expect("fault marker is writable");

    let submitted = worker.submit("terminal-session", "go left", "terminal-key");
    let id = submitted["task_id"].as_str().expect("task id").to_string();
    worker.wait_for_fault_exit();
    assert_eq!(worker.effects(), ["a", "b", "c"]);

    worker.restart();
    let recovering = worker.task(&worker.token, &id);
    assert_eq!(recovering["state"], "recovering", "{recovering:?}");
    assert_eq!(recovering["native_thread_id"], "terminal-session");

    let response = worker.resume(&worker.token, &id);
    let status = response.status();
    let body = response.text().expect("resume body");
    assert!(status.is_success(), "resume failed: {status} {body}");
    let task = worker.wait(&worker.token, &id);

    assert_eq!(task["state"], "succeeded", "{task:?}");
    assert_eq!(output(&task), "left:complete");
    assert_eq!(task["native_thread_id"], "terminal-session");
    assert_eq!(
        worker.effects(),
        ["a", "b", "c"],
        "a terminal checkpoint must not re-run any node"
    );
    let kinds = event_kinds(&worker.events(&worker.token, &id));
    assert!(
        kinds.contains(&"task.resumed_from_checkpoint".to_string()),
        "{kinds:?}"
    );
}

/// Only the original identity in the original namespace can resume. A foreign
/// caller receives not-found and the task stays recovering with no new work.
#[test]
fn denied_resume_is_not_found_and_preserves_task() {
    let mut worker = Worker::start_with("crash", true, &[]);
    std::fs::write(worker.data.path().join("crash-b.once"), b"once")
        .expect("crash marker is writable");
    let submitted = worker.submit("denied-session", "go left", "denied-key");
    let id = submitted["task_id"].as_str().expect("task id").to_string();
    worker.wait_for_fault_exit();
    worker.restart();

    for token in ["other", "outsider"] {
        let response = worker.resume(token, &id);
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND,
            "foreign resume must not reveal the task"
        );
        let error: Value = response.json().expect("error is JSON");
        assert_eq!(error["code"], "task.not_found");
    }
    let unchanged = worker.task(&worker.token, &id);
    assert_eq!(unchanged["state"], "recovering", "{unchanged:?}");
    assert!(unchanged["output"].is_null());
    assert_eq!(count(&worker.effects(), "a"), 1);

    let resumed = worker.resume(&worker.token, &id);
    assert!(resumed.status().is_success());
    let task = worker.wait(&worker.token, &id);
    assert_eq!(task["state"], "succeeded", "{task:?}");
}

/// A deleted or undecodable checkpoint is an actionable recovering condition.
/// Neither case silently starts a fresh run under the original task handle.
#[test]
fn missing_checkpoint_is_actionable_and_does_not_restart_fresh() {
    let mut worker = Worker::start("crash");
    std::fs::write(worker.data.path().join("crash-b.once"), b"once")
        .expect("crash marker is writable");
    let submitted = worker.submit("missing-session", "go left", "missing-key");
    let id = submitted["task_id"].as_str().expect("task id").to_string();
    worker.wait_for_fault_exit();
    worker.restart();
    worker.execute_sql(
        "DELETE FROM graph_checkpoints WHERE thread_id = ?",
        "missing-session",
    );

    let response = worker.resume(&worker.token, &id);
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let error: Value = response.json().expect("error is JSON");
    assert_eq!(error["code"], "graph.checkpoint_missing");
    let task = worker.task(&worker.token, &id);
    assert_eq!(task["state"], "recovering", "{task:?}");
    assert_eq!(
        worker.effects(),
        ["a", "b"],
        "resume must not dispatch a fresh graph"
    );
}

#[test]
fn corrupt_checkpoint_is_actionable_and_quarantined() {
    let mut worker = Worker::start("crash");
    std::fs::write(worker.data.path().join("crash-b.once"), b"once")
        .expect("crash marker is writable");
    let submitted = worker.submit("corrupt-session", "go left", "corrupt-key");
    let id = submitted["task_id"].as_str().expect("task id").to_string();
    worker.wait_for_fault_exit();
    worker.restart();
    worker.execute_sql(
        "UPDATE graph_checkpoints SET state = '{not-json' WHERE thread_id = ?",
        "corrupt-session",
    );

    let response = worker.resume(&worker.token, &id);
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let error: Value = response.json().expect("error is JSON");
    assert_eq!(error["code"], "graph.checkpoint_corrupt");
    let task = worker.task(&worker.token, &id);
    assert_eq!(task["state"], "recovering", "{task:?}");
    assert_eq!(count(&worker.effects(), "a"), 1);
}

/// Changing the graph definition after a crash is rejected before execution.
#[test]
fn changed_graph_definition_fails_safely() {
    let mut worker = Worker::start("crash");
    std::fs::write(worker.data.path().join("crash-b.once"), b"once")
        .expect("crash marker is writable");
    let submitted = worker.submit("definition-session", "go left", "definition-key");
    let id = submitted["task_id"].as_str().expect("task id").to_string();
    worker.wait_for_fault_exit();
    let before = worker.effects();

    // Same graph id and version, different native topology.
    worker.rewrite_scenario("cycle", false);
    worker.restart();
    let response = worker.resume(&worker.token, &id);
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let error: Value = response.json().expect("error is JSON");
    assert_eq!(error["code"], "graph.definition_incompatible");
    let task = worker.task(&worker.token, &id);
    assert_eq!(task["state"], "recovering", "{task:?}");
    assert_eq!(worker.effects(), before, "no node may run on mismatch");
}

/// A native interrupt is a checkpointed pause, not successful output. Resume
/// clears the static gate and completes the same thread.
#[test]
fn graph_interrupt_is_not_success_and_resume_continues() {
    let worker = Worker::start("interrupt");
    let submitted = worker.submit("interrupt-session", "go left", "interrupt-key");
    let id = submitted["task_id"].as_str().expect("task id").to_string();
    let paused = worker.wait(&worker.token, &id);

    assert_eq!(paused["state"], "unknown", "{paused:?}");
    assert_eq!(paused["error"]["code"], "graph.interrupted");
    assert!(paused["output"].is_null());
    let kinds = event_kinds(&worker.events(&worker.token, &id));
    assert!(
        kinds.contains(&"graph.interrupted".to_string()),
        "{kinds:?}"
    );
    assert_eq!(worker.effects(), ["a"]);
    let rows = worker.checkpoint_rows();
    let (_, pending, _) = &rows[rows.len() - 1];
    assert!(
        serde_json::from_str::<Vec<String>>(pending)
            .expect("pending nodes")
            .contains(&"b".to_string()),
        "the paused frontier retains the gate node"
    );

    let resumed = worker.resume(&worker.token, &id);
    assert!(resumed.status().is_success());
    let task = worker.wait(&worker.token, &id);
    assert_eq!(task["state"], "succeeded", "{task:?}");
    assert_eq!(output(&task), "left:complete");
    assert_eq!(worker.effects(), ["a", "b", "c"]);
}

/// Native recursion bounds stop a cyclic graph; the task fails truthfully.
/// A protected operation pauses before its effect, survives a process restart
/// with its native checkpoint, and an approval resumes the same graph thread
/// for exactly one external effect.
#[test]
fn approval_survives_restart_and_resumes_the_same_native_thread_once() {
    let ledger = tempfile::tempdir().expect("external ledger directory");
    let ledger_path = ledger.path().display().to_string();
    let mut worker = Worker::start_with(
        "approval",
        true,
        &[("GOLDENBEE_GRAPH_EFFECT_DIR", ledger_path.clone())],
    );
    let session = "approval-session";
    let submitted = worker.submit(
        session,
        "call:ledger_write {\"entry\":\"graph-target\"}",
        "approval-key",
    );
    let id = submitted["task_id"].as_str().expect("task id").to_string();
    let waiting = worker.wait_for_state(&worker.token, &id, "waiting_approval");
    assert_eq!(waiting["native_thread_id"], session);
    assert_eq!(effect_lines(ledger.path()), Vec::<String>::new());

    let pending = worker.approvals(&worker.token);
    let approval = pending
        .as_array()
        .and_then(|values| values.iter().find(|value| value["task_id"] == id))
        .cloned()
        .expect("pending approval is visible");
    assert_eq!(approval["operation"], "ledger_write");
    assert_eq!(approval["target"], "graph-target");
    assert_eq!(approval["risk"], "external-write");
    let digest = approval["digest"].as_str().expect("digest").to_string();
    let approval_id = approval["id"].as_str().expect("approval id").to_string();

    // Kill while the native workflow waits on the exact operation.
    worker.restart();
    let recovering = worker.task(&worker.token, &id);
    assert_eq!(recovering["state"], "recovering", "{recovering:?}");
    assert_eq!(recovering["native_thread_id"], session);
    let replay = worker.approvals(&worker.token);
    assert!(
        replay
            .as_array()
            .expect("approvals")
            .iter()
            .any(|value| value["id"] == approval_id && value["status"] == "pending"),
        "the persisted request survives restart: {replay}"
    );
    assert!(
        !worker.checkpoint_rows().is_empty(),
        "the native graph checkpoint survives restart"
    );
    assert_eq!(effect_lines(ledger.path()), Vec::<String>::new());

    let response = worker.decide(&worker.token, &approval_id, "approved", &digest);
    assert_eq!(response.status(), 200);
    let task = worker.wait(&worker.token, &id);
    assert_eq!(task["state"], "succeeded", "{task:?}");
    assert_eq!(task["native_thread_id"], session);
    assert_eq!(effect_lines(ledger.path()).len(), 1);

    let repeated = worker.decide(&worker.token, &approval_id, "approved", &digest);
    assert_eq!(repeated.status(), 200);
    assert_eq!(effect_lines(ledger.path()).len(), 1);
    let kinds = event_kinds(&worker.events(&worker.token, &id));
    assert!(
        kinds.contains(&"approval.approved".to_string()),
        "{kinds:?}"
    );
    assert!(kinds.contains(&"approval.receipt".to_string()), "{kinds:?}");
    assert!(
        kinds.contains(&"task.resume_accepted".to_string()),
        "{kinds:?}"
    );
}

/// A crash after the durable approval decision but before the native workflow
/// resumes leaves one recorded decision, no effect, and a resumable checkpoint.
/// Replaying the identical decision completes the same operation once.
#[test]
fn crash_near_decision_keeps_one_decision_and_resumes_without_duplicate_effect() {
    let ledger = tempfile::tempdir().expect("external ledger directory");
    let faults = tempfile::tempdir().expect("fault directory");
    let mut worker = Worker::start_with(
        "approval",
        true,
        &[
            (
                "GOLDENBEE_GRAPH_EFFECT_DIR",
                ledger.path().display().to_string(),
            ),
            ("GOLDENBEE_FAULT_DIR", faults.path().display().to_string()),
        ],
    );
    let submitted = worker.submit(
        "approval-crash-session",
        "call:ledger_write {\"entry\":\"crash-target\"}",
        "approval-crash-key",
    );
    let id = submitted["task_id"].as_str().expect("task id").to_string();
    worker.wait_for_state(&worker.token, &id, "waiting_approval");
    let approval = worker
        .approvals(&worker.token)
        .as_array()
        .and_then(|values| values.iter().find(|value| value["task_id"] == id))
        .cloned()
        .expect("pending approval");
    let approval_id = approval["id"].as_str().expect("approval id").to_string();
    let digest = approval["digest"].as_str().expect("digest").to_string();

    std::fs::write(faults.path().join("after-approval-decision"), b"once")
        .expect("fault marker is writable");
    worker.decide_ignoring_disconnect(&worker.token, &approval_id, &digest);
    worker.wait_for_fault_exit();
    assert_eq!(effect_lines(ledger.path()), Vec::<String>::new());

    worker.restart();
    let recovering = worker.task(&worker.token, &id);
    assert_eq!(recovering["state"], "recovering", "{recovering:?}");
    assert_eq!(
        worker.approval_by_id(&worker.token, &approval_id)["status"],
        "approved"
    );
    assert!(!worker.checkpoint_rows().is_empty());
    assert_eq!(effect_lines(ledger.path()), Vec::<String>::new());

    // Replaying the identical decision finishes the interrupted resume.
    let replay = worker.decide(&worker.token, &approval_id, "approved", &digest);
    assert_eq!(replay.status(), 200);
    let task = worker.wait(&worker.token, &id);
    assert_eq!(task["state"], "succeeded", "{task:?}");
    assert_eq!(effect_lines(ledger.path()).len(), 1);
    assert_eq!(
        worker.approval_by_id(&worker.token, &approval_id)["status"],
        "approved"
    );
}

fn effect_lines(directory: &Path) -> Vec<String> {
    std::fs::read_to_string(directory.join("effects.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn native_recursion_limit_bounds_cycle() {
    let worker = Worker::start("cycle");
    let task = worker.run("cycle-session", "loop", "cycle-key");

    assert_eq!(task["state"], "failed", "{task:?}");
    assert_eq!(task["error"]["code"], "graph.recursion_limit");
    assert_ne!(task["state"], "succeeded");
    assert_eq!(
        count(&worker.effects(), "loop"),
        50,
        "the pinned ExecutionConfig default is the effective super-step bound"
    );
}
