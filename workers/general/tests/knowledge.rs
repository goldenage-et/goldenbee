//! Cross-process knowledge graph evidence: write through the native ADK tool,
//! restart, recall, and deny another caller through the Worker protocol.

use common::{Limits, SubmitTaskRequest};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

mod fixture;

struct Worker {
    child: Child,
    data: tempfile::TempDir,
    base: String,
    principals: Vec<(String, String, String)>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_worker(data: &Path, principals: &[(String, String, String)]) -> (Child, String) {
    let fixture = fixture::write(
        data,
        fixture::Options {
            instance: "general-knowledge".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: principals.to_vec(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    fixture::set_agent_tools(
        &fixture,
        "general",
        &["observe_knowledge", "recall_knowledge"],
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_general"));
    command
        .args([
            "serve",
            "--config",
            fixture.config.to_str().expect("fixture config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
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
    fn start() -> Self {
        let principals = vec![
            ("mallory".into(), "local".into(), "mallory".into()),
            ("other-ns".into(), "other".into(), "alice".into()),
        ];
        let data = tempfile::tempdir().expect("temporary data directory");
        let (child, base) = spawn_worker(data.path(), &principals);
        Self {
            child,
            data,
            base,
            principals,
        }
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (child, base) = spawn_worker(self.data.path(), &self.principals);
        self.child = child;
        self.base = base;
    }

    fn run(&self, token: &str, session: &str, input: &str, key: &str) -> Value {
        let client = reqwest::blocking::Client::new();
        let response = client
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
        let submitted: Value = serde_json::from_str(&body).expect("submit is JSON");
        let task_id = submitted["task_id"].as_str().expect("task id").to_string();
        for _ in 0..500 {
            let response = client
                .get(format!("{}/v1/tasks/{task_id}", self.base))
                .bearer_auth(token)
                .send()
                .expect("task responds");
            assert!(
                response.status().is_success(),
                "task read failed: {}",
                response.text().unwrap_or_default()
            );
            let task: Value = response.json().expect("task is JSON");
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
}

fn output(task: &Value) -> String {
    task["output"].as_str().unwrap_or_default().to_string()
}

#[test]
fn native_knowledge_tool_survives_restart_and_preserves_provenance() {
    let mut worker = Worker::start();
    let stored = worker.run(
        "dev-token",
        "knowledge-write",
        "observe:Ada|works_at|Acme|doc://1",
        "knowledge-write-1",
    );
    assert_eq!(stored["state"], "succeeded", "{}", output(&stored));
    let text = output(&stored);
    assert!(
        text.contains("observed"),
        "the native observe tool ran: {text}"
    );
    assert!(text.contains("\"stored\":true"), "{text}");
    assert!(text.contains("doc://1"), "source is returned: {text}");
    assert!(
        text.contains("\"namespace\":\"local\""),
        "scope is returned: {text}"
    );
    assert!(
        text.contains("\"valid_from\":"),
        "native valid-time is returned: {text}"
    );

    worker.restart();

    let recalled = worker.run(
        "dev-token",
        "knowledge-read",
        "recall:Ada",
        "knowledge-read-1",
    );
    assert_eq!(recalled["state"], "succeeded", "{}", output(&recalled));
    let text = output(&recalled);
    assert!(
        text.contains("recalled"),
        "the native recall tool ran: {text}"
    );
    assert!(text.contains("Ada"), "the entity was recalled: {text}");
    assert!(
        text.contains("Acme"),
        "the relation object was recalled: {text}"
    );
    assert!(
        text.contains("doc://1"),
        "source provenance survived restart: {text}"
    );
    assert!(
        text.contains("\"kind\":\"relation\""),
        "the returned relation is inspectable: {text}"
    );

    let foreign_principal = worker.run(
        "mallory",
        "knowledge-read",
        "recall:Ada",
        "knowledge-read-2",
    );
    assert_eq!(foreign_principal["state"], "succeeded");
    assert!(
        !output(&foreign_principal).contains("Acme"),
        "another principal must not retrieve the relation: {}",
        output(&foreign_principal)
    );

    let foreign_namespace = worker.run(
        "other-ns",
        "knowledge-read",
        "recall:Ada",
        "knowledge-read-3",
    );
    assert_eq!(foreign_namespace["state"], "succeeded");
    assert!(
        !output(&foreign_namespace).contains("Acme"),
        "another namespace must not retrieve the relation: {}",
        output(&foreign_namespace)
    );

    let bytes = std::fs::metadata(worker.data.path().join("native-graph.sqlite3"))
        .expect("native graph file exists")
        .len();
    assert!(bytes > 0, "the native backend persisted real bytes");
}

mod profile_capability {
    use super::fixture;
    use common::{Limits, ProfileRevision, SubmitTaskRequest, Task};
    use std::time::{Duration, Instant};

    fn revision() -> ProfileRevision {
        ProfileRevision {
            revision: 0,
            instruction: "PROFILE-NARROW".into(),
            model: "deterministic".into(),
            capabilities: vec!["echo".into()],
            grants: vec!["task:submit".into()],
            restrictions: Vec::new(),
            composition: "single".into(),
            members: vec!["echo".into()],
            skills: Vec::new(),
            mcp: Vec::new(),
            workflow: None,
            limits: Limits::default(),
            compaction: None,
            secret_refs: Vec::new(),
            created_at: 0,
        }
    }

    async fn wait(runtime: &runtime::Runtime, task_id: &str) -> Task {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let task = runtime.task_unscoped(task_id).await.expect("task exists");
            if task.state.is_terminal() {
                return task;
            }
            assert!(Instant::now() < deadline, "task did not finish: {task:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn profile_cannot_invent_a_graph_tool_the_package_did_not_bind() {
        let data = tempfile::tempdir().expect("data");
        let fixture = fixture::write(
            data.path(),
            fixture::Options {
                instance: "knowledge-profile".into(),
                token: Some("dev-token".into()),
                model: "deterministic".into(),
                principals: Vec::new(),
                settings: Vec::new(),
                token_scopes: vec!["*".into()],
            },
        );
        let instance = general::worker()
            .expect("package definition")
            .start(&fixture.config)
            .await
            .expect("worker starts");
        let runtime = instance.runtime();
        let admin = instance
            .operator_principal()
            .await
            .expect("operator principal");

        runtime
            .create_profile(&admin, "local", "narrow", revision())
            .await
            .expect("profile created");
        runtime
            .activate_profile(&admin, "local", "narrow", 1)
            .await
            .expect("profile activated");

        let submitted = runtime
            .submit_task(
                &admin,
                SubmitTaskRequest {
                    namespace: Some("local".into()),
                    agent: "echo".into(),
                    session_id: "profile-knowledge".into(),
                    input: "observe:Ada|works_at|Acme|doc://profile".into(),
                    idempotency_key: "profile-knowledge-1".into(),
                    profile: Some("narrow".into()),
                    team: None,
                    worker: None,
                    limits: Limits::default(),
                },
            )
            .await
            .expect("profile task admitted");
        let task = wait(&runtime, &submitted.task_id).await;
        assert_eq!(
            task.usage.tool_calls, 0,
            "no graph tool ran for the profile"
        );
        assert!(
            task.output
                .as_deref()
                .unwrap_or_default()
                .contains("Tool observe_knowledge not found"),
            "the profile agent has no graph tool to invoke: {task:?}"
        );

        let facts = runtime
            .search_knowledge(&admin, "local", "Ada", 10)
            .await
            .expect("knowledge search");
        assert!(
            facts.is_empty(),
            "the profile task must not write a graph fact: {facts:?}"
        );
    }
}
