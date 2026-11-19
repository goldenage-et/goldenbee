//! Shared cross-process fixture for the Knowledge Worker Package.
#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Fixture {
    pub root: PathBuf,
    pub config: PathBuf,
    pub assets: PathBuf,
    pub agents: PathBuf,
    pub prompts: PathBuf,
    pub sources: PathBuf,
    pub data_dir: PathBuf,
}

pub struct Options {
    pub instance: String,
    pub token: Option<String>,
    pub model: String,
    pub principals: Vec<(String, String, String)>,
    pub settings: Vec<(String, String)>,
    pub source_roots: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            instance: "knowledge-test".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            source_roots: Some("sources".into()),
        }
    }
}

pub fn write(root: &Path, options: Options) -> Fixture {
    let assets = root.join("assets");
    let agents = assets.join("agents");
    let prompts = assets.join("prompts");
    let skills = assets.join("skills").join("citation");
    let sources = root.join("sources");
    let data_dir = root.join("state");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::create_dir_all(&skills).expect("skill directory");
    std::fs::create_dir_all(&sources).expect("source directory");
    std::fs::write(
        skills.join("SKILL.md"),
        "---\nname: citation\ndescription: Cite exact source revisions.\n---\nUse returned citation tokens.",
    )
    .expect("skill");
    std::fs::write(
        prompts.join("knowledge.md"),
        "Answer from retrieved chunks and cite every fact with the returned token.",
    )
    .expect("prompt");
    std::fs::write(
        agents.join("knowledge.toml"),
        format!(
            "name = \"knowledge\"\ndescription = \"knowledge file agent\"\nmodel = \"{}\"\nprompt = \"knowledge\"\ntools = [\"knowledge_ingest\", \"knowledge_ask\", \"knowledge_remove\"]\nskills = [\"citation\"]\n",
            options.model
        ),
    )
    .expect("agent definition");

    let mut config = String::new();
    config.push_str(&format!("instance = \"{}\"\n", options.instance));
    config.push_str("namespace = \"local\"\ndefault_agent = \"knowledge\"\n");
    if let Some(token) = &options.token {
        config.push_str(&format!("token = \"{token}\"\n"));
        config.push_str("token_scopes = [\"*\"]\n");
    }
    for (token, namespace, principal) in &options.principals {
        config.push_str(&format!(
            "\n[[tokens]]\ntoken = \"{token}\"\nnamespace = \"{namespace}\"\nprincipal = \"{principal}\"\nscopes = [\"task:submit\", \"task:read\", \"session:write\", \"artifact:read\", \"artifact:write\"]\n"
        ));
    }
    config.push_str(
        "\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\nskills = \"assets/skills\"\n",
    );
    config.push_str(&format!(
        "\n[storage]\ndirectory = \"{}\"\n",
        data_dir.display()
    ));
    config.push_str("\n[settings]\n");
    if let Some(roots) = &options.source_roots {
        config.push_str(&format!("source_roots = \"{roots}\"\n"));
    }
    for (key, value) in &options.settings {
        config.push_str(&format!("{key} = \"{value}\"\n"));
    }
    let config_path = root.join("worker.toml");
    std::fs::write(&config_path, config).expect("worker config");
    Fixture {
        root: root.to_path_buf(),
        config: config_path,
        assets,
        agents,
        prompts,
        sources,
        data_dir,
    }
}

pub fn run(fixture: &Fixture, input: &str) -> std::process::Output {
    run_in(&fixture.config, fixture.root.as_path(), input)
}

pub fn run_in(config: &Path, cwd: &Path, input: &str) -> std::process::Output {
    run_binary(
        std::path::Path::new(env!("CARGO_BIN_EXE_knowledge")),
        config,
        cwd,
        input,
    )
}

pub fn run_binary(binary: &Path, config: &Path, cwd: &Path, input: &str) -> std::process::Output {
    Command::new(binary)
        .args([
            "run",
            "--config",
            config.to_str().expect("config path"),
            "--input",
            input,
            "--session",
            "fixture-session",
        ])
        .current_dir(cwd)
        .output()
        .expect("knowledge run executes")
}

pub fn output_json(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let start = stdout.find('{').unwrap_or_else(|| {
        panic!(
            "no JSON object in stdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let text = stdout[start..].trim();
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON: {error}\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// Extract the deterministic model's `knowledge:` payload as JSON.
pub fn knowledge_payload(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let marker = stdout.find("knowledge:").unwrap_or_else(|| {
        panic!(
            "no knowledge payload in stdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let rest = &stdout[marker + "knowledge:".len()..];
    let start = rest
        .find('{')
        .unwrap_or_else(|| panic!("knowledge payload has no JSON object: {rest}"));
    let text = rest[start..].trim();
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("knowledge payload is not JSON: {error}\n{text}"))
}

pub fn knowledge_payload_from_stdout(stdout: &str) -> serde_json::Value {
    payload_from_text(stdout)
}

pub fn payload_from_text(text: &str) -> serde_json::Value {
    let marker = text
        .find("knowledge:")
        .unwrap_or_else(|| panic!("no knowledge payload in: {text}"));
    let rest = &text[marker + "knowledge:".len()..];
    let start = rest
        .find('{')
        .unwrap_or_else(|| panic!("knowledge payload has no JSON object: {rest}"));
    let mut stream =
        serde_json::Deserializer::from_str(&rest[start..]).into_iter::<serde_json::Value>();
    stream
        .next()
        .unwrap_or_else(|| panic!("knowledge payload has no JSON value: {rest}"))
        .unwrap_or_else(|error| panic!("knowledge payload is not JSON: {error}\n{rest}"))
}

pub struct Worker {
    pub child: Child,
    pub base: String,
    pub token: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn spawn(config: &Path, cwd: &Path, token: &str) -> Worker {
    let mut child = Command::new(env!("CARGO_BIN_EXE_knowledge"))
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
        .expect("knowledge serves");
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
        token: token.to_string(),
    }
}

impl Worker {
    pub fn submit_with(&self, token: &str, input: &str, key: &str) -> serde_json::Value {
        let response = reqwest::blocking::Client::new()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "agent": "knowledge",
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

    pub fn wait(&self, task_id: &str) -> serde_json::Value {
        self.wait_as(&self.token.clone(), task_id)
    }

    pub fn wait_as(&self, token: &str, task_id: &str) -> serde_json::Value {
        for _ in 0..1_000 {
            let task: serde_json::Value = reqwest::blocking::Client::new()
                .get(format!("{}/v1/tasks/{task_id}", self.base))
                .bearer_auth(token)
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

    pub fn run_as(&self, token: &str, input: &str, key: &str) -> serde_json::Value {
        let submitted = self.submit_with(token, input, key);
        self.wait_as(token, submitted["task_id"].as_str().expect("task id"))
    }

    pub fn wait_port(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if reqwest::blocking::Client::new()
                .get(format!("{}/v1/health", self.base))
                .send()
                .is_ok()
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}
