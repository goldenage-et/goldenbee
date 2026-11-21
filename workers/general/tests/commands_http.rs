//! Command discovery and invocation over the Worker HTTP surface.
//!
//! The same registry and admission path serve the CLI, so this test asserts the
//! external contract: a client can list neutral and Worker commands, execute a
//! neutral command, submit a Worker command, and observe the narrowed task.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod fixture;

struct Served {
    child: Child,
    base: String,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fixture(data: &Path) -> fixture::Fixture {
    fixture::write(
        data,
        fixture::Options {
            instance: "commands-http".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    )
}

fn start(config: &Path) -> Served {
    let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("worker serves");
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("listener announcement");
    let base = line
        .split("listening on ")
        .nth(1)
        .unwrap_or_else(|| panic!("no listener announcement: {line}"))
        .trim()
        .to_string();
    Served { child, base }
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client")
}

fn get(_served: &Served, base: &str, path: &str) -> Value {
    client()
        .get(format!("{base}{path}"))
        .bearer_auth("dev-token")
        .send()
        .expect("response")
        .json()
        .expect("json")
}

fn post(served: &Served, base: &str, path: &str, body: Value) -> Value {
    let _ = served;
    client()
        .post(format!("{base}{path}"))
        .bearer_auth("dev-token")
        .json(&body)
        .send()
        .expect("response")
        .json()
        .expect("json")
}

#[test]
fn commands_discover_and_behave_consistently_over_http() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path());
    std::fs::write(
        fixture.commands.join("summarize.toml"),
        "description = \"Summarize a topic\"\nprompt = \"HTTP-COMMAND {{args}}\"\nallowed_tools = [\"save_artifact\"]\n",
    )
    .expect("command asset");
    let served = start(&fixture.config);
    let base = served.base.clone();

    let capabilities = get(&served, &base, "/v1/capabilities");
    let commands = capabilities["commands"]
        .as_array()
        .expect("capability command inventory");
    assert!(commands
        .iter()
        .any(|command| command["name"] == "session.info"));
    assert!(commands
        .iter()
        .any(|command| command["name"] == "summarize"));

    let listed = get(&served, &base, "/v1/commands");
    let listed = listed.as_array().expect("command list");
    assert!(listed.iter().any(|command| command["name"] == "summarize"));
    let summarize = listed
        .iter()
        .find(|command| command["name"] == "summarize")
        .expect("worker command");
    assert_eq!(summarize["kind"], "worker");
    assert_eq!(summarize["allowed_tools"][0], "save_artifact");

    let neutral = post(
        &served,
        &base,
        "/v1/commands/context.info",
        json!({"session_id": "http-neutral"}),
    );
    assert_eq!(neutral["kind"], "neutral");
    assert_eq!(neutral["result"]["default_agent"], "general");

    let submitted = post(
        &served,
        &base,
        "/v1/commands/summarize",
        json!({
            "input": "prompt: release notes",
            "session_id": "http-command",
            "idempotency_key": "http-command-1",
        }),
    );
    assert_eq!(submitted["kind"], "worker");
    let task_id = submitted["task"]["task_id"]
        .as_str()
        .expect("task id")
        .to_string();

    let deadline = Instant::now() + Duration::from_secs(20);
    let task = loop {
        let task = get(&served, &base, &format!("/v1/tasks/{task_id}"));
        if task["state"]
            .as_str()
            .is_some_and(|state| !matches!(state, "accepted" | "queued" | "running" | "cancelling"))
        {
            break task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task}");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(task["state"], "succeeded", "{task}");
    assert!(
        task["output"]
            .as_str()
            .unwrap_or_default()
            .contains("prompt=HTTP-COMMAND prompt: release notes"),
        "{task}"
    );

    let before = get(&served, &base, "/v1/tasks")
        .as_array()
        .expect("tasks")
        .len();
    let malformed = client()
        .post(format!("{base}/v1/commands/summarize"))
        .bearer_auth("dev-token")
        .json(&json!({
            "input": "\"unterminated",
            "session_id": "http-command",
            "idempotency_key": "http-command-2",
        }))
        .send()
        .expect("response");
    assert!(
        malformed.status().is_client_error(),
        "{}",
        malformed.status()
    );
    let after = get(&served, &base, "/v1/tasks")
        .as_array()
        .expect("tasks")
        .len();
    assert_eq!(
        before, after,
        "malformed command input must not admit a task"
    );
}

#[test]
fn commands_are_discoverable_and_executable_from_cli() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path());
    std::fs::write(
        fixture.commands.join("summarize.toml"),
        "description = \"Summarize a topic\"\nprompt = \"CLI-COMMAND {{args}}\"\n",
    )
    .expect("command asset");
    let config = fixture.config.to_str().expect("config path");

    let listed = Command::new(env!("CARGO_BIN_EXE_general"))
        .args(["commands", "list", "--config", config, "--json"])
        .output()
        .expect("commands list");
    assert!(listed.status.success(), "{listed:?}");
    let commands: Vec<Value> =
        serde_json::from_slice(&listed.stdout).expect("command inventory JSON");
    assert!(commands
        .iter()
        .any(|command| command["name"] == "session.info"));
    assert!(commands
        .iter()
        .any(|command| command["name"] == "summarize"));

    let context = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "commands",
            "run",
            "--config",
            config,
            "--command",
            "context.info",
            "--json",
        ])
        .output()
        .expect("commands run");
    assert!(context.status.success(), "{context:?}");
    let response: Value = serde_json::from_slice(&context.stdout).expect("command response JSON");
    assert_eq!(response["kind"], "neutral");
    assert_eq!(response["result"]["default_agent"], "general");
}
