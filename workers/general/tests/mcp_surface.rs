//! Ticket 12: MCP resources, prompts, health, restart, and allowlist parity.
//!
//! These tests assert external behavior through the real Worker: a real stdio
//! MCP child, the instance registries, HTTP management routes, task events, and
//! audit records. They do not assert native MCP internals.

use common::{Limits, Principal, ProfileRevision, SubmitTaskRequest, Task, TaskState};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod fixture;

struct Server {
    id: String,
    pid: PathBuf,
    effect: PathBuf,
    allowed_tools: Vec<String>,
    allowed_resources: Vec<String>,
    protected_resources: Vec<String>,
    allowed_prompts: Vec<String>,
    max_output_bytes: usize,
}

impl Server {
    fn new(directory: &Path, id: &str) -> Self {
        Self {
            id: id.into(),
            pid: directory.join(format!("{id}-pid.txt")),
            effect: directory.join(format!("{id}-effect.txt")),
            allowed_tools: vec!["record_effect".into()],
            allowed_resources: Vec::new(),
            protected_resources: Vec::new(),
            allowed_prompts: Vec::new(),
            max_output_bytes: 256 * 1024,
        }
    }

    fn resources(mut self, resources: &[&str]) -> Self {
        self.allowed_resources = resources.iter().map(|value| (*value).into()).collect();
        self
    }

    fn protected_resources(mut self, resources: &[&str]) -> Self {
        self.protected_resources = resources.iter().map(|value| (*value).into()).collect();
        self
    }

    fn prompts(mut self, prompts: &[&str]) -> Self {
        self.allowed_prompts = prompts.iter().map(|value| (*value).into()).collect();
        self
    }

    fn max_output(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes;
        self
    }
}

fn quote(value: impl AsRef<str>) -> String {
    format!(
        "\"{}\"",
        value.as_ref().replace('\\', "\\\\").replace('"', "\\\"")
    )
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixture")
        .join("mcp_stdio.py")
}

fn append_servers(config: &Path, servers: &[&Server]) {
    let mut text = fs::read_to_string(config).expect("worker config");
    for server in servers {
        text.push_str(&format!("\n[mcp.servers.{}]\n", server.id));
        text.push_str(&format!("command = {}\n", quote("/usr/bin/python3")));
        text.push_str(&format!(
            "args = [{}]\n",
            quote(script().display().to_string())
        ));
        text.push_str(&format!(
            "env = {{ MCP_MODE = \"deterministic\", MCP_PID_FILE = {}, MCP_EFFECT_FILE = {} }}\n",
            quote(server.pid.display().to_string()),
            quote(server.effect.display().to_string()),
        ));
        text.push_str(&format!(
            "allowed_tools = [{}]\n",
            server
                .allowed_tools
                .iter()
                .map(quote)
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if !server.allowed_resources.is_empty() {
            text.push_str(&format!(
                "allowed_resources = [{}]\n",
                server
                    .allowed_resources
                    .iter()
                    .map(quote)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !server.protected_resources.is_empty() {
            text.push_str(&format!(
                "protected_resources = [{}]\n",
                server
                    .protected_resources
                    .iter()
                    .map(quote)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !server.allowed_prompts.is_empty() {
            text.push_str(&format!(
                "allowed_prompts = [{}]\n",
                server
                    .allowed_prompts
                    .iter()
                    .map(quote)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        text.push_str(&format!("max_output_bytes = {}\n", server.max_output_bytes));
    }
    fs::write(config, text).expect("mcp config");
}

fn options(instance: &str) -> fixture::Options {
    fixture::Options {
        instance: instance.into(),
        token: Some("dev-token".into()),
        model: "deterministic".into(),
        principals: Vec::new(),
        settings: Vec::new(),
        token_scopes: vec!["*".into()],
    }
}

fn revision(mcp: &[&str], grants: &[&str]) -> ProfileRevision {
    ProfileRevision {
        revision: 0,
        instruction: "Use the configured local MCP capability.".into(),
        model: "deterministic".into(),
        reasoning: None,
        capabilities: vec!["general".into()],
        grants: grants.iter().map(|grant| (*grant).into()).collect(),
        restrictions: Vec::new(),
        composition: "single".into(),
        members: vec!["general".into()],
        skills: Vec::new(),
        mcp: mcp.iter().map(|server| (*server).into()).collect(),
        workflow: None,
        limits: Limits::default(),
        compaction: None,
        secret_refs: Vec::new(),
        created_at: 0,
    }
}

fn request(profile: &str, input: String, key: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: "general".into(),
        session_id: format!("session-{key}"),
        input,
        idempotency_key: key.into(),
        profile: Some(profile.into()),
        team: None,
        worker: None,
        limits: Limits::default(),
    }
}

fn call(server: &str, tool: &str, arguments: &str) -> String {
    format!("call:mcp__{server}__{tool} {arguments}")
}

async fn start(config: &Path) -> runtime::worker::WorkerInstance {
    general::worker()
        .expect("package definition")
        .start(config)
        .await
        .expect("worker starts")
}

async fn wait(runtime: &runtime::Runtime, task_id: &str) -> Task {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let task = runtime.task_unscoped(task_id).await.expect("task exists");
        if task.state.is_terminal() {
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn pid(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        assert!(Instant::now() < deadline, "pid file was not written");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn resources_and_prompts_are_namespaced_allowlisted_and_bounded() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-surface"));
    let server = Server::new(data.path(), "local")
        .resources(&[
            "file:///notes/alpha.txt",
            "file:///notes/huge.txt",
            "file:///notes/binary.bin",
            "file:///notes/{name}.txt",
        ])
        .prompts(&["summarize"])
        .max_output(64);
    append_servers(&fixture.config, &[&server]);
    let instance = start(&fixture.config).await;
    let mcp = instance.mcp();

    let resources = mcp.resources("local").await.expect("resources list");
    let uris: Vec<&str> = resources
        .iter()
        .map(|resource| resource.uri.as_str())
        .collect();
    assert_eq!(
        uris,
        vec![
            "file:///notes/alpha.txt",
            "file:///notes/binary.bin",
            "file:///notes/huge.txt",
            "file:///notes/{name}.txt",
        ],
        "{resources:?}"
    );
    assert_eq!(
        resources[0].id, "mcp:local:resource:file:///notes/alpha.txt",
        "stable namespaced id"
    );
    assert!(!uris.iter().any(|uri| uri.contains("hidden")));

    let alpha = mcp
        .read_resource("local", "file:///notes/alpha.txt")
        .await
        .expect("alpha read");
    assert_eq!(alpha.text.as_deref(), Some("alpha resource body"));
    assert!(!alpha.truncated);

    let huge = mcp
        .read_resource("local", "file:///notes/huge.txt")
        .await
        .expect("huge read");
    assert!(huge.truncated, "huge resource must be bounded");
    assert!(huge.text.as_deref().unwrap_or_default().len() <= 64);

    let binary = mcp
        .read_resource("local", "file:///notes/binary.bin")
        .await
        .expect("binary read");
    assert_eq!(binary.blob_bytes, Some(4));
    assert!(binary.text.is_none(), "binary content must not be returned");

    let denied = mcp
        .read_resource("local", "file:///notes/hidden.txt")
        .await
        .expect_err("hidden resource is denied");
    assert_eq!(denied.code, "mcp.resource_denied");

    let prompts = mcp.prompts("local").await.expect("prompts list");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].id, "mcp:local:prompt:summarize");
    assert_eq!(prompts[0].arguments[0].name, "topic");
    assert!(prompts[0].arguments[0].required);

    let arguments = serde_json::Map::from_iter([(
        "topic".to_string(),
        serde_json::Value::String("MCP".into()),
    )]);
    let prompt = mcp
        .prompt("local", "summarize", arguments)
        .await
        .expect("prompt retrieval");
    assert_eq!(prompt.messages.len(), 2);
    assert_eq!(prompt.messages[0].role, "user");
    assert_eq!(prompt.messages[0].text, "Summarize MCP");
    assert_eq!(prompt.messages[1].role, "assistant");

    let denied = mcp
        .prompt("local", "hidden", serde_json::Map::new())
        .await
        .expect_err("hidden prompt is denied");
    assert_eq!(denied.code, "mcp.prompt_denied");

    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn protected_resources_require_an_explicit_instance_grant() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-protected"));
    let server = Server::new(data.path(), "local")
        .resources(&["file:///notes/alpha.txt"])
        .protected_resources(&["file:///notes/alpha.txt"]);
    append_servers(&fixture.config, &[&server]);
    let text = fs::read_to_string(&fixture.config).expect("config");
    let text = format!("grants = [\"*\", \"mcp:local:resource:file:///notes/alpha.txt\"]\n{text}");
    fs::write(&fixture.config, text).expect("explicit grant");
    let instance = start(&fixture.config).await;
    let alpha = instance
        .mcp()
        .read_resource("local", "file:///notes/alpha.txt")
        .await
        .expect("granted resource read");
    assert_eq!(alpha.text.as_deref(), Some("alpha resource body"));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unavailable_connection_fails_closed_and_restart_restores_it() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-health"));
    let server = Server::new(data.path(), "local").resources(&["file:///notes/alpha.txt"]);
    append_servers(&fixture.config, &[&server]);
    let instance = start(&fixture.config).await;
    let mcp = instance.mcp();
    let first = pid(&server.pid).await;

    let health = mcp.health().await;
    assert_eq!(health[0].state, runtime::mcp::McpState::Connected);

    let status = std::process::Command::new("kill")
        .args(["-9", &first.to_string()])
        .status()
        .expect("kill child");
    assert!(status.success());

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let health = mcp.health().await;
        if health[0].state != runtime::mcp::McpState::Connected {
            break;
        }
        assert!(Instant::now() < deadline, "child exit was not observed");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let failure = mcp
        .read_resource("local", "file:///notes/alpha.txt")
        .await
        .expect_err("dependent call fails closed");
    assert!(
        failure.code.contains("connection") || failure.code.contains("transport"),
        "{failure:?}"
    );

    mcp.restart("local").await.expect("restart succeeds");
    let second = pid(&server.pid).await;
    assert_ne!(first, second, "restart must start a new child");
    let health = mcp.health().await;
    assert_eq!(health[0].state, runtime::mcp::McpState::Connected);
    assert_eq!(health[0].restart_count, 1);
    let alpha = mcp
        .read_resource("local", "file:///notes/alpha.txt")
        .await
        .expect("resource read after restart");
    assert_eq!(alpha.text.as_deref(), Some("alpha resource body"));

    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_tool_calls_emit_audit_and_external_effect_evidence() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-evidence"));
    let server = Server::new(data.path(), "local");
    append_servers(&fixture.config, &[&server]);
    let toolsets = fixture.agents.join("general.toml");
    let mut text = fs::read_to_string(&toolsets).expect("agent");
    text.push_str("\ntoolsets = [\"mcp:local\"]\n");
    fs::write(&toolsets, text).expect("toolset");

    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin: Principal = instance.operator_principal().await.expect("principal");
    runtime
        .create_profile(
            &admin,
            "local",
            "mcp",
            revision(&["local"], &["task:submit", "mcp:local"]),
        )
        .await
        .expect("profile");
    runtime
        .activate_profile(&admin, "local", "mcp", 1)
        .await
        .expect("activate");

    let task = runtime
        .submit_task(
            &admin,
            request(
                "mcp",
                call("local", "record_effect", "{\"value\":\"evidence\"}"),
                "mcp-evidence",
            ),
        )
        .await
        .expect("admitted");
    let task = wait(&runtime, &task.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert_eq!(
        fs::read_to_string(&server.effect).unwrap_or_default(),
        "evidence\n"
    );

    let page = runtime
        .events_since(&task.id, 0, 2_000)
        .await
        .expect("events");
    let effect = page
        .events
        .iter()
        .find(|event| event.kind == "tool.effect")
        .expect("external effect evidence");
    assert_eq!(effect.payload["kind"], "external");
    assert_eq!(effect.payload["outcome"], "committed");
    let completed = page
        .events
        .iter()
        .find(|event| event.kind == "tool.completed")
        .expect("tool completed");
    assert_eq!(completed.payload["effect_evidence"]["kind"], "external");

    let records = runtime.audit_records(&task.id).await;
    assert!(
        records.iter().any(|record| record.action == "tool.call"
            && record.detail.contains("capability=mcp:local:call")),
        "{records:?}"
    );

    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_declared_mcp_servers_require_operator_opt_in() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-plugin"));
    let plugin = data.path().join("plugins").join("plugin-mcp");
    fs::create_dir_all(&plugin).expect("plugin directory");
    fs::write(
        plugin.join("plugin.toml"),
        format!(
            "manifest_version = 1\nname = \"plugin-mcp\"\nversion = \"0.1.0\"\ndescription = \"plugin mcp\"\ntools = [\"exec_command\"]\n\n[mcp.servers.plugin-local]\ncommand = {}\nargs = [{}]\nallowed_tools = [\"record_effect\"]\n",
            quote("/usr/bin/python3"),
            quote(script().display().to_string()),
        ),
    )
    .expect("plugin manifest");
    let mut text = fs::read_to_string(&fixture.config).expect("config");
    text.push_str("\n[plugins]\ndirectory = \"plugins\"\n\n[plugins.allow]\nplugin-mcp = \"*\"\n");
    fs::write(&fixture.config, text).expect("plugin config");

    let error = match general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
    {
        Ok(_) => panic!("plugin MCP server must be rejected by default"),
        Err(error) => error,
    };
    assert_eq!(error.code, "plugin.mcp_not_allowed");

    let mut text = fs::read_to_string(&fixture.config).expect("config");
    text.push_str("\n[mcp]\nallow_plugin_servers = true\n");
    fs::write(&fixture.config, text).expect("opt in");
    let instance = start(&fixture.config).await;
    assert!(instance.mcp().known("plugin-local").await);
    instance.shutdown().await;
}

struct Serve {
    child: std::process::Child,
    base: String,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_worker(config: &Path, log: &Path) -> Serve {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let stderr = fs::File::create(log).expect("stderr log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("worker serves");
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("listener address is announced");
    assert!(
        line.contains("listening on "),
        "worker did not start: {line}"
    );
    let base = line
        .split("listening on ")
        .nth(1)
        .expect("address announcement")
        .trim()
        .to_string();
    Serve { child, base }
}

impl Serve {
    fn get(&self, path: &str) -> (reqwest::StatusCode, serde_json::Value) {
        let response = reqwest::blocking::Client::new()
            .get(format!("{}{path}", self.base))
            .bearer_auth("dev-token")
            .send()
            .expect("request");
        let status = response.status();
        let body = response.text().expect("body");
        (
            status,
            serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
        )
    }

    fn post(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> (reqwest::StatusCode, serde_json::Value) {
        let response = reqwest::blocking::Client::new()
            .post(format!("{}{path}", self.base))
            .bearer_auth("dev-token")
            .json(&body)
            .send()
            .expect("request");
        let status = response.status();
        let body = response.text().expect("body");
        (
            status,
            serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
        )
    }
}

#[test]
fn management_routes_report_health_and_deny_unlisted_capabilities() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-routes"));
    let server = Server::new(data.path(), "local")
        .resources(&["file:///notes/alpha.txt"])
        .prompts(&["summarize"]);
    append_servers(&fixture.config, &[&server]);
    let log = data.path().join("stderr.log");
    let worker = spawn_worker(&fixture.config, &log);

    let (status, servers) = worker.get("/v1/mcp/servers");
    assert_eq!(status, reqwest::StatusCode::OK, "{servers}");
    assert_eq!(servers["servers"][0]["id"], "local");
    assert_eq!(servers["servers"][0]["transport"], "stdio");
    assert_eq!(servers["servers"][0]["state"], "connected");
    assert_eq!(servers["servers"][0]["allowed_resources"], 1);
    assert!(
        !servers.to_string().contains("MCP_PID_FILE"),
        "child environment must not be exposed: {servers}"
    );

    let (status, resources) = worker.get("/v1/mcp/servers/local/resources");
    assert_eq!(status, reqwest::StatusCode::OK, "{resources}");
    assert_eq!(
        resources[0]["id"],
        "mcp:local:resource:file:///notes/alpha.txt"
    );

    let (status, denied) = worker.post(
        "/v1/mcp/servers/local/resources/read",
        serde_json::json!({"uri": "file:///notes/hidden.txt"}),
    );
    assert!(!status.is_success(), "{denied}");
    assert_eq!(denied["code"], "mcp.resource_denied");

    let (status, prompt) = worker.post(
        "/v1/mcp/servers/local/prompts/summarize",
        serde_json::json!({"arguments": {"topic": "MCP"}}),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{prompt}");
    assert_eq!(prompt["messages"][0]["text"], "Summarize MCP");

    let (status, denied) = worker.post(
        "/v1/mcp/servers/local/prompts/hidden",
        serde_json::json!({"arguments": {}}),
    );
    assert!(!status.is_success(), "{denied}");
    assert_eq!(denied["code"], "mcp.prompt_denied");
}
