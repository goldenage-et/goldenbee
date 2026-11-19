//! Ticket 47 Worker-side security acceptance: hostile model output, package
//! independence, asset revisions, and native hook policy.
//!
//! These tests start the packaged `general` executable as a real process from
//! an unrelated working directory. They observe public task, event, artifact,
//! and capability responses plus explicit files for side effects.

use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

mod fixture;

struct Worker {
    child: Child,
    base: String,
    token: String,
    log: PathBuf,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::new()
}

fn report(step: &str, detail: impl std::fmt::Display) {
    println!("security-47: {step}: {detail}");
}

fn spawn(config: &Path, cwd: &Path, log: &Path) -> Worker {
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
    assert!(base.starts_with("http://127.0.0.1:"), "{line}");
    Worker {
        child,
        base,
        token: "dev-token".into(),
        log: log.to_path_buf(),
    }
}

fn start_output(config: &Path, cwd: &Path) -> Output {
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

impl Worker {
    fn submit(&self, agent: &str, input: &str, key: &str) -> (reqwest::StatusCode, Value) {
        let response = client()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "agent": agent,
                "session_id": format!("security-{key}"),
                "input": input,
                "idempotency_key": key
            }))
            .send()
            .expect("submit responds");
        let status = response.status();
        let text = response.text().expect("submit body");
        (
            status,
            serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text})),
        )
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
        let started = Instant::now();
        loop {
            let task = self.task(id);
            if matches!(
                task["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled" | "unknown")
            ) {
                return task;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "task {id} did not finish: {task}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn run(&self, agent: &str, input: &str, key: &str) -> (reqwest::StatusCode, Value) {
        let (status, submitted) = self.submit(agent, input, key);
        if !status.is_success() {
            return (status, submitted);
        }
        let task = self.wait(submitted["task_id"].as_str().expect("task id"));
        (status, task)
    }

    fn tasks(&self) -> Vec<Value> {
        client()
            .get(format!("{}/v1/tasks", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("tasks respond")
            .json()
            .expect("tasks are JSON")
    }

    fn events(&self, id: &str) -> Vec<Value> {
        let page: Value = client()
            .get(format!("{}/v1/tasks/{id}/events", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("events respond")
            .json()
            .expect("events are JSON");
        page["events"].as_array().cloned().unwrap_or_default()
    }

    fn artifacts(&self, id: &str) -> Vec<Value> {
        client()
            .get(format!("{}/v1/tasks/{id}/artifacts", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("artifacts respond")
            .json()
            .expect("artifacts are JSON")
    }

    fn status(&self) -> Value {
        client()
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

fn options(instance: &str, settings: Vec<(String, String)>) -> fixture::Options {
    fixture::Options {
        instance: instance.into(),
        token: Some("dev-token".into()),
        model: "deterministic".into(),
        principals: Vec::new(),
        settings,
        token_scopes: vec!["*".into()],
    }
}

#[test]
fn hostile_model_tool_calls_cannot_expand_an_agent_without_tools() {
    let cwd = tempfile::tempdir().expect("unrelated cwd");
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("stderr.log");
    let canary = data.path().join("host-escape-canary");
    let fixture = fixture::write(data.path(), options("hostile-tools", Vec::new()));
    let worker = spawn(&fixture.config, cwd.path(), &log);

    // The `echo` agent has no tools. Each input asks the deterministic native
    // model to emit a tool call it is not allowed to have. The model can only
    // request; admission, the installed tool ceiling, and the OS boundary must
    // still deny the effect.
    let cases = [
        (
            "sandbox",
            format!("sandbox:touch {}", canary.display()),
            "host escape",
        ),
        (
            "exact call",
            format!(
                "call:exec_command {{\"command\":\"touch {}\"}}",
                canary.display()
            ),
            "exact call escape",
        ),
        ("artifact", "artifact:HOSTILE-ARTIFACT".into(), "artifact"),
        ("effect", "effect:HOSTILE-EFFECT".into(), "effect"),
        (
            "mcp",
            "mcp:hidden_effect {\"value\":\"HIDDEN-MCP\"}".into(),
            "mcp",
        ),
    ];
    let mut ids = Vec::new();
    for (label, input, key) in cases {
        let (status, task) = worker.run("echo", &input, key);
        assert!(
            status.is_success(),
            "{label} submission was not recorded: {status} {task}"
        );
        assert!(
            matches!(
                task["state"].as_str(),
                Some("succeeded" | "failed" | "unknown")
            ),
            "{label}: {task}"
        );
        assert!(!canary.exists(), "{label} escaped the sandbox: {task}");
        let artifacts = worker.artifacts(task["id"].as_str().expect("task id"));
        assert!(
            artifacts.is_empty(),
            "{label} created an artifact: {artifacts:?}"
        );
        let id = task["id"].as_str().expect("task id").to_string();
        assert!(!ids.contains(&id), "{label} reused a task identity");
        ids.push(id);
    }

    // Unrelated work on the same process still succeeds after the hostile
    // attempts, and no requested content became durable evidence.
    let (status, normal) = worker.run("echo", "normal echo continues", "normal-after");
    assert!(status.is_success());
    assert_eq!(normal["state"], "succeeded", "{normal}");
    assert!(output(&normal).contains("normal echo continues"));
    let tasks = worker.tasks();
    assert_eq!(tasks.len(), 6);
    for id in &ids {
        assert!(
            worker.artifacts(id).is_empty(),
            "hostile request produced durable artifact evidence"
        );
    }
    report(
        "hostile model output",
        format!("tasks={} artifacts=0 sandbox_effect=false", tasks.len()),
    );
}

#[test]
fn hooks_and_native_callbacks_cannot_bypass_policy_on_loaded_or_manual_agents() {
    let cwd = tempfile::tempdir().expect("unrelated cwd");
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("native-hooks.log");
    let fixture = fixture::write(
        data.path(),
        options(
            "hook-security",
            vec![
                ("reject_contains".into(), "forbidden".into()),
                ("after_task_fail".into(), "true".into()),
            ],
        ),
    );
    let mut worker = spawn(&fixture.config, cwd.path(), &log);

    // Mandatory admission runs before any optional hook and rejects an
    // unregistered agent without recording a task.
    let (status, denied) = worker.submit("absent", "must not run", "hook-absent");
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{denied}");
    assert!(worker.tasks().is_empty());

    let (status, rejected) = worker.run("general", "forbidden input", "hook-rejected");
    assert!(status.is_success());
    assert_eq!(rejected["state"], "failed", "{rejected}");
    assert_eq!(rejected["error"]["code"], "policy.before_task_rejected");
    assert!(rejected["output"].is_null());
    assert!(!rejected["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("sk-before-task-secret"));

    let normal = worker.run("general", "hook-normal", "hook-normal").1;
    assert_eq!(normal["state"], "succeeded", "{normal}");
    let started = Instant::now();
    let after = loop {
        if let Some(event) = worker
            .events(normal["id"].as_str().expect("normal task id"))
            .into_iter()
            .find(|event| event["kind"] == "hook.after_task_failed")
        {
            break event;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "after-task failure was not recorded"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(after["payload"]["code"], "policy.after_task_failed");
    assert!(!after["payload"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("sk-after-task-secret"));

    // Native agent callbacks and the plugin attach once to both automatically
    // loaded and manually registered agents.
    for agent in ["general", "manual"] {
        let task = worker
            .run(agent, "plugin: cb:", &format!("markers-{agent}"))
            .1;
        assert_eq!(task["state"], "succeeded", "{agent}: {task}");
        let text = output(&task);
        for marker in [
            "[plugin-user]",
            "[agent-before]",
            "[agent-after]",
            "[plugin-event]",
        ] {
            assert_eq!(
                count(&text, marker),
                1,
                "{agent} must run {marker} once: {text}"
            );
        }
        assert!(
            text.find("[plugin-user]").unwrap() < text.find("[agent-before]").unwrap()
                && text.find("[agent-before]").unwrap() < text.find("[agent-after]").unwrap(),
            "{agent} ordering: {text}"
        );
    }

    // A plugin short-circuit cannot create a task for an unregistered agent or
    // a caller without authority.
    let (status, _) = worker.submit("absent", "plugin-short", "hook-bypass");
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    let denied = client()
        .get(format!("{}/v1/tasks", worker.base))
        .header("Authorization", "Bearer not-a-token")
        .send()
        .expect("unauthorized list responds");
    assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);

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
    assert!(!log.contains("sk-"), "hook failures leaked secrets: {log}");
    report(
        "hooks",
        format!(
            "before_task={} after_task={} manual_and_loaded=true",
            count(&log, "hook:before_task"),
            count(&log, "hook:after_task")
        ),
    );
}

#[test]
fn identical_packages_from_unrelated_cwds_keep_assets_revisions_and_registration_isolated() {
    let cwd = tempfile::tempdir().expect("unrelated cwd");
    let first_data = tempfile::tempdir().expect("first data");
    let second_data = tempfile::tempdir().expect("second data");
    let first_log = first_data.path().join("first.log");
    let second_log = second_data.path().join("second.log");
    let first = fixture::write(first_data.path(), options("package-instance-a", Vec::new()));
    let second = fixture::write(
        second_data.path(),
        options("package-instance-b", Vec::new()),
    );
    fixture::set_agent_prompt(&first, "general", "PACKAGE-PROMPT-A");
    fixture::set_agent_prompt(&second, "general", "PACKAGE-PROMPT-B");

    let first_worker = spawn(&first.config, cwd.path(), &first_log);
    let second_worker = spawn(&second.config, cwd.path(), &second_log);
    assert_eq!(first_worker.status()["package"], "general");
    assert_eq!(second_worker.status()["package"], "general");
    assert_ne!(
        first_worker.status()["instance"],
        second_worker.status()["instance"]
    );

    let first_task = first_worker.run("general", "prompt:", "package-a").1;
    let second_task = second_worker.run("general", "prompt:", "package-b").1;
    assert!(
        output(&first_task).contains("PACKAGE-PROMPT-A"),
        "{first_task}"
    );
    assert!(
        output(&second_task).contains("PACKAGE-PROMPT-B"),
        "{second_task}"
    );
    assert!(!output(&first_task).contains("PACKAGE-PROMPT-B"));
    assert!(!output(&second_task).contains("PACKAGE-PROMPT-A"));

    // The other instance's store does not expose the first task, even when the
    // package binary and protocol are identical.
    let foreign = client()
        .get(format!(
            "{}/v1/tasks/{}",
            second_worker.base,
            first_task["id"].as_str().expect("first task id")
        ))
        .bearer_auth(&second_worker.token)
        .send()
        .expect("foreign task responds");
    assert_eq!(foreign.status(), reqwest::StatusCode::NOT_FOUND);

    // Automatic loading exposes both agent capabilities without recompiling
    // the shared package binary.
    let caps: Value = client()
        .get(format!("{}/v1/capabilities", first_worker.base))
        .bearer_auth(&first_worker.token)
        .send()
        .expect("capabilities respond")
        .json()
        .expect("capabilities are JSON");
    assert_eq!(caps["package"], "general");
    let configured = caps["capabilities"].as_array().expect("capabilities");
    assert!(configured.iter().any(|entry| entry["id"] == "general"));
    assert!(configured.iter().any(|entry| entry["id"] == "echo"));

    // Revision pinning: an active invocation keeps its admitted prompt while a
    // complete candidate reload affects new invocations only.
    let slow = first_worker.submit("general", "delay:700 prompt:", "revision-old");
    assert!(slow.0.is_success(), "{slow:?}");
    let slow_id = slow.1["task_id"]
        .as_str()
        .expect("slow task id")
        .to_string();
    fixture::set_agent_prompt(&first, "general", "PACKAGE-PROMPT-A-V2");
    Command::new("kill")
        .args(["-HUP", &first_worker.pid().to_string()])
        .status()
        .expect("SIGHUP");
    let deadline = Instant::now() + Duration::from_secs(5);
    while first_worker.status()["config_revision"]
        .as_u64()
        .unwrap_or(0)
        < 2
    {
        assert!(Instant::now() < deadline, "reload did not activate");
        std::thread::sleep(Duration::from_millis(25));
    }
    let new = first_worker.run("general", "prompt:", "revision-new").1;
    assert!(output(&new).contains("PACKAGE-PROMPT-A-V2"), "{new}");
    let old = first_worker.wait(&slow_id);
    assert!(output(&old).contains("PACKAGE-PROMPT-A"), "{old}");
    assert!(!output(&old).contains("PACKAGE-PROMPT-A-V2"), "{old}");

    // Duplicate loaded/manual identifiers fail before serving from an unrelated
    // cwd and never fall back to bundled assets.
    let duplicate_data = tempfile::tempdir().expect("duplicate data");
    let duplicate = fixture::write(
        duplicate_data.path(),
        options("package-duplicate", Vec::new()),
    );
    std::fs::copy(
        duplicate.agents.join("general.toml"),
        duplicate.agents.join("duplicate.toml"),
    )
    .expect("duplicate agent file");
    let failed = start_output(&duplicate.config, cwd.path());
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(!failed.status.success(), "duplicate registration served");
    assert!(stderr.contains("asset.agent_duplicate"), "{stderr}");
    assert!(!String::from_utf8_lossy(&failed.stdout).contains("listening on"));
    assert!(!stderr.contains("sk-"), "{stderr}");
    report(
        "package isolation",
        format!(
            "instances=2 assets=independent revision_old={} revision_new={} duplicate=rejected",
            old["asset_revision"], new["asset_revision"]
        ),
    );
}
