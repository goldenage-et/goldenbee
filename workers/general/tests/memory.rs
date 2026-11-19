//! Cross-process native memory evidence: write through ADK, restart, recall,
//! and deny another caller through the authenticated Worker protocol.

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
            instance: "general-memory".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: principals.to_vec(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
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
fn adk_memory_survives_restart_and_recalls_inside_native_invocation() {
    let mut worker = Worker::start();
    let stored = worker.run(
        "dev-token",
        "memory-write",
        "remember: alpha secret",
        "memory-write-1",
    );
    assert_eq!(stored["state"], "succeeded", "{}", output(&stored));
    assert!(
        output(&stored).contains("remembered"),
        "the native remember tool ran: {}",
        output(&stored)
    );

    worker.restart();

    // The deterministic model reports the system memory block that the native
    // PreloadMemoryTool callback injected during this real Runner invocation.
    let recalled = worker.run("dev-token", "memory-read", "alpha", "memory-read-1");
    assert_eq!(recalled["state"], "succeeded", "{}", output(&recalled));
    let text = output(&recalled);
    assert!(text.contains("alpha secret"), "memory was recalled: {text}");
    assert!(
        text.contains("deterministic-model"),
        "source provenance was preserved: {text}"
    );

    let foreign_principal = worker.run("mallory", "memory-read", "alpha", "memory-read-2");
    assert_eq!(foreign_principal["state"], "succeeded");
    assert!(
        !output(&foreign_principal).contains("alpha secret"),
        "another principal must not retrieve the memory: {}",
        output(&foreign_principal)
    );

    let foreign_namespace = worker.run("other-ns", "memory-read", "alpha", "memory-read-3");
    assert_eq!(foreign_namespace["state"], "succeeded");
    assert!(
        !output(&foreign_namespace).contains("alpha secret"),
        "another namespace must not retrieve the memory: {}",
        output(&foreign_namespace)
    );
}
