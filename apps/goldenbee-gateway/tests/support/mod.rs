//! Cross-process fixture: two real General Workers plus one Gateway.

#![allow(dead_code)]

use common::{Limits, SubmitTaskRequest, Task};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Output, Stdio};

pub const GATEWAY_TOKEN: &str = "caller-token";
pub const OTHER_CALLER_TOKEN: &str = "only-b-token";
pub const READONLY_TOKEN: &str = "readonly-token";
pub const ADMIN_TOKEN: &str = "admin-token";
pub const WORKER_A_TOKEN: &str = "worker-a-token";
pub const WORKER_B_TOKEN: &str = "worker-b-token";
pub const DELEGATION_SECRET: &str = "t04-shared-delegation-secret";

pub struct Worker {
    child: Child,
    data: tempfile::TempDir,
    env: Vec<(String, String)>,
    pub base: String,
    pub instance: String,
    pub token: String,
    binary: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn binary(name: &str) -> PathBuf {
    if let Ok(path) = std::env::var(format!("CARGO_BIN_EXE_{name}")) {
        return PathBuf::from(path);
    }
    // `CARGO_BIN_EXE_*` is only defined for the package under test. For the
    // workspace-wide run, resolve sibling package binaries from this test
    // executable's target directory. Artifact output may be nested under
    // per-package build directories, so walk ancestors.
    let current = std::env::current_exe().expect("test executable path");
    let mut directory = current.parent();
    while let Some(candidate_directory) = directory {
        let candidate = candidate_directory.join(name);
        if candidate.is_file() {
            return candidate;
        }
        directory = candidate_directory.parent();
    }
    panic!("{name} is missing; build workspace binaries before running cross-process tests");
}

fn worker_config(data: &Path, token: &str, env: &[(String, String)], package: &str) -> PathBuf {
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
    let tools = if package == "general" {
        "\ntools = [\"exec_command\", \"save_artifact\"]"
    } else {
        ""
    };
    std::fs::write(
        agents.join("general.toml"),
        format!(
            "name = \"general\"\ndescription = \"General Worker\"\nmodel = \"deterministic\"\nprompt = \"general\"{tools}\n"
        ),
    )
    .expect("general agent");
    std::fs::write(
        agents.join("echo.toml"),
        "name = \"echo\"\ndescription = \"Echo Worker\"\nmodel = \"deterministic\"\nprompt = \"echo\"\n",
    )
    .expect("echo agent");
    let limits = [
        "GOLDENBEE_MAX_ACTIVE",
        "GOLDENBEE_MAX_QUEUE",
        "GOLDENBEE_PER_PRINCIPAL",
        "GOLDENBEE_EVENT_RETENTION",
        "GOLDENBEE_MAX_STREAMS",
        "GOLDENBEE_STREAM_BUFFER",
        "GOLDENBEE_STREAM_KEEPALIVE_MS",
    ]
    .iter()
    .filter_map(|name| {
        env.iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| {
                let field = name.trim_start_matches("GOLDENBEE_").to_ascii_lowercase();
                format!("{field} = {value}\n")
            })
    })
    .collect::<String>();
    let mut config = format!(
        "instance = \"gateway-fixture-{}\"\nnamespace = \"local\"\ndefault_agent = \"general\"\ntoken = \"{token}\"\ntoken_scopes = [\"*\"]\n{limits}\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n",
        data.file_name().unwrap().to_string_lossy(),
        data.display()
    );
    if let Some((_, principals)) = env.iter().find(|(key, _)| key == "GOLDENBEE_PRINCIPALS") {
        let entries: Vec<Value> =
            serde_json::from_str(principals).expect("principal fixture is JSON");
        for entry in entries {
            let scopes = entry["scopes"]
                .as_array()
                .expect("principal scopes")
                .iter()
                .map(|scope| format!("\"{}\"", scope.as_str().expect("scope text")))
                .collect::<Vec<_>>()
                .join(", ");
            config.push_str(&format!(
                "\n[[tokens]]\ntoken = \"{}\"\nnamespace = \"{}\"\nprincipal = \"{}\"\nscopes = [{}]\n",
                entry["token"].as_str().expect("token"),
                entry["namespace"].as_str().expect("namespace"),
                entry["principal"].as_str().expect("principal"),
                scopes
            ));
        }
    }
    let path = data.join("worker.toml");
    std::fs::write(&path, config).expect("worker config");
    path
}

fn spawn_named_worker(
    binary_name: &str,
    data: &Path,
    token: &str,
    env: &[(String, String)],
    addr: &str,
) -> (Child, String, String) {
    let config = worker_config(data, token, env, binary_name);
    let mut command = Command::new(binary(binary_name));
    command
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            addr,
        ])
        .env("GOLDENBEE_DELEGATION_SECRET", DELEGATION_SECRET)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("general worker starts");
    let stdout = child.stdout.take().expect("worker stdout is piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("worker announces its listener");
    let base = line
        .split("listening on ")
        .nth(1)
        .unwrap_or_else(|| panic!("worker address announcement: {line}"))
        .trim()
        .to_string();
    let instance = reqwest::blocking::Client::new()
        .get(format!("{base}/v1/health"))
        .send()
        .expect("worker health responds")
        .json::<Value>()
        .expect("worker health is JSON")["instance"]
        .as_str()
        .expect("worker instance identity")
        .to_string();
    (child, base, instance)
}

fn start_worker(token: &str, env: Vec<(String, String)>) -> Worker {
    start_named_worker("general", token, env)
}

fn start_named_worker(binary_name: &str, token: &str, env: Vec<(String, String)>) -> Worker {
    let data = tempfile::tempdir().expect("worker data directory");
    let (child, base, instance) =
        spawn_named_worker(binary_name, data.path(), token, &env, "127.0.0.1:0");
    Worker {
        child,
        data,
        env,
        base,
        instance,
        token: token.to_string(),
        binary: binary_name.to_string(),
    }
}

pub struct Stack {
    _config: tempfile::TempDir,
    config_path: PathBuf,
    gateway_env: Vec<(String, String)>,
    _stdout: BufReader<ChildStdout>,
    gateway: Child,
    pub gateway_base: String,
    pub worker_a: Worker,
    pub worker_b: Worker,
    pub worker_c: Option<Worker>,
}

impl Drop for Stack {
    fn drop(&mut self) {
        let _ = self.gateway.kill();
        let _ = self.gateway.wait();
    }
}

fn spawn_gateway(
    config_path: &Path,
    env: &[(String, String)],
) -> (Child, BufReader<ChildStdout>, String) {
    let mut command = Command::new(binary("gb-gateway"));
    command
        .args([
            "serve",
            "--addr",
            "127.0.0.1:0",
            "--config",
            config_path.to_str().expect("configuration path"),
        ])
        .env("GOLDENBEE_DELEGATION_SECRET", DELEGATION_SECRET)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut gateway = command.spawn().expect("gateway starts");
    let stdout = gateway.stdout.take().expect("gateway stdout is piped");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("gateway announces its listener");
    let gateway_base = line
        .split("listening on ")
        .nth(1)
        .unwrap_or_else(|| panic!("gateway address announcement: {line}"))
        .trim()
        .to_string();
    (gateway, reader, gateway_base)
}

/// Options controlling cross-process fixture startup.
#[derive(Debug, Default)]
pub struct Options {
    /// Configure `worker-a` with an endpoint identity that cannot match.
    pub instance_mismatch: bool,
    /// Leave `worker-a`'s instance unpinned so a restarted process identity can
    /// be verified by a Gateway restart.
    pub unpinned_worker_a: bool,
    /// Gateway-side fault marker directory.
    pub gateway_faults: Option<PathBuf>,
    /// `worker-a` fault marker directory.
    pub worker_a_faults: Option<PathBuf>,
    /// Extra `worker-a` environment, including operator admission limits.
    pub worker_a_env: Vec<(String, String)>,
    /// Gateway-side concurrent stream limit. Omitted uses the control-plane
    /// default.
    pub gateway_max_streams: Option<usize>,
    /// Gateway dispatch claim duration. Omitted uses the control-plane default.
    pub gateway_claim_ttl_ms: Option<i64>,
    /// Start an independently built `echo` Worker Package as `worker-c`.
    pub with_echo_worker: bool,
    /// Optional `socket` operator configuration object.
    pub socket: Option<serde_json::Value>,
}

impl Stack {
    pub fn start() -> Self {
        Self::start_with_options(Options::default())
    }

    pub fn start_with_instance_mismatch() -> Self {
        Self::start_with_options(Options {
            instance_mismatch: true,
            ..Options::default()
        })
    }

    /// Start the stack with operator admission limits applied to Worker A.
    pub fn start_with_worker_env(env: &[(&str, String)]) -> Self {
        Self::start_with_options(Options {
            worker_a_env: env
                .iter()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect(),
            ..Options::default()
        })
    }

    pub fn start_with_options(options: Options) -> Self {
        let mut worker_a_env = options.worker_a_env.clone();
        if let Some(path) = &options.worker_a_faults {
            worker_a_env.push((
                "GOLDENBEE_FAULT_DIR".to_string(),
                path.display().to_string(),
            ));
        }
        let worker_a = start_worker(WORKER_A_TOKEN, worker_a_env);
        let tenant_profile = serde_json::json!([{
            "token": "tenant-b-operator",
            "namespace": "tenant-b",
            "principal": "tenant-b-operator",
            "scopes": ["*"]
        }])
        .to_string();
        let worker_b = start_worker(
            WORKER_B_TOKEN,
            vec![("GOLDENBEE_PRINCIPALS".to_string(), tenant_profile)],
        );
        let worker_c = options
            .with_echo_worker
            .then(|| start_named_worker("echo", "worker-c-token", Vec::new()));
        let instance_mismatch = options.instance_mismatch;
        let worker_a_instance = if instance_mismatch {
            "worker-instance-that-does-not-match".to_string()
        } else {
            worker_a.instance.clone()
        };
        let config = tempfile::tempdir().expect("gateway configuration directory");
        let path = config.path().join("gateway.json");
        let data_dir = config.path().join("state");
        let mut worker_a_config = serde_json::json!({
            "id": "worker-a",
            "endpoint": worker_a.base
        });
        if !options.unpinned_worker_a {
            worker_a_config["instance"] = serde_json::json!(worker_a_instance);
        }
        let mut document = serde_json::json!({
            "listen": "127.0.0.1:0",
            "data_dir": data_dir,
            "max_streams": options.gateway_max_streams.unwrap_or(256),
            "callers": [
                {
                    "token": GATEWAY_TOKEN,
                    "id": "caller-1",
                    "namespace": "local",
                    "scopes": ["task:submit", "task:read", "task:cancel", "artifact:read", "team:read", "team:write"],
                    "workers": ["worker-a", "worker-down"]
                },
                {
                    "token": OTHER_CALLER_TOKEN,
                    "id": "caller-2",
                    "namespace": "tenant-b",
                    "scopes": ["task:submit", "task:read", "task:cancel", "artifact:read"],
                    "workers": ["worker-b"]
                },
                {
                    "token": ADMIN_TOKEN,
                    "id": "admin",
                    "namespace": "local",
                    "scopes": ["*"]
                },
                {
                    "token": READONLY_TOKEN,
                    "id": "readonly",
                    "namespace": "local",
                    "scopes": ["task:read", "team:read", "profile:read"],
                    "workers": ["worker-a", "worker-b"]
                }
            ],
            "workers": [
                worker_a_config,
                {"id": "worker-b", "endpoint": worker_b.base, "instance": worker_b.instance},
                {"id": "worker-down", "endpoint": "http://127.0.0.1:1"}
            ]
        });
        if let Some(worker_c) = &worker_c {
            document["workers"]
                .as_array_mut()
                .expect("worker list")
                .push(serde_json::json!({
                    "id": "worker-c",
                    "endpoint": worker_c.base,
                    "instance": worker_c.instance
                }));
        }
        if let Some(ttl) = options.gateway_claim_ttl_ms {
            document["claim_ttl_ms"] = serde_json::json!(ttl);
        }
        if let Some(socket) = &options.socket {
            document["socket"] = socket.clone();
        }
        std::fs::write(
            &path,
            serde_json::to_vec(&document).expect("configuration JSON"),
        )
        .expect("gateway configuration is written");
        let mut gateway_env = Vec::new();
        if let Some(path) = &options.gateway_faults {
            gateway_env.push((
                "GOLDENBEE_GATEWAY_FAULT_DIR".to_string(),
                path.display().to_string(),
            ));
        }
        let (gateway, reader, gateway_base) = spawn_gateway(&path, &gateway_env);
        Self {
            _config: config,
            config_path: path,
            gateway_env,
            _stdout: reader,
            gateway,
            gateway_base,
            worker_a,
            worker_b,
            worker_c,
        }
    }

    /// Stop the Gateway and start a new incarnation against the same
    /// configuration and durable dispatch store.
    pub fn restart_gateway(&mut self) {
        let _ = self.gateway.kill();
        let _ = self.gateway.wait();
        let (gateway, reader, base) = spawn_gateway(&self.config_path, &self.gateway_env);
        self.gateway = gateway;
        self._stdout = reader;
        self.gateway_base = base;
    }

    /// Stop `worker-a` while keeping its data directory, simulating an
    /// unreachable remote Worker.
    pub fn stop_worker_a(&mut self) {
        self.worker_a.stop();
    }

    /// Wait for an injected Gateway fault to terminate the process.
    pub fn wait_for_gateway_fault_exit(&mut self) {
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(10) {
            match self
                .gateway
                .try_wait()
                .expect("gb-gateway status is observable")
            {
                Some(status) => {
                    assert!(
                        !status.success(),
                        "the injected fault must terminate the gateway"
                    );
                    return;
                }
                None => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        panic!("the injected fault did not terminate the gateway");
    }

    pub fn client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::new()
    }

    pub fn submit(
        &self,
        token: &str,
        worker: &str,
        agent: &str,
        input: &str,
        session: &str,
        key: &str,
    ) -> reqwest::blocking::Response {
        self.try_submit(token, worker, agent, input, session, key)
            .expect("gateway submit responds")
    }

    pub fn try_submit(
        &self,
        token: &str,
        worker: &str,
        agent: &str,
        input: &str,
        session: &str,
        key: &str,
    ) -> Result<reqwest::blocking::Response, reqwest::Error> {
        Self::client()
            .post(format!("{}/v1/tasks", self.gateway_base))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "worker": worker,
                "agent": agent,
                "session_id": session,
                "input": input,
                "idempotency_key": key
            }))
            .send()
    }

    /// Gateway task projection for one execution id.
    pub fn task(&self, token: &str, id: &str) -> Value {
        Self::client()
            .get(format!("{}/v1/tasks/{id}", self.gateway_base))
            .bearer_auth(token)
            .send()
            .expect("gateway task responds")
            .json()
            .expect("task projection is JSON")
    }

    pub fn wait_task(&self, token: &str, id: &str) -> Value {
        for _ in 0..300 {
            let response = Self::client()
                .get(format!("{}/v1/tasks/{id}", self.gateway_base))
                .bearer_auth(token)
                .send()
                .expect("gateway task responds");
            if response.status().is_success() {
                let projection = response.json::<Value>().expect("task projection is JSON");
                if is_terminal(&projection["task"]["state"]) {
                    return projection;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("gateway task {id} did not reach a terminal state");
    }

    pub fn events(&self, token: &str, id: &str) -> Value {
        Self::client()
            .get(format!("{}/v1/tasks/{id}/events", self.gateway_base))
            .bearer_auth(token)
            .send()
            .expect("gateway events respond")
            .json()
            .expect("event page is JSON")
    }

    /// Open the public Gateway SSE route without consuming the body.
    pub fn stream_raw(&self, token: &str, id: &str, after: u64) -> reqwest::blocking::Response {
        Self::client()
            .get(format!(
                "{}/v1/tasks/{id}/events/stream?after={after}",
                self.gateway_base
            ))
            .bearer_auth(token)
            .send()
            .expect("gateway stream responds")
    }

    /// Read one SSE response body with a bounded per-read timeout.
    pub fn sse(&self, token: &str, id: &str, after: u64) -> Sse {
        let response = Self::client()
            .get(format!(
                "{}/v1/tasks/{id}/events/stream?after={after}",
                self.gateway_base
            ))
            .bearer_auth(token)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .expect("gateway stream responds");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::OK,
            "gateway stream rejected: {}",
            response.status()
        );
        Sse {
            reader: BufReader::new(response),
        }
    }

    pub fn cancel(&self, token: &str, id: &str) -> reqwest::blocking::Response {
        Self::client()
            .post(format!("{}/v1/tasks/{id}/cancel", self.gateway_base))
            .bearer_auth(token)
            .send()
            .expect("gateway cancel responds")
    }

    pub fn kill_gateway(&mut self) {
        let _ = self.gateway.kill();
        let _ = self.gateway.wait();
    }

    /// Live Gateway process identity for bounded measurement sampling.
    pub fn gateway_pid(&self) -> u32 {
        self.gateway.id()
    }

    /// Gateway-owned durable state directory.
    pub fn gateway_data_dir(&self) -> PathBuf {
        self._config.path().join("state")
    }

    pub fn cli(&self, arguments: &[&str]) -> Output {
        self.cli_as(GATEWAY_TOKEN, arguments)
    }

    /// Run the CLI with an explicit caller token. The Gateway URL is always
    /// explicit so no stored profile can influence the result.
    pub fn cli_as(&self, token: &str, arguments: &[&str]) -> Output {
        Command::new(binary("gb-client"))
            .arg("--gateway")
            .arg(&self.gateway_base)
            .arg("--token")
            .arg(token)
            .args(arguments)
            .output()
            .expect("cli runs")
    }
}

pub fn is_terminal(state: &Value) -> bool {
    matches!(
        state.as_str(),
        Some("succeeded" | "failed" | "cancelled" | "unknown")
    )
}

/// Raw SSE reader for public Gateway streams.
pub struct Sse {
    reader: BufReader<reqwest::blocking::Response>,
}

impl Sse {
    /// Read the next JSON event, skipping keepalive comments.
    pub fn next_event(&mut self) -> Option<common::StreamEvent> {
        let mut data = String::new();
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).ok()?;
            if read == 0 {
                return None;
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !data.is_empty() {
                    return serde_json::from_str(&data).ok();
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("data:") {
                data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            }
        }
    }

    /// Read until a terminal task event.
    pub fn until_terminal(&mut self) -> Vec<common::StreamEvent> {
        let mut events = Vec::new();
        while let Some(event) = self.next_event() {
            let terminal = matches!(
                event.kind.as_str(),
                "task.succeeded" | "task.failed" | "task.cancelled" | "task.unknown"
            );
            events.push(event);
            if terminal {
                return events;
            }
        }
        events
    }
}

impl Worker {
    pub fn client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::new()
    }

    /// Live Worker process identity for bounded measurement sampling.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Worker-owned durable state directory.
    pub fn data_dir(&self) -> &Path {
        self.data.path()
    }

    /// Stop the process and start a new incarnation against the same data
    /// directory and environment.
    pub fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let addr = self
            .base
            .strip_prefix("http://")
            .unwrap_or(&self.base)
            .to_string();
        let (child, base, instance) = spawn_named_worker(
            &self.binary,
            self.data.path(),
            &self.token,
            &self.env,
            &addr,
        );
        self.child = child;
        self.base = base;
        self.instance = instance;
    }

    /// Stop the process, keeping its durable data for a later restart.
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Wait for an injected fault to terminate the process.
    pub fn wait_for_fault_exit(&mut self) {
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(10) {
            match self.child.try_wait().expect("worker status is observable") {
                Some(status) => {
                    assert!(
                        !status.success(),
                        "the injected fault must terminate the worker"
                    );
                    return;
                }
                None => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        panic!("the injected fault did not terminate the worker");
    }

    pub fn tasks(&self) -> Vec<Task> {
        self.tasks_with(&self.token)
    }

    pub fn tasks_with(&self, token: &str) -> Vec<Task> {
        Worker::client()
            .get(format!("{}/v1/tasks", self.base))
            .bearer_auth(token)
            .send()
            .expect("worker task list responds")
            .json()
            .expect("worker task list is JSON")
    }

    pub fn task(&self, id: &str) -> Task {
        Worker::client()
            .get(format!("{}/v1/tasks/{id}", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("worker task responds")
            .json()
            .expect("worker task is JSON")
    }

    pub fn events(&self, id: &str) -> Value {
        Worker::client()
            .get(format!("{}/v1/tasks/{id}/events", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("worker events respond")
            .json()
            .expect("worker event page is JSON")
    }

    pub fn status(&self) -> Value {
        Worker::client()
            .get(format!("{}/v1/status", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("worker status responds")
            .json()
            .expect("worker status is JSON")
    }

    pub fn submit(&self, agent: &str, input: &str, session: &str, key: &str) -> Value {
        let response = Worker::client()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(&self.token)
            .json(&SubmitTaskRequest {
                namespace: None,
                agent: agent.into(),
                session_id: session.into(),
                input: input.into(),
                idempotency_key: key.into(),
                profile: None,
                team: None,
                worker: None,
                limits: Limits::default(),
            })
            .send()
            .expect("direct worker submit responds");
        assert!(
            response.status().is_success(),
            "direct submit failed: {}",
            response.status()
        );
        response.json().expect("direct submit is JSON")
    }

    pub fn wait_direct(&self, id: &str) -> Task {
        for _ in 0..300 {
            let task = self.task(id);
            if task.state.is_terminal() {
                return task;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("worker task {id} did not reach a terminal state");
    }
}
