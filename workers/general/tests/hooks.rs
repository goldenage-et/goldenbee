//! Cross-process Worker hook and native callback/plugin evidence.

use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

mod fixture;

struct Worker {
    child: Child,
    base: String,
    log: PathBuf,
    token: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn(config: &Path, log: &Path, cwd: &Path) -> Worker {
    let stderr = File::create(log).expect("stderr log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .current_dir(cwd)
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
        .expect("address announcement")
        .trim()
        .to_string();
    Worker {
        child,
        base,
        log: log.to_path_buf(),
        token: "dev-token".into(),
    }
}

impl Worker {
    fn submit(&self, agent: &str, input: &str, key: &str) -> (reqwest::StatusCode, Value) {
        let response = reqwest::blocking::Client::new()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "agent": agent,
                "session_id": format!("session-{key}"),
                "input": input,
                "idempotency_key": key,
            }))
            .send()
            .expect("submit responds");
        let status = response.status();
        let body = response.text().expect("submit body");
        (status, serde_json::from_str(&body).unwrap_or(Value::Null))
    }

    fn run(&self, agent: &str, input: &str, key: &str) -> Value {
        let (status, submitted) = self.submit(agent, input, key);
        assert!(status.is_success(), "submit failed: {status} {submitted}");
        self.wait(submitted["task_id"].as_str().expect("task id"))
    }

    fn wait(&self, task_id: &str) -> Value {
        for _ in 0..500 {
            let task: Value = reqwest::blocking::Client::new()
                .get(format!("{}/v1/tasks/{task_id}", self.base))
                .bearer_auth(&self.token)
                .send()
                .expect("task responds")
                .json()
                .expect("task is JSON");
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

    fn events(&self, task_id: &str) -> Vec<Value> {
        let page: Value = reqwest::blocking::Client::new()
            .get(format!("{}/v1/tasks/{task_id}/events", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("events respond")
            .json()
            .expect("events JSON");
        page["events"].as_array().cloned().unwrap_or_default()
    }

    fn tasks(&self) -> Vec<Value> {
        let value: Value = reqwest::blocking::Client::new()
            .get(format!("{}/v1/tasks", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("tasks respond")
            .json()
            .expect("tasks JSON");
        value.as_array().cloned().unwrap_or_default()
    }

    fn stop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let _ = self.child.wait();
    }
}

fn output(task: &Value) -> String {
    task["output"].as_str().unwrap_or_default().to_string()
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

#[test]
fn worker_hook_order_rejection_and_redacted_failures_are_real() {
    let cwd = tempfile::tempdir().expect("cwd");
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("hooks.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "hooks".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: vec![
                ("reject_contains".into(), "forbidden".into()),
                ("after_task_fail".into(), "true".into()),
            ],
            token_scopes: vec!["*".into()],
        },
    );
    let mut worker = spawn(&fixture.config, &log, cwd.path());

    // Mandatory admission rejects an unknown agent before any hook runs.
    let (status, _) = worker.submit("absent", "must not run", "hook-unauthorized");
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert!(worker.tasks().is_empty());

    let accepted = worker.run("general", "hook-normal", "hook-normal");
    assert_eq!(accepted["state"], "succeeded");
    let task_id = accepted["id"].as_str().unwrap();
    let mut after_failure = None;
    for _ in 0..100 {
        let events = worker.events(task_id);
        after_failure = events
            .into_iter()
            .find(|event| event["kind"] == "hook.after_task_failed");
        if after_failure.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let after_failure = after_failure.expect("after-task failure is recorded");
    assert_eq!(after_failure["payload"]["code"], "policy.after_task_failed");
    assert!(!after_failure["payload"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("sk-after-task-secret"));

    let (status, submitted) = worker.submit("general", "forbidden input", "hook-rejected");
    assert!(status.is_success(), "{status} {submitted}");
    assert_eq!(submitted["state"], "failed");
    let rejected = worker.wait(submitted["task_id"].as_str().unwrap());
    assert_eq!(rejected["state"], "failed");
    assert_eq!(rejected["error"]["code"], "policy.before_task_rejected");
    assert!(!rejected["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("sk-before-task-secret"));
    assert!(rejected["output"].is_null());

    worker.stop();
    let log = std::fs::read_to_string(&worker.log).expect("hook log");
    let startup = log.find("hook:startup").expect("startup hook");
    let before = log.find("hook:before_task").expect("before hook");
    let after = log.find("hook:after_task").expect("after hook");
    let shutdown = log.find("hook:shutdown").expect("shutdown hook");
    assert!(
        startup < before && before < after && after < shutdown,
        "{log}"
    );
    assert_eq!(count(&log, "hook:before_task"), 2, "{log}");
    assert_eq!(count(&log, "hook:after_task"), 1, "{log}");
    assert!(!log.contains("sk-"), "hook failures leaked secrets: {log}");
}

#[test]
fn native_callbacks_and_plugins_attach_to_loaded_and_manual_agents_once() {
    let cwd = tempfile::tempdir().expect("cwd");
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("native.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "native-hooks".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let worker = spawn(&fixture.config, &log, cwd.path());

    for agent in ["general", "manual"] {
        let task = worker.run(agent, "plugin: cb:", &format!("markers-{agent}"));
        let text = output(&task);
        assert_eq!(task["state"], "succeeded", "{agent}: {task}");
        for marker in [
            "[plugin-user]",
            "[agent-before]",
            "[agent-after]",
            "[plugin-event]",
        ] {
            assert_eq!(
                count(&text, marker),
                1,
                "{agent} must run {marker} exactly once: {text}"
            );
        }
        let plugin_user = text.find("[plugin-user]").unwrap();
        let agent_before = text.find("[agent-before]").unwrap();
        let agent_after = text.find("[agent-after]").unwrap();
        assert!(
            plugin_user < agent_before && agent_before < agent_after,
            "{agent} ordering: {text}"
        );
    }

    let short = worker.run("general", "cb-short", "short-agent");
    let text = output(&short);
    assert!(text.contains("agent-short-circuit"), "{text}");
    assert!(!text.contains("[agent-before]"), "{text}");

    let short = worker.run("general", "plugin-short", "short-plugin");
    let text = output(&short);
    assert!(text.contains("plugin-short-circuit"), "{text}");
    assert!(!text.contains("[agent-before]"), "{text}");

    let canary = data.path().join("cb-skip-canary");
    let skipped = worker.run(
        "general",
        &format!("sandbox:touch {} cb-skip-tool", canary.display()),
        "skip-tool",
    );
    assert_eq!(skipped["state"], "succeeded");
    assert!(output(&skipped).contains("agent-before-tool"), "{skipped}");
    assert!(
        !canary.exists(),
        "before-tool callback did not skip execution"
    );
}

#[test]
fn native_plugin_short_circuit_cannot_bypass_admission() {
    let cwd = tempfile::tempdir().expect("cwd");
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("bypass.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "bypass".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let worker = spawn(&fixture.config, &log, cwd.path());

    // A plugin can short-circuit a valid run, but it cannot create a task for
    // an unregistered agent or a principal without submission authority.
    let (status, _) = worker.submit("absent", "plugin-short", "bypass-agent");
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert!(worker.tasks().is_empty());

    let (status, _) = worker.submit("general", "plugin-short", "bypass-unauthorized");
    assert!(status.is_success());

    let denied = reqwest::blocking::Client::new()
        .get(format!("{}/v1/tasks", worker.base))
        .header("Authorization", "Bearer not-a-token")
        .send()
        .expect("tasks respond");
    assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);
}
