//! Ticket 25: authorized local MCP execution through the native Runner.

use common::{Limits, Principal, ProfileRevision, SubmitTaskRequest, Task, TaskState};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod fixture;

struct Server {
    id: String,
    effect: PathBuf,
    pid: PathBuf,
    env_dump: PathBuf,
    mode: String,
    allowed: Vec<String>,
    protected: Vec<String>,
    credential: Option<String>,
    call_timeout_ms: u64,
    max_output_bytes: usize,
    enabled: bool,
}

impl Server {
    fn new(directory: &Path, id: &str) -> Self {
        Self {
            id: id.into(),
            effect: directory.join(format!("{id}-effect.txt")),
            pid: directory.join(format!("{id}-pid.txt")),
            env_dump: directory.join(format!("{id}-env.json")),
            mode: "deterministic".into(),
            allowed: vec![
                "record_effect".into(),
                "guarded_effect".into(),
                "hidden_effect".into(),
                "crash_server".into(),
                "slow_call".into(),
                "huge_result".into(),
            ],
            protected: Vec::new(),
            credential: None,
            call_timeout_ms: 5_000,
            max_output_bytes: 256 * 1024,
            enabled: true,
        }
    }

    fn mode(mut self, mode: &str) -> Self {
        self.mode = mode.into();
        self
    }

    fn allowed(mut self, tools: &[&str]) -> Self {
        self.allowed = tools.iter().map(|tool| (*tool).into()).collect();
        self
    }

    fn protected(mut self, tools: &[&str]) -> Self {
        self.protected = tools.iter().map(|tool| (*tool).into()).collect();
        self
    }

    fn credential(mut self, reference: &str) -> Self {
        self.credential = Some(reference.into());
        self
    }

    fn call_timeout_mut(mut self, milliseconds: u64) -> Self {
        self.call_timeout_ms = milliseconds;
        self
    }

    fn max_output_mut(mut self, bytes: usize) -> Self {
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
            "env = {{ MCP_MODE = {}, MCP_PID_FILE = {}, MCP_EFFECT_FILE = {}, MCP_ENV_FILE = {} }}\n",
            quote(&server.mode),
            quote(server.pid.display().to_string()),
            quote(server.effect.display().to_string()),
            quote(server.env_dump.display().to_string()),
        ));
        text.push_str(&format!(
            "allowed_tools = [{}]\n",
            server
                .allowed
                .iter()
                .map(quote)
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if !server.protected.is_empty() {
            text.push_str(&format!(
                "protected_tools = [{}]\n",
                server
                    .protected
                    .iter()
                    .map(quote)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(credential) = &server.credential {
            text.push_str(&format!("credential = {}\n", quote(credential)));
        }
        text.push_str(&format!(
            "call_timeout_ms = {}\nmax_output_bytes = {}\nenabled = {}\n",
            server.call_timeout_ms, server.max_output_bytes, server.enabled
        ));
    }
    fs::write(config, text).expect("mcp config");
}

fn replace_servers(config: &Path, servers: &[&Server]) {
    let text = fs::read_to_string(config).expect("worker config");
    let marker = "\n[mcp.servers.";
    let text = match text.find(marker) {
        Some(index) => &text[..index],
        None => &text,
    };
    fs::write(config, format!("{}\n", text.trim_end())).expect("base config restored");
    append_servers(config, servers);
}

fn declare_toolsets(fixture: &fixture::Fixture, servers: &[&str]) {
    let path = fixture.agents.join("general.toml");
    let mut text = fs::read_to_string(&path).expect("agent definition");
    text.push_str(&format!(
        "\ntoolsets = [{}]\n",
        servers
            .iter()
            .map(|server| quote(format!("mcp:{server}")))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    fs::write(path, text).expect("agent toolsets");
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

fn revision(mcp: &[&str], grants: &[&str], secrets: &[&str]) -> ProfileRevision {
    ProfileRevision {
        revision: 0,
        instruction: "Use the configured local MCP capability.".into(),
        model: "deterministic".into(),
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
        secret_refs: secrets.iter().map(|secret| (*secret).into()).collect(),
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

async fn wait_gone(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Path::new("/proc").join(pid.to_string()).exists() {
        assert!(
            Instant::now() < deadline,
            "child process {pid} still exists"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn effect(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

async fn activate(
    instance: &runtime::worker::WorkerInstance,
    principal: &Principal,
    name: &str,
    revision: ProfileRevision,
) {
    instance
        .runtime()
        .create_profile(principal, "local", name, revision)
        .await
        .expect("profile created");
    instance
        .runtime()
        .activate_profile(principal, "local", name, 1)
        .await
        .expect("profile activated");
}

#[tokio::test(flavor = "multi_thread")]
async fn config_rejects_auto_approval_and_raw_credentials() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-config"));
    let server = Server::new(data.path(), "local").allowed(&["record_effect"]);
    append_servers(&fixture.config, &[&server]);
    let mut text = fs::read_to_string(&fixture.config).expect("config");
    text.push_str("auto_approve = true\n");
    fs::write(&fixture.config, text).expect("auto approve config");
    let error = match general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
    {
        Ok(_) => panic!("auto_approve must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, "config.parse_failed");

    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-secret-config"));
    let mut text = fs::read_to_string(&fixture.config).expect("config");
    text.push_str(
        "\n[mcp.servers.bad]\ncommand = \"/usr/bin/python3\"\nargs = []\nallowed_tools = [\"record_effect\"]\nenv = { LEAKED = \"sk-raw-value\" }\n",
    );
    fs::write(&fixture.config, text).expect("secret config");
    let error = match general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
    {
        Ok(_) => panic!("raw secret must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, "config.secret_value_rejected");
}

#[tokio::test(flavor = "multi_thread")]
async fn native_runner_invokes_local_mcp_tool_with_evidence() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-native"));
    let server = Server::new(data.path(), "local").allowed(&["record_effect"]);
    append_servers(&fixture.config, &[&server]);
    declare_toolsets(&fixture, &["local"]);
    let instance = start(&fixture.config).await;
    let child = pid(&server.pid).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("principal");
    activate(
        &instance,
        &admin,
        "mcp",
        revision(&["local"], &["task:submit", "mcp:local"], &[]),
    )
    .await;

    let task = runtime
        .submit_task(
            &admin,
            request(
                "mcp",
                call("local", "record_effect", "{\"value\":\"native-effect\"}"),
                "native",
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &task.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert_eq!(effect(&server.effect), "native-effect\n");
    assert_eq!(task.usage.tool_calls, 1);

    let page = runtime
        .events_since(&task.id, 0, 2_000)
        .await
        .expect("events");
    let call_event = page
        .events
        .iter()
        .find(|event| event.kind == "tool.call")
        .expect("native tool call evidence");
    assert_eq!(call_event.payload["name"], "mcp__local__record_effect");
    assert_eq!(call_event.payload["server"], "local");
    assert!(call_event.payload["id"].as_str().is_some());
    let result_event = page
        .events
        .iter()
        .find(|event| event.kind == "tool.result")
        .expect("native tool result evidence");
    assert_eq!(result_event.payload["ok"], true);

    instance.shutdown().await;
    wait_gone(child).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_revision_enables_and_disables_future_mcp_exposure() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-profile"));
    let server = Server::new(data.path(), "local").allowed(&["record_effect"]);
    append_servers(&fixture.config, &[&server]);
    declare_toolsets(&fixture, &["local"]);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("principal");

    runtime
        .create_profile(
            &admin,
            "local",
            "mcp",
            revision(&["local"], &["task:submit", "mcp:local"], &[]),
        )
        .await
        .expect("profile created");
    runtime
        .activate_profile(&admin, "local", "mcp", 1)
        .await
        .expect("revision one active");

    let first = runtime
        .submit_task(
            &admin,
            request(
                "mcp",
                call("local", "record_effect", "{\"value\":\"first\"}"),
                "first",
            ),
        )
        .await
        .expect("first admitted");
    assert_eq!(
        wait(&runtime, &first.task_id).await.state,
        TaskState::Succeeded
    );
    assert_eq!(effect(&server.effect), "first\n");

    runtime
        .revise_profile(
            &admin,
            "local",
            "mcp",
            1,
            revision(&[], &["task:submit"], &[]),
        )
        .await
        .expect("second revision appended");
    runtime
        .activate_profile(&admin, "local", "mcp", 2)
        .await
        .expect("revision two active");

    let profile = runtime
        .profile(&admin, "local", "mcp")
        .await
        .expect("profile readable");
    assert_eq!(profile.revisions[0].mcp, vec!["local".to_string()]);
    assert!(profile.revisions[1].mcp.is_empty());

    let second = runtime
        .submit_task(
            &admin,
            request(
                "mcp",
                call("local", "record_effect", "{\"value\":\"second\"}"),
                "second",
            ),
        )
        .await
        .expect("second admitted");
    let second = wait(&runtime, &second.task_id).await;
    assert_eq!(
        effect(&server.effect),
        "first\n",
        "old revision must not run"
    );
    assert!(
        second
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("not found"),
        "unexposed tool must not dispatch: {second:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_and_profile_grants_default_to_deny() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-deny"));
    let server = Server::new(data.path(), "local")
        .allowed(&["record_effect", "guarded_effect"])
        .protected(&["guarded_effect"]);
    append_servers(&fixture.config, &[&server]);
    declare_toolsets(&fixture, &["local"]);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("principal");

    let missing_grant = runtime
        .create_profile(
            &admin,
            "local",
            "ungranted",
            revision(&["local"], &["task:submit"], &[]),
        )
        .await
        .expect_err("connection grant is required");
    assert_eq!(missing_grant.code, "profile.mcp_grant_required");

    activate(
        &instance,
        &admin,
        "mcp",
        revision(&["local"], &["task:submit", "mcp:local"], &[]),
    )
    .await;

    let guarded = runtime
        .submit_task(
            &admin,
            request(
                "mcp",
                call("local", "guarded_effect", "{\"value\":\"guarded\"}"),
                "guarded",
            ),
        )
        .await
        .expect("guarded task admitted");
    let guarded = wait(&runtime, &guarded.task_id).await;
    assert!(guarded
        .output
        .as_deref()
        .unwrap_or_default()
        .contains("not found"));
    assert!(!effect(&server.effect).contains("guarded"));

    let hidden = runtime
        .submit_task(
            &admin,
            request(
                "mcp",
                call("local", "hidden_effect", "{\"value\":\"hidden\"}"),
                "hidden",
            ),
        )
        .await
        .expect("hidden task admitted");
    let _hidden = wait(&runtime, &hidden.task_id).await;
    assert!(!effect(&server.effect).contains("hidden"));

    runtime
        .revise_profile(
            &admin,
            "local",
            "mcp",
            1,
            revision(
                &["local"],
                &["task:submit", "mcp:local", "mcp:local:guarded_effect"],
                &[],
            ),
        )
        .await
        .expect("tool grant appended");
    runtime
        .activate_profile(&admin, "local", "mcp", 2)
        .await
        .expect("tool grant active");
    let allowed = runtime
        .submit_task(
            &admin,
            request(
                "mcp",
                call("local", "guarded_effect", "{\"value\":\"allowed\"}"),
                "allowed",
            ),
        )
        .await
        .expect("allowed task admitted");
    assert_eq!(
        wait(&runtime, &allowed.task_id).await.state,
        TaskState::Succeeded
    );
    assert!(effect(&server.effect).contains("guarded:allowed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn child_env_is_constrained_and_credential_scope_is_enforced() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-env"));
    let server = Server::new(data.path(), "local").allowed(&["record_effect"]);
    append_servers(&fixture.config, &[&server]);
    declare_toolsets(&fixture, &["local"]);
    std::env::set_var("GOLDENBEE_TEST_SECRET", "sk-parent-canary-value");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("principal");
    activate(
        &instance,
        &admin,
        "mcp",
        revision(&["local"], &["task:submit", "mcp:local"], &[]),
    )
    .await;

    let task = runtime
        .submit_task(
            &admin,
            request(
                "mcp",
                call("local", "record_effect", "{\"value\":\"env\"}"),
                "env",
            ),
        )
        .await
        .expect("task admitted");
    assert_eq!(
        wait(&runtime, &task.task_id).await.state,
        TaskState::Succeeded
    );
    let environment: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&server.env_dump).expect("environment dump"))
            .expect("environment json");
    assert_eq!(environment["MCP_MODE"], "deterministic");
    assert!(environment.get("GOLDENBEE_TEST_SECRET").is_none());
    assert!(environment.get("PATH").is_none());
    assert!(environment.get("HOME").is_none());

    let fixture_credential =
        fixture::write(&data.path().join("credential"), options("mcp-credential"));
    let credential_server = Server::new(&data.path().join("credential"), "local")
        .allowed(&["record_effect"])
        .credential("env:GOLDENBEE_TEST_SECRET");
    append_servers(&fixture_credential.config, &[&credential_server]);
    declare_toolsets(&fixture_credential, &["local"]);
    let credential_instance = start(&fixture_credential.config).await;
    let credential_runtime = credential_instance.runtime();
    let credential_admin = credential_instance
        .operator_principal()
        .await
        .expect("principal");
    let denied = credential_runtime
        .create_profile(
            &credential_admin,
            "local",
            "missing",
            revision(&["local"], &["task:submit", "mcp:local"], &[]),
        )
        .await
        .expect_err("credential scope is required");
    assert_eq!(denied.code, "profile.credential_scope_denied");
    credential_runtime
        .create_profile(
            &credential_admin,
            "local",
            "scoped",
            revision(
                &["local"],
                &["task:submit", "mcp:local"],
                &["env:GOLDENBEE_TEST_SECRET"],
            ),
        )
        .await
        .expect("scoped profile accepted");
    credential_instance.shutdown().await;
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn server_failure_is_isolated_to_its_connection_and_cleanup_runs() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-failure"));
    let failing = Server::new(data.path(), "failing");
    let healthy = Server::new(data.path(), "healthy");
    let broken = Server::new(data.path(), "broken")
        .mode("exit_on_start")
        .allowed(&["record_effect"]);
    std::env::set_var("GOLDENBEE_MCP_CANARY_25", "sk-mcp-canary-secret");
    let malformed = Server::new(data.path(), "malformed")
        .mode("malformed")
        .allowed(&["record_effect"])
        .credential("env:GOLDENBEE_MCP_CANARY_25");
    append_servers(&fixture.config, &[&failing, &healthy, &broken, &malformed]);
    declare_toolsets(&fixture, &["failing", "healthy", "broken", "malformed"]);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("principal");
    let failing_pid = pid(&failing.pid).await;
    let healthy_pid = pid(&healthy.pid).await;

    activate(
        &instance,
        &admin,
        "failing",
        revision(&["failing"], &["task:submit", "mcp:failing"], &[]),
    )
    .await;
    activate(
        &instance,
        &admin,
        "healthy",
        revision(&["healthy"], &["task:submit", "mcp:healthy"], &[]),
    )
    .await;
    activate(
        &instance,
        &admin,
        "broken",
        revision(&["broken"], &["task:submit", "mcp:broken"], &[]),
    )
    .await;
    activate(
        &instance,
        &admin,
        "malformed",
        revision(
            &["malformed"],
            &["task:submit", "mcp:malformed"],
            &["env:GOLDENBEE_MCP_CANARY_25"],
        ),
    )
    .await;

    let crashed = runtime
        .submit_task(
            &admin,
            request("failing", call("failing", "crash_server", "{}"), "crash"),
        )
        .await
        .expect("crash task admitted");
    let crashed = wait(&runtime, &crashed.task_id).await;
    assert_ne!(crashed.state, TaskState::Succeeded, "{crashed:?}");
    wait_gone(failing_pid).await;

    let healthy_task = runtime
        .submit_task(
            &admin,
            request(
                "healthy",
                call("healthy", "record_effect", "{\"value\":\"still-works\"}"),
                "healthy",
            ),
        )
        .await
        .expect("healthy task admitted");
    assert_eq!(
        wait(&runtime, &healthy_task.task_id).await.state,
        TaskState::Succeeded
    );
    assert_eq!(effect(&healthy.effect), "still-works\n");

    let broken_task = runtime
        .submit_task(
            &admin,
            request(
                "broken",
                call("broken", "record_effect", "{\"value\":\"never\"}"),
                "broken",
            ),
        )
        .await
        .expect("broken task admitted");
    let broken_task = wait(&runtime, &broken_task.task_id).await;
    assert_ne!(broken_task.state, TaskState::Succeeded);
    assert!(effect(&broken.effect).is_empty());

    let malformed_task = runtime
        .submit_task(
            &admin,
            request(
                "malformed",
                call("malformed", "record_effect", "{\"value\":\"never\"}"),
                "malformed",
            ),
        )
        .await
        .expect("malformed task admitted");
    let malformed_task = wait(&runtime, &malformed_task.task_id).await;
    assert_ne!(malformed_task.state, TaskState::Succeeded);
    let diagnostic = format!(
        "{} {}",
        malformed_task
            .error
            .as_ref()
            .map(|error| error.message.as_str())
            .unwrap_or_default(),
        malformed_task.output.as_deref().unwrap_or_default()
    );
    assert!(
        !diagnostic.contains("sk-mcp-canary-secret"),
        "secret leaked into diagnostics: {diagnostic}"
    );

    instance.shutdown().await;
    wait_gone(healthy_pid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn call_timeout_and_output_cap_bound_one_connection() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-bounds"));
    let server = Server::new(data.path(), "local")
        .allowed(&["huge_result", "slow_call", "record_effect"])
        .call_timeout_mut(200)
        .max_output_mut(512);
    append_servers(&fixture.config, &[&server]);
    declare_toolsets(&fixture, &["local"]);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("principal");
    activate(
        &instance,
        &admin,
        "mcp",
        revision(&["local"], &["task:submit", "mcp:local"], &[]),
    )
    .await;

    let huge = runtime
        .submit_task(
            &admin,
            request("mcp", call("local", "huge_result", "{}"), "huge"),
        )
        .await
        .expect("huge task admitted");
    let huge = wait(&runtime, &huge.task_id).await;
    assert_eq!(huge.state, TaskState::Succeeded, "{huge:?}");
    let output = huge.output.unwrap_or_default();
    assert!(
        output.contains("truncated"),
        "output was not capped: {output}"
    );
    assert!(
        output.len() < 2_000,
        "output was not finite: {}",
        output.len()
    );

    let slow = runtime
        .submit_task(
            &admin,
            request("mcp", call("local", "slow_call", "{}"), "slow"),
        )
        .await
        .expect("slow task admitted");
    let slow = wait(&runtime, &slow.task_id).await;
    let output = slow.output.clone().unwrap_or_default();
    let message = slow
        .error
        .as_ref()
        .map(|error| error.message.as_str())
        .unwrap_or("");
    assert!(
        output.contains("mcp.timeout") || message.contains("mcp.timeout"),
        "timeout was not enforced: {slow:?}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_instances_do_not_share_processes_or_effects() {
    let first_dir = tempfile::tempdir().expect("first data");
    let second_dir = tempfile::tempdir().expect("second data");
    let first_fixture = fixture::write(first_dir.path(), options("mcp-first"));
    let second_fixture = fixture::write(second_dir.path(), options("mcp-second"));
    let first_server = Server::new(first_dir.path(), "local").allowed(&["record_effect"]);
    let second_server = Server::new(second_dir.path(), "local").allowed(&["record_effect"]);
    append_servers(&first_fixture.config, &[&first_server]);
    append_servers(&second_fixture.config, &[&second_server]);
    declare_toolsets(&first_fixture, &["local"]);
    declare_toolsets(&second_fixture, &["local"]);

    let first = start(&first_fixture.config).await;
    let second = start(&second_fixture.config).await;
    let first_pid = pid(&first_server.pid).await;
    let second_pid = pid(&second_server.pid).await;
    assert_ne!(first_pid, second_pid);

    for (instance, _server, value, key) in [
        (&first, &first_server, "first-only", "first"),
        (&second, &second_server, "second-only", "second"),
    ] {
        let runtime = instance.runtime();
        let admin = instance.operator_principal().await.expect("principal");
        activate(
            instance,
            &admin,
            "mcp",
            revision(&["local"], &["task:submit", "mcp:local"], &[]),
        )
        .await;
        let task = runtime
            .submit_task(
                &admin,
                request(
                    "mcp",
                    call(
                        "local",
                        "record_effect",
                        &format!("{{\"value\":\"{value}\"}}"),
                    ),
                    key,
                ),
            )
            .await
            .expect("task admitted");
        assert_eq!(
            wait(&runtime, &task.task_id).await.state,
            TaskState::Succeeded
        );
    }
    assert_eq!(effect(&first_server.effect), "first-only\n");
    assert_eq!(effect(&second_server.effect), "second-only\n");

    first.shutdown().await;
    wait_gone(first_pid).await;
    assert!(
        Path::new("/proc").join(second_pid.to_string()).exists(),
        "shutting down one instance killed another instance's child"
    );
    second.shutdown().await;
    wait_gone(second_pid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn disable_and_reload_removes_the_connection_without_mutating_revisions() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), options("mcp-reload"));
    let server = Server::new(data.path(), "local").allowed(&["record_effect"]);
    append_servers(&fixture.config, &[&server]);
    declare_toolsets(&fixture, &["local"]);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("principal");
    let child = pid(&server.pid).await;
    activate(
        &instance,
        &admin,
        "mcp",
        revision(&["local"], &["task:submit", "mcp:local"], &[]),
    )
    .await;

    let disabled = Server {
        enabled: false,
        ..Server::new(data.path(), "local").allowed(&["record_effect"])
    };
    replace_servers(&fixture.config, &[&disabled]);
    instance.reload().await.expect("reload accepted");
    wait_gone(child).await;

    let profile = runtime
        .profile(&admin, "local", "mcp")
        .await
        .expect("profile readable");
    assert_eq!(profile.revisions[0].mcp, vec!["local".to_string()]);
    let denied = runtime
        .submit_task(
            &admin,
            request(
                "mcp",
                call("local", "record_effect", "{\"value\":\"disabled\"}"),
                "disabled",
            ),
        )
        .await
        .expect_err("disabled connection must reject admission");
    assert_eq!(denied.code, "profile.mcp_disabled");
    instance.shutdown().await;
}
