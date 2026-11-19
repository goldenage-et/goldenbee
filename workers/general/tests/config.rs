//! Cross-process configuration, asset, isolation, and reload evidence.

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod fixture;

struct Worker {
    child: Child,
    base: String,
    token: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn(config: &Path, cwd: &Path) -> Worker {
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
    assert!(base.starts_with("http://127.0.0.1:"), "{line}");
    Worker {
        child,
        base,
        token: "dev-token".into(),
    }
}

impl Worker {
    fn submit(&self, agent: &str, input: &str, key: &str) -> Value {
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
        assert!(status.is_success(), "submit failed: {status} {body}");
        serde_json::from_str(&body).expect("submit is JSON")
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

    fn run(&self, agent: &str, input: &str, key: &str) -> Value {
        let submitted = self.submit(agent, input, key);
        self.wait(submitted["task_id"].as_str().expect("task id"))
    }

    fn status(&self) -> Value {
        reqwest::blocking::Client::new()
            .get(format!("{}/v1/status", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("status responds")
            .json()
            .expect("status is JSON")
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

type Case = (
    &'static str,
    Box<dyn Fn(&fixture::Fixture, &Path)>,
    &'static str,
);

fn output(task: &Value) -> String {
    task["output"].as_str().unwrap_or_default().to_string()
}

fn start_output(config: &Path, cwd: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .current_dir(cwd)
        .output()
        .expect("worker exits")
}

#[test]
fn two_configs_from_an_unrelated_cwd_have_independent_assets_and_storage() {
    let cwd = tempfile::tempdir().expect("unrelated cwd");
    let first_data = tempfile::tempdir().expect("first data");
    let second_data = tempfile::tempdir().expect("second data");
    let first = fixture::write(
        first_data.path(),
        fixture::Options {
            instance: "instance-a".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    fixture::set_agent_prompt(&first, "general", "PROMPT-INSTANCE-A");
    let second = fixture::write(
        second_data.path(),
        fixture::Options {
            instance: "instance-b".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    fixture::set_agent_prompt(&second, "general", "PROMPT-INSTANCE-B");

    let first_worker = spawn(&first.config, cwd.path());
    let second_worker = spawn(&second.config, cwd.path());
    assert_ne!(
        first_worker.status()["instance"],
        second_worker.status()["instance"]
    );

    let first_task = first_worker.run("general", "prompt:", "isolation-a");
    let second_task = second_worker.run("general", "prompt:", "isolation-b");
    assert!(
        output(&first_task).contains("PROMPT-INSTANCE-A"),
        "{first_task}"
    );
    assert!(
        output(&second_task).contains("PROMPT-INSTANCE-B"),
        "{second_task}"
    );
    assert!(!output(&first_task).contains("INSTANCE-B"));
    assert!(!output(&second_task).contains("INSTANCE-A"));

    // The second instance's store does not contain the first instance's task.
    let foreign = reqwest::blocking::Client::new()
        .get(format!(
            "{}/v1/tasks/{}",
            second_worker.base,
            first_task["id"].as_str().unwrap()
        ))
        .bearer_auth(&second_worker.token)
        .send()
        .expect("task responds");
    assert_eq!(foreign.status(), reqwest::StatusCode::NOT_FOUND);
}

#[test]
fn file_defined_agent_and_manual_agent_execute_native_adk_behavior() {
    let cwd = tempfile::tempdir().expect("unrelated cwd");
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "native-agents".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let worker = spawn(&fixture.config, cwd.path());

    let loaded = worker.run("general", "sandbox:printf loaded-native", "native-loaded");
    assert_eq!(loaded["state"], "succeeded");
    assert!(output(&loaded).contains("sandbox result"), "{loaded}");
    assert!(output(&loaded).contains("loaded-native"), "{loaded}");

    let manual = worker.run("manual", "cb: manual-native", "native-manual");
    assert_eq!(manual["state"], "succeeded");
    assert!(output(&manual).contains("manual-native"), "{manual}");
    assert!(output(&manual).contains("[agent-before]"), "{manual}");

    // Configured skills and tool references are resolved and exposed.
    let caps = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "capabilities",
            "--config",
            fixture.config.to_str().unwrap(),
            "--json",
        ])
        .current_dir(cwd.path())
        .output()
        .expect("capabilities run");
    assert!(
        caps.status.success(),
        "{}",
        String::from_utf8_lossy(&caps.stderr)
    );
    let caps: Value = serde_json::from_slice(&caps.stdout).expect("capabilities JSON");
    let general = caps["configured"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "general")
        .expect("configured general");
    assert_eq!(general["skills"], serde_json::json!(["tone"]));
}

#[test]
fn missing_malformed_duplicate_and_unresolved_references_fail_before_serving() {
    let cwd = tempfile::tempdir().expect("unrelated cwd");

    let cases: Vec<Case> = vec![
        (
            "missing agents directory",
            Box::new(|fixture, _| {
                let text = std::fs::read_to_string(&fixture.config).unwrap();
                std::fs::write(
                    &fixture.config,
                    text.replace("agents = \"assets/agents\"", "agents = \"missing/agents\""),
                )
                .unwrap();
            }),
            "asset.agent_missing",
        ),
        (
            "malformed agent definition",
            Box::new(|fixture, _| {
                std::fs::write(fixture.agents.join("general.toml"), "name = [").unwrap();
            }),
            "asset.agent_malformed",
        ),
        (
            "duplicate loaded agent",
            Box::new(|fixture, _| {
                std::fs::copy(
                    fixture.agents.join("general.toml"),
                    fixture.agents.join("duplicate.toml"),
                )
                .unwrap();
            }),
            "asset.agent_duplicate",
        ),
        (
            "unresolved prompt",
            Box::new(|fixture, _| {
                let path = fixture.agents.join("general.toml");
                let text = std::fs::read_to_string(&path).unwrap();
                std::fs::write(
                    path,
                    text.replace("prompt = \"general\"", "prompt = \"absent\""),
                )
                .unwrap();
            }),
            "asset.prompt_missing",
        ),
        (
            "unresolved skill",
            Box::new(|fixture, _| {
                let path = fixture.agents.join("general.toml");
                let text = std::fs::read_to_string(&path).unwrap();
                std::fs::write(
                    path,
                    text.replace("skills = [\"tone\"]", "skills = [\"absent\"]"),
                )
                .unwrap();
            }),
            "asset.skill_missing",
        ),
        (
            "unresolved tool",
            Box::new(|fixture, _| {
                let path = fixture.agents.join("general.toml");
                let text = std::fs::read_to_string(&path).unwrap();
                std::fs::write(
                    path,
                    text.replace(
                        "tools = [\"exec_command\", \"save_artifact\", \"remember_memory\"]",
                        "tools = [\"absent\"]",
                    ),
                )
                .unwrap();
            }),
            "asset.tool_unresolved",
        ),
        (
            "duplicate manual and loaded id",
            Box::new(|fixture, _| {
                let path = fixture.agents.join("general.toml");
                let text = std::fs::read_to_string(&path).unwrap();
                std::fs::write(
                    path,
                    text.replace("name = \"general\"", "name = \"manual\""),
                )
                .unwrap();
            }),
            "asset.agent_duplicate",
        ),
        (
            "missing default agent",
            Box::new(|fixture, _| {
                let text = std::fs::read_to_string(&fixture.config).unwrap();
                std::fs::write(
                    &fixture.config,
                    text.replace("default_agent = \"general\"", "default_agent = \"absent\""),
                )
                .unwrap();
            }),
            "asset.default_agent_missing",
        ),
    ];

    for (name, mutate, code) in cases {
        let data = tempfile::tempdir().expect("data");
        let fixture = fixture::write(
            data.path(),
            fixture::Options {
                instance: "failure".into(),
                token: Some("dev-token".into()),
                model: "deterministic".into(),
                principals: Vec::new(),
                settings: Vec::new(),
                token_scopes: vec!["*".into()],
            },
        );
        mutate(&fixture, data.path());
        let result = start_output(&fixture.config, cwd.path());
        let stdout = String::from_utf8_lossy(&result.stdout);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{name} unexpectedly served");
        assert!(!stdout.contains("listening on"), "{name}: {stdout}");
        assert!(stderr.contains(code), "{name} must report {code}: {stderr}");
        assert!(!stderr.contains("sk-"), "{name} leaked a secret: {stderr}");
    }
}

#[test]
fn revision_pinning_and_failed_reload_preserve_active_assets() {
    let cwd = tempfile::tempdir().expect("unrelated cwd");
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "revisions".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    fixture::set_agent_prompt(&fixture, "general", "PROMPT-V1");
    let worker = spawn(&fixture.config, cwd.path());

    let slow = worker.submit("general", "delay:700 prompt:", "revision-old");
    assert_eq!(slow["state"], "accepted");
    let slow_id = slow["task_id"].as_str().unwrap().to_string();

    fixture::set_agent_prompt(&fixture, "general", "PROMPT-V2");
    Command::new("kill")
        .args(["-HUP", &worker.pid().to_string()])
        .status()
        .expect("SIGHUP");
    let deadline = Instant::now() + Duration::from_secs(5);
    while worker.status()["config_revision"].as_u64().unwrap_or(0) < 2 {
        assert!(Instant::now() < deadline, "reload did not activate");
        std::thread::sleep(Duration::from_millis(25));
    }

    let new = worker.run("general", "prompt:", "revision-new");
    assert!(output(&new).contains("PROMPT-V2"), "{new}");
    assert_eq!(new["asset_revision"], 2);

    let old = worker.wait(&slow_id);
    assert!(output(&old).contains("PROMPT-V1"), "{old}");
    assert_eq!(old["asset_revision"], 1);

    // A malformed candidate is rejected and the active revision stays.
    std::fs::write(fixture.agents.join("general.toml"), "name = [").unwrap();
    Command::new("kill")
        .args(["-HUP", &worker.pid().to_string()])
        .status()
        .expect("SIGHUP");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(worker.status()["config_revision"], 2);
    let after_failure = worker.run("general", "prompt:", "revision-failed");
    assert!(
        output(&after_failure).contains("PROMPT-V2"),
        "{after_failure}"
    );
    assert_eq!(after_failure["asset_revision"], 2);
}
