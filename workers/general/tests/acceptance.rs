//! Ticket 14: runtime CLI parity acceptance.
//!
//! One fixture Worker composes the runtime mechanism surface from package
//! declarations and operator configuration. The same deterministic scenario
//! runs through the Worker HTTP/app-server contract and through headless
//! JSON/JSONL execution. The test asserts only external behavior: files on
//! disk, task projections, events, audit, checkpoints, usage, MCP effects, and
//! protected-effect operations. It never reaches into runtime internals.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

mod fixture;

const TOKEN: &str = "dev-token";
const PROFILE: &str = "acceptance";

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

impl Served {
    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Acceptance {
    fixture: fixture::Fixture,
    workspace: PathBuf,
    effect: PathBuf,
    hook_log: PathBuf,
    plugin_hook: PathBuf,
    mcp_effect: PathBuf,
    mcp_pid: PathBuf,
    web: String,
}

fn workspace_of(data: &Path) -> PathBuf {
    data.join("workspace")
}

fn append_config(config: &Path, text: &str) {
    let mut current = std::fs::read_to_string(config).expect("worker config");
    current.push_str(text);
    std::fs::write(config, current).expect("worker config update");
}

fn write_plugin(root: &Path, hook_log: &Path, trusted: bool) {
    let directory = root.join("acceptance-plugin");
    std::fs::create_dir_all(directory.join("commands")).expect("plugin commands");
    std::fs::create_dir_all(directory.join("skills/plugin-extra")).expect("plugin skill");
    let log = hook_log.display();
    let script = directory.join("hook.py");
    std::fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env python3
import json
import sys
data = json.load(sys.stdin)
event = data.get("event", "")
payload = data.get("payload", {{}})
with open(r"{log}", "a") as handle:
    handle.write(event + "|" + payload.get("tool", "") + "\n")
print(json.dumps({{"decision": "continue"}}))
"#
        ),
    )
    .expect("plugin hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("hook executable");
    }
    std::fs::write(
        directory.join("skills/plugin-extra/SKILL.md"),
        "---\nname: plugin-extra\ndescription: Plugin-provided acceptance skill.\n---\nPlugin instructions.",
    )
    .expect("plugin skill");
    std::fs::write(
        directory.join("commands/plugin_cmd.toml"),
        "description = \"Plugin acceptance command\"\nprompt = \"plugin-command: {{args}}\"\nagent = \"general\"\n",
    )
    .expect("plugin command");
    let mut manifest = String::from(
        "manifest_version = 1\nname = \"acceptance-plugin\"\nversion = \"0.1.0\"\ndescription = \"runtime acceptance fixture\"\ntools = [\"shell\"]\n[commands]\ndirectory = \"commands\"\n[skills]\ndirectory = \"skills\"\n",
    );
    for event in ["prompt_submit", "pre_tool", "stop", "subagent_stop"] {
        manifest.push_str(&format!(
            "[[hooks]]\nname = \"{event}\"\nevent = \"{event}\"\ncommand = \"hook.py\"\ntimeout_ms = 5000\n"
        ));
    }
    std::fs::write(directory.join("plugin.toml"), manifest).expect("plugin manifest");
    let mut trust = String::new();
    if trusted {
        trust.push_str("\n[plugins.allow]\nacceptance-plugin = \"*\"\n");
    }
    std::fs::write(root.join("trust.toml"), trust).expect("plugin trust");
}

fn write_assets(fixture: &fixture::Fixture) {
    std::fs::write(
        fixture.agents.join("general.toml"),
        "name = \"general\"\ndescription = \"Runtime parity acceptance agent\"\nmodel = \"deterministic\"\nprompt = \"general\"\ntools = [\"read_file\", \"write_file\", \"edit_file\", \"multi_edit\", \"apply_patch\", \"glob\", \"grep\", \"shell\", \"web_fetch\", \"write_todos\", \"spawn_subagent\", \"wait_subagent\", \"interrupt_subagent\", \"list_subagents\", \"close_subagent\", \"ledger_write\"]\ntoolsets = [\"mcp:fixture\"]\nskills = [\"tone\"]\n",
    )
    .expect("acceptance agent");
    std::fs::write(
        fixture.prompts.join("general.md"),
        "Use the declared runtime capabilities. Do not add domain behavior.",
    )
    .expect("acceptance prompt");
    std::fs::write(
        fixture.commands.join("acceptance_command.toml"),
        "description = \"Acceptance command\"\nprompt = \"acceptance-command: {{args}}\"\nagent = \"general\"\nallowed_tools = [\"read_file\"]\n",
    )
    .expect("acceptance command");
}

fn web_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("web fixture listener");
    let port = listener.local_addr().expect("web fixture address").port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for mut stream in listener.incoming().flatten() {
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer);
            let body = b"web-evidence-body";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    });
    format!("http://127.0.0.1:{port}/evidence")
}

fn acceptance(data: &Path, instance: &str, trusted_plugin: bool, mcp_mode: &str) -> Acceptance {
    let fixture = fixture::write(
        data,
        fixture::Options {
            instance: instance.into(),
            token: Some(TOKEN.into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    write_assets(&fixture);
    let workspace = workspace_of(data);
    std::fs::create_dir_all(&workspace).expect("workspace");
    let effect = data.join("effects.log");
    let hook_log = data.join("worker-hooks.log");
    let plugin_hook = data.join("plugin-hooks.log");
    let mcp_effect = data.join("mcp-effect.txt");
    let mcp_pid = data.join("mcp-pid.txt");
    let web = web_server();
    let plugin_root = data.join("plugins");
    write_plugin(&plugin_root, &plugin_hook, trusted_plugin);
    let trust = std::fs::read_to_string(plugin_root.join("trust.toml")).expect("plugin trust");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture/mcp_stdio.py");
    let mcp = format!(
        "\n[mcp.servers.fixture]\ncommand = \"/usr/bin/python3\"\nargs = [\"{}\"]\nenv = {{ MCP_MODE = \"{}\", MCP_EFFECT_FILE = \"{}\", MCP_PID_FILE = \"{}\" }}\nallowed_tools = [\"record_effect\"]\ncall_timeout_ms = 5000\nmax_output_bytes = 262144\nenabled = true\n",
        script.display(),
        mcp_mode,
        mcp_effect.display(),
        mcp_pid.display()
    );
    append_config(
        &fixture.config,
        &format!(
            "\n[tools]\nworkspace = \"workspace\"\ntimeout_ms = 120000\nmax_output_bytes = 1048576\nmax_parallel = 2\n\n[permissions]\nmode = \"accept-edits\"\napproval_wait_ms = 0\napproval_ttl_ms = 60000\n\n[[permissions.rules]]\nid = \"allow-shell\"\ndecision = \"allow\"\ntools = [\"shell\"]\npriority = 10\n\n[[permissions.rules]]\nid = \"allow-web\"\ndecision = \"allow\"\ntools = [\"web_fetch\"]\npriority = 10\n\n[[permissions.rules]]\nid = \"ask-grep\"\ndecision = \"ask\"\ntools = [\"grep\"]\npriority = 100\n\n[[permissions.rules]]\nid = \"deny-patch\"\ndecision = \"deny\"\ntools = [\"apply_patch\"]\npriority = 100\n\n[sandbox]\nprofile = \"workspace-write\"\n\n[sandbox.network]\nenabled = true\n\n[skills]\ndisclosure = \"progressive\"\n\n[hooks]\ntimeout_ms = 5000\n\n[plugins]\ndirectory = \"plugins\"\n{trust}{mcp}"
        ),
    );
    Acceptance {
        fixture,
        workspace,
        effect,
        hook_log,
        plugin_hook,
        mcp_effect,
        mcp_pid,
        web,
    }
}

fn envs(acceptance: &Acceptance) -> Vec<(&'static str, std::ffi::OsString)> {
    vec![
        (
            "ACCEPTANCE_SANDBOX_DIR",
            acceptance.workspace.as_os_str().to_owned(),
        ),
        (
            "ACCEPTANCE_EFFECT_FILE",
            acceptance.effect.as_os_str().to_owned(),
        ),
        (
            "ACCEPTANCE_HOOK_LOG",
            acceptance.hook_log.as_os_str().to_owned(),
        ),
    ]
}

fn serve(acceptance: &Acceptance) -> Served {
    let mut command = Command::new(env!("CARGO_BIN_EXE_acceptance-fixture"));
    command
        .args([
            "serve",
            "--config",
            acceptance.fixture.config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in envs(acceptance) {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("fixture Worker serves");
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

fn cli(acceptance: &Acceptance, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_acceptance-fixture"));
    command
        .args(args)
        .args([
            "--config",
            acceptance.fixture.config.to_str().expect("config path"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs(acceptance) {
        command.env(key, value);
    }
    command.output().expect("fixture CLI runs")
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .expect("http client")
}

fn get(base: &str, path: &str) -> Value {
    client()
        .get(format!("{base}{path}"))
        .bearer_auth(TOKEN)
        .send()
        .expect("response")
        .json()
        .expect("json")
}

fn post(base: &str, path: &str, body: Value) -> Value {
    client()
        .post(format!("{base}{path}"))
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .expect("response")
        .json()
        .expect("json")
}

fn create_profile(base: &str, name: &str, compaction: Option<Value>) {
    let revision = json!({
        "revision": 0,
        "instruction": "Use the runtime acceptance fixture.",
        "model": "deterministic",
        "capabilities": [],
        "grants": ["task:submit", "mcp:fixture"],
        "restrictions": [],
        "composition": "single",
        "members": [],
        "skills": [],
        "mcp": ["fixture"],
        "workflow": null,
        "limits": {},
        "compaction": compaction,
        "secret_refs": [],
        "reasoning": null,
        "created_at": 0
    });
    let created = post(
        base,
        "/v1/profiles",
        json!({"name": name, "revision": revision}),
    );
    assert!(created["name"] == name, "{created}");
    let activated = post(
        base,
        &format!("/v1/profiles/{name}/activate"),
        json!({"revision": 1}),
    );
    assert_eq!(activated["active_revision"], 1, "{activated}");
}

fn submit(base: &str, session: &str, input: &str, profile: Option<&str>) -> String {
    let mut body = json!({
        "agent": "general",
        "session_id": session,
        "input": input,
        "idempotency_key": format!("{session}-{}", common::canonical_digest(&input)),
    });
    if let Some(profile) = profile {
        body["profile"] = json!(profile);
    }
    let submitted = post(base, "/v1/tasks", body);
    submitted["task_id"]
        .as_str()
        .unwrap_or_else(|| panic!("task id missing: {submitted}"))
        .to_string()
}

fn wait_task(base: &str, task_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let task = get(base, &format!("/v1/tasks/{task_id}"));
        if task["state"]
            .as_str()
            .is_some_and(|state| matches!(state, "succeeded" | "failed" | "cancelled" | "unknown"))
        {
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn events(base: &str, task_id: &str) -> Vec<Value> {
    get(base, &format!("/v1/tasks/{task_id}/events"))
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn event_kind<'a>(events: &'a [Value], kind: &str) -> Option<&'a Value> {
    events.iter().find(|event| event["kind"] == kind)
}

fn tool_event<'a>(events: &'a [Value], kind: &str, tool: &str) -> Option<&'a Value> {
    events.iter().find(|event| {
        event["kind"] == kind
            && (event["payload"]["tool"] == tool || event["payload"]["name"] == tool)
    })
}

fn scenario_steps(web: &str) -> String {
    let steps = json!([
        {"tool": "write_file", "args": {"path": "notes/acceptance.txt", "content": "alpha\n"}},
        {"tool": "edit_file", "args": {"path": "notes/acceptance.txt", "old_string": "alpha", "new_string": "beta"}},
        {"tool": "shell", "args": {"command": "printf shell-ok"}},
        {"tool": "web_fetch", "args": {"url": web}},
        {"tool": "mcp__fixture__record_effect", "args": {"value": "mcp-ok"}},
        {"tool": "ledger_write", "args": {"entry": "acceptance"}},
        {"tool": "spawn_subagent", "args": {"definition": "helper", "input": "child:acceptance-child"}},
        {"tool": "apply_patch", "args": {"patch": "*** Begin Patch\n*** Add File: denied.txt\n+no\n*** End Patch"}},
        {"tool": "write_file", "args": {"path": "../escape.txt", "content": "escaped"}},
        {"tool": "grep", "args": {"pattern": "beta", "path": "notes"}},
        {"tool": "read_file", "args": {"path": "notes/acceptance.txt"}}
    ]);
    format!("steps:{steps}")
}

fn wait_log(path: &Path, needle: &str) -> String {
    for _ in 0..1_000 {
        let contents = std::fs::read_to_string(path).unwrap_or_default();
        if contents.lines().any(|line| line.contains(needle)) {
            return contents;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "missing '{needle}' in {}",
        std::fs::read_to_string(path).unwrap_or_default()
    );
}

fn assert_scenario_artifacts(acceptance: &Acceptance) {
    assert_eq!(
        std::fs::read_to_string(acceptance.workspace.join("notes/acceptance.txt"))
            .expect("edited file exists"),
        "beta\n",
        "generic write/edit tools must produce the edited workspace file"
    );
    assert!(
        !acceptance.workspace.join("escape.txt").exists()
            && !acceptance.workspace.join("../escape.txt").exists(),
        "sandbox escape attempt wrote outside the workspace"
    );
    let mcp = std::fs::read_to_string(&acceptance.mcp_effect).unwrap_or_default();
    assert!(
        mcp.contains("mcp-ok"),
        "MCP fixture effect missing: {mcp:?}"
    );
    let effect = std::fs::read_to_string(&acceptance.effect).unwrap_or_default();
    assert!(
        effect.contains("acceptance"),
        "protected effect missing: {effect:?}"
    );
    let pid = std::fs::read_to_string(&acceptance.mcp_pid).unwrap_or_default();
    assert!(
        pid.trim().parse::<u32>().is_ok(),
        "the MCP fixture process did not start: {pid:?}"
    );
}

fn assert_scenario_events(events: &[Value]) {
    for tool in [
        "write_file",
        "edit_file",
        "shell",
        "web_fetch",
        "mcp__fixture__record_effect",
        "ledger_write",
        "spawn_subagent",
        "read_file",
    ] {
        let completed = tool_event(events, "tool.completed", tool)
            .or_else(|| tool_event(events, "tool.result", tool))
            .unwrap_or_else(|| panic!("no completion event for {tool}: {events:?}"));
        assert_eq!(
            completed["payload"]["ok"], true,
            "{tool} did not complete successfully: {completed}"
        );
    }
    let denied_patch = events
        .iter()
        .find(|event| {
            event["kind"] == "permission.decision" && event["payload"]["tool"] == "apply_patch"
        })
        .expect("denied tool decision");
    assert_eq!(denied_patch["payload"]["decision"], "deny");
    assert_eq!(denied_patch["payload"]["rule"], "deny-patch");
    let sandbox = events
        .iter()
        .find(|event| event["kind"] == "sandbox.decision" && event["payload"]["decision"] == "deny")
        .expect("sandbox escape decision");
    assert_eq!(sandbox["payload"]["tool"], "write_file");
    assert_eq!(sandbox["payload"]["code"], "sandbox.path_denied");
    let pending = event_kind(events, "approval.pending").expect("ask approval evidence");
    assert_eq!(pending["payload"]["tool"], "grep");
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "subagent.spawned"),
        "subagent spawn missing: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "subagent.terminal"),
        "subagent terminal evidence missing: {events:?}"
    );
    assert!(
        events.iter().any(|event| event["kind"] == "subagent.stop"),
        "automatic subagent-stop evidence missing: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "checkpoint.committed"),
        "checkpoint evidence missing: {events:?}"
    );
    assert!(
        events.iter().any(|event| event["kind"] == "diff.recorded"),
        "diff evidence missing: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event["kind"].as_str(), Some("hook.failed" | "hook.timeout"))),
        "clean hooks produced failure evidence: {events:?}"
    );
}

fn assert_audit_and_evidence(base: &str, task: &Value, events: &[Value], acceptance: &Acceptance) {
    let task_id = task["id"].as_str().expect("task id");
    let trace = task["trace_id"].as_str().expect("trace id");
    assert!(!trace.is_empty());
    assert!(
        events
            .iter()
            .all(|event| event["trace_id"].as_str() == Some(trace)),
        "every event must carry the task trace"
    );
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0]["seq"].as_u64() < pair[1]["seq"].as_u64()),
        "events must be ordered by sequence: {events:?}"
    );

    let audit = get(base, &format!("/v1/tasks/{task_id}/audit"));
    let audit = audit.as_array().expect("audit entries");
    let submit_entry = audit
        .iter()
        .find(|entry| entry["action"] == "task.submit")
        .expect("task submission audit");
    assert_eq!(submit_entry["principal"], "local");
    assert_eq!(submit_entry["decision"], "accepted");
    assert_eq!(submit_entry["resource"], task_id);
    assert_eq!(submit_entry["trace_id"], trace);
    assert!(audit
        .iter()
        .any(|entry| entry["action"] == "permission.decision" && entry["decision"] == "deny"));
    assert!(audit
        .iter()
        .any(|entry| entry["action"] == "sandbox.decision" && entry["decision"] == "deny"));

    let usage = &task["usage"];
    assert!(
        usage["tool_calls"].as_u64().unwrap_or(0) >= 7,
        "usage must report executed tool calls: {usage}"
    );
    assert!(
        usage["prompt_tokens"].as_u64().is_some(),
        "provider-confirmed prompt tokens are missing: {usage}"
    );

    let operations = get(base, &format!("/v1/tasks/{task_id}/operations"));
    assert_eq!(operations["state"], "succeeded", "{operations}");
    assert_eq!(operations["terminal"], true, "{operations}");
    assert_eq!(operations["trace_id"], trace, "{operations}");
    let operation = operations["operations"]
        .as_array()
        .and_then(|operations| operations.first())
        .expect("protected effect operation");
    assert_eq!(operation["tool"], "ledger_write");
    assert_eq!(operation["status"], "committed");
    assert_eq!(operation["terminal"], true);

    // The same evidence is consumable through the shared SDK contract.
    let sdk = sdk::Client::new(base.to_string(), TOKEN);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("sdk runtime");
    let sdk_audit = runtime.block_on(sdk.audit(task_id)).expect("sdk audit");
    assert_eq!(sdk_audit.len(), audit.len());
    let sdk_operations = runtime
        .block_on(sdk.operations(task_id))
        .expect("sdk operations");
    assert_eq!(sdk_operations.operations.len(), 1);
    let sdk_events = runtime
        .block_on(sdk.events(task_id, 0, 1_000))
        .expect("sdk events");
    assert_eq!(sdk_events.events.len(), events.len());
    assert!(sdk_events
        .events
        .iter()
        .any(|event| event.kind == "checkpoint.committed"));

    let hook_log = std::fs::read_to_string(&acceptance.hook_log).unwrap_or_default();
    let entries = hook_log.lines().collect::<Vec<_>>();
    let prompt = entries
        .iter()
        .position(|entry| entry.starts_with("prompt_submit|"))
        .expect("prompt_submit hook");
    let pre = entries
        .iter()
        .position(|entry| entry == &"pre_tool|write_file")
        .expect("pre_tool hook");
    let post = entries
        .iter()
        .position(|entry| entry == &"post_tool|write_file")
        .expect("post_tool hook");
    assert!(prompt < pre && pre < post, "hook order: {entries:?}");
    let hook_log = wait_log(&acceptance.hook_log, "stop|");
    let entries = hook_log.lines().collect::<Vec<_>>();
    assert!(
        entries
            .iter()
            .any(|entry| entry.starts_with("stop|") && entry.contains("|succeeded")),
        "terminal stop hook missing: {entries:?}"
    );
    let plugin = std::fs::read_to_string(&acceptance.plugin_hook).unwrap_or_default();
    assert!(
        plugin.contains("prompt_submit|"),
        "plugin hook missing: {plugin:?}"
    );
    assert!(
        plugin.contains("pre_tool|shell"),
        "plugin tool hook missing: {plugin:?}"
    );
    let plugin = wait_log(&acceptance.plugin_hook, "subagent_stop|");
    assert!(
        plugin.contains("subagent_stop|"),
        "automatic subagent-stop hook missing: {plugin:?}"
    );
}

#[test]
fn runtime_parity_scenario_works_over_http_sdk_and_headless_protocols() {
    let data = tempfile::tempdir().expect("data");
    let acceptance = acceptance(data.path(), "acceptance-http", true, "deterministic");
    let served = serve(&acceptance);
    let base = served.base.clone();
    create_profile(&base, PROFILE, None);

    let scenario = scenario_steps(&acceptance.web);
    let task_id = submit(&base, "acceptance-http", &scenario, Some(PROFILE));
    let task = wait_task(&base, &task_id);
    assert_eq!(task["state"], "succeeded", "{task}");
    assert!(
        task["output"].as_str().unwrap_or_default().contains("beta"),
        "final read did not expose the edited content: {task}"
    );
    assert_scenario_artifacts(&acceptance);
    let task_events = events(&base, &task_id);
    assert_scenario_events(&task_events);
    assert_audit_and_evidence(&base, &task, &task_events, &acceptance);

    // Discovery: runtime commands, Worker commands, plugin assets, skills, and
    // trusted plugin inventory are all visible at the same seam.
    let commands = get(&base, "/v1/commands");
    let commands = commands.as_array().expect("commands");
    assert!(commands
        .iter()
        .any(|command| command["name"] == "acceptance_command"));
    let acceptance_command = commands
        .iter()
        .find(|command| command["name"] == "acceptance_command")
        .expect("acceptance command descriptor");
    assert_eq!(acceptance_command["allowed_tools"][0], "read_file");
    assert!(commands
        .iter()
        .any(|command| command["name"] == "plugin_cmd"));
    let skills = post(
        &base,
        "/v1/commands/skills.list",
        json!({"session_id": "acceptance-skills"}),
    );
    let skills = skills["result"]["skills"]
        .as_array()
        .expect("skill inventory");
    assert!(
        skills.iter().any(|skill| skill["name"] == "tone"),
        "{skills:?}"
    );
    assert!(
        skills.iter().any(|skill| skill["name"] == "plugin-extra"),
        "plugin skill was not activated: {skills:?}"
    );
    let capabilities = get(&base, "/v1/capabilities");
    for forbidden in [
        "init",
        "review",
        "commit",
        "security-review",
        "pr",
        "merge",
        "deploy",
    ] {
        assert!(
            !commands.iter().any(|command| command["name"] == forbidden),
            "runtime composition exposed a vendor coding command: {forbidden}"
        );
    }
    let plugins = capabilities["plugins"]
        .as_array()
        .expect("plugin inventory");
    assert_eq!(plugins.len(), 1, "{capabilities}");
    assert_eq!(plugins[0]["trust"], "allowlist");
    assert_eq!(plugins[0]["tools"][0], "shell");
    let servers = get(&base, "/v1/mcp/servers");
    assert_eq!(servers["servers"][0]["state"], "connected", "{servers}");

    // Worker command invocation narrows the target task and still uses the
    // one admission/execution path.
    let command = post(
        &base,
        "/v1/commands/acceptance_command",
        json!({
            "input": "command-ok",
            "session_id": "acceptance-command",
            "idempotency_key": "acceptance-command",
        }),
    );
    let command_task = wait_task(
        &base,
        command["task"]["task_id"].as_str().expect("command task"),
    );
    assert_eq!(command_task["state"], "succeeded", "{command_task}");
    assert!(
        command_task["output"]
            .as_str()
            .unwrap_or_default()
            .contains("command-ok"),
        "{command_task}"
    );

    // Checkpoint diff and rewind/restore over the same contract.
    let checkpoints = get(&base, &format!("/v1/tasks/{task_id}/checkpoints"));
    let checkpoint = checkpoints
        .as_array()
        .and_then(|checkpoints| checkpoints.first())
        .expect("checkpoint");
    let checkpoint_id = checkpoint["id"].as_str().expect("checkpoint id");
    let diff = get(&base, &format!("/v1/checkpoints/{checkpoint_id}/diff"));
    assert!(
        diff["files"]
            .as_array()
            .expect("diff files")
            .iter()
            .any(|file| file["path"] == "notes/acceptance.txt"),
        "{diff}"
    );
    let rewound = post(
        &base,
        &format!("/v1/checkpoints/{checkpoint_id}/restore"),
        json!({"mode": "rewind", "files": ["notes/acceptance.txt"]}),
    );
    assert_eq!(rewound["changed"][0], "notes/acceptance.txt", "{rewound}");
    assert!(!acceptance.workspace.join("notes/acceptance.txt").exists());
    post(
        &base,
        &format!("/v1/checkpoints/{checkpoint_id}/restore"),
        json!({"mode": "restore", "files": ["notes/acceptance.txt"]}),
    );
    assert_eq!(
        std::fs::read_to_string(acceptance.workspace.join("notes/acceptance.txt"))
            .expect("restored file"),
        "beta\n"
    );

    // Session continuation, fork, and transcript export use the same canonical
    // identity as the executed scenario.
    let session = get(&base, "/v1/sessions/acceptance-http");
    assert_eq!(session["session_id"], "acceptance-http", "{session}");
    assert!(
        session["event_count"].as_u64().unwrap_or(0) > 0,
        "{session}"
    );
    assert!(
        !session["turns"].as_array().expect("turns").is_empty(),
        "{session}"
    );
    let fork = post(
        &base,
        "/v1/sessions/acceptance-http/fork",
        json!({"session_id": "acceptance-http-fork", "title": "fork"}),
    );
    assert_eq!(fork["forked_from"], "acceptance-http", "{fork}");
    let transcript = post(&base, "/v1/sessions/acceptance-http/transcript", json!({}));
    assert!(
        transcript["turn_count"].as_u64().unwrap_or(0) > 0,
        "{transcript}"
    );
    assert!(!transcript["digest"].as_str().unwrap_or_default().is_empty());
    assert!(
        !transcript["turns"].as_array().expect("turns").is_empty(),
        "{transcript}"
    );

    // Stop the serving process before the headless runs open the same durable
    // store, so each phase has one writer.
    served.stop();

    // Headless JSON and JSONL execute the same scenario through the same
    // runtime and produce equivalent outcomes.
    let json = cli(
        &acceptance,
        &[
            "run",
            "--profile",
            PROFILE,
            "--session",
            "acceptance-json",
            "--input",
            &scenario,
            "--json",
        ],
    );
    assert!(
        json.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&json.stderr)
    );
    let json: Value = serde_json::from_slice(&json.stdout).expect("structured result");
    assert_eq!(json["state"], "succeeded", "{json}");
    assert_eq!(json["exit_code"], 0, "{json}");
    assert!(json["task_id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(json["session_id"] == "acceptance-json", "{json}");
    assert!(
        json["output"].as_str().unwrap_or_default().contains("beta"),
        "{json}"
    );
    assert_scenario_artifacts(&acceptance);

    let resumed = cli(
        &acceptance,
        &[
            "run",
            "--profile",
            PROFILE,
            "--resume",
            "acceptance-json",
            "--input",
            "resume-check",
            "--json",
        ],
    );
    assert!(
        resumed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let resumed: Value = serde_json::from_slice(&resumed.stdout).expect("resume result");
    assert_eq!(resumed["session_id"], "acceptance-json", "{resumed}");
    assert!(
        resumed["output"]
            .as_str()
            .unwrap_or_default()
            .contains("resume-check"),
        "{resumed}"
    );

    let jsonl = cli(
        &acceptance,
        &[
            "run",
            "--profile",
            PROFILE,
            "--session",
            "acceptance-jsonl",
            "--input",
            &scenario,
            "--jsonl",
        ],
    );
    assert!(
        jsonl.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&jsonl.stderr)
    );
    let frames: Vec<Value> = String::from_utf8(jsonl.stdout)
        .expect("jsonl utf-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("frame json"))
        .collect();
    assert!(!frames.is_empty());
    assert_eq!(frames.first().unwrap()["type"], "task_started");
    assert_eq!(frames.last().unwrap()["type"], "task_completed");
    assert_eq!(frames.last().unwrap()["state"], "succeeded");
    assert_eq!(frames.last().unwrap()["exit_code"], 0);
    let frame_task = frames[0]["task_id"].clone();
    assert!(frames.iter().all(|frame| frame["task_id"] == frame_task));
    let items = frames
        .iter()
        .filter_map(|frame| frame.get("item"))
        .filter(|item| !item.is_null())
        .collect::<Vec<_>>();
    let mut tool_names = items
        .iter()
        .filter(|item| item["item_type"] == "tool_call")
        .filter_map(|item| item["name"].as_str())
        .collect::<Vec<_>>();
    tool_names.sort();
    tool_names.dedup();
    for tool in [
        "write_file",
        "edit_file",
        "shell",
        "web_fetch",
        "ledger_write",
        "spawn_subagent",
        "apply_patch",
        "grep",
        "read_file",
    ] {
        assert!(
            tool_names.contains(&tool),
            "jsonl frame missing {tool}: {tool_names:?}"
        );
    }
    assert!(
        items.iter().any(|item| {
            (item["item_type"] == "approval" && item["decision"] == "pending")
                || (item["item_type"] == "tool_call" && item["error_code"] == "approval.pending")
        }),
        "jsonl frames must expose the ask outcome: {items:?}"
    );
    assert!(
        items.iter().any(|item| item["item_type"] == "diff"),
        "jsonl frames must expose typed checkpoint diffs: {items:?}"
    );

    let headless_task = frame_task.as_str().expect("jsonl task id");
    let listed = cli(
        &acceptance,
        &["checkpoint", "list", "--task", headless_task, "--json"],
    );
    assert!(
        listed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let checkpoints: Vec<Value> =
        serde_json::from_slice(&listed.stdout).expect("cli checkpoint list json");
    let checkpoint = checkpoints.first().expect("headless checkpoint");
    assert!(
        checkpoint["files"]
            .as_array()
            .expect("checkpoint files")
            .iter()
            .any(|file| file["path"] == "notes/acceptance.txt"),
        "{checkpoint}"
    );
    let restored = cli(
        &acceptance,
        &[
            "checkpoint",
            "restore",
            checkpoint["id"].as_str().expect("checkpoint id"),
            "--json",
        ],
    );
    assert!(
        restored.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&restored.stderr)
    );
}

#[test]
fn runtime_surface_stays_generic_and_credential_free() {
    // The acceptance fixture composes the shared runtime. Its dependency
    // surface must not pull TUI, Gateway, host lifecycle, or client crates in.
    let manifest = include_str!("../../../crates/runtime/Cargo.toml");
    for forbidden in [
        "ratatui",
        "crossterm",
        "gateway =",
        "sdk =",
        "instance =",
        "install =",
    ] {
        assert!(
            !manifest
                .lines()
                .any(|line| !line.trim_start().starts_with('#') && line.contains(forbidden)),
            "runtime gained a non-runtime dependency: {forbidden}"
        );
    }
    // The scenario uses only generic runtime mechanisms.
    let steps = scenario_steps("http://127.0.0.1/evidence");
    for forbidden in [
        "git ",
        "cargo ",
        "npm ",
        "python ",
        "rustc ",
        "pull request",
    ] {
        assert!(
            !steps.contains(forbidden),
            "acceptance scenario gained a coding-specific action: {forbidden}"
        );
    }
}

#[test]
fn negative_cases_cover_untrusted_plugin_denied_paths_and_missing_capabilities() {
    // An untrusted plugin is refused before the Worker serves at all.
    let data = tempfile::tempdir().expect("data");
    let untrusted = acceptance(data.path(), "acceptance-untrusted", false, "deterministic");
    let output = Command::new(env!("CARGO_BIN_EXE_acceptance-fixture"))
        .args([
            "serve",
            "--config",
            untrusted.fixture.config.to_str().expect("config"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("untrusted serve exits");
    assert!(
        !output.status.success(),
        "untrusted plugin must fail startup"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("plugin.untrusted"), "{stderr}");

    // A child subagent can be cancelled and reports the truthful terminal
    // state without losing its durable identity.
    let data = tempfile::tempdir().expect("data");
    let cancel_fixture = acceptance(data.path(), "acceptance-cancel", true, "deterministic");
    std::fs::write(
        cancel_fixture.workspace.join("slow-input.txt"),
        "delay:30000 child:cancel-me",
    )
    .expect("slow child input");
    let served = serve(&cancel_fixture);
    let base = served.base.clone();
    create_profile(&base, PROFILE, None);
    let steps = json!([
        {"tool": "read_file", "args": {"path": "slow-input.txt"}},
        {"tool": "spawn_subagent", "args": {"definition": "helper", "input": "$last.content", "background": true}},
        {"tool": "interrupt_subagent", "args": {"task_id": "$last.task_id"}},
        {"tool": "wait_subagent", "args": {"task_id": "$last.task_id", "timeout_ms": 3000}}
    ]);
    let task_id = submit(
        &base,
        "acceptance-cancel",
        &format!("steps:{steps}"),
        Some(PROFILE),
    );
    let task = wait_task(&base, &task_id);
    assert_eq!(task["state"], "succeeded", "{task}");
    let output = task["output"].as_str().unwrap_or_default();
    assert!(output.contains("cancelled"), "{output}");
    let cancel_events = events(&base, &task_id);
    assert!(
        cancel_events
            .iter()
            .any(|event| event["kind"] == "subagent.interrupted"),
        "{cancel_events:?}"
    );
    let terminal =
        event_kind(&cancel_events, "subagent.terminal").expect("child terminal evidence");
    assert_eq!(terminal["payload"]["state"], "cancelled", "{terminal}");
    assert!(
        cancel_events
            .iter()
            .any(|event| event["kind"] == "subagent.stop"),
        "cancelled child did not emit subagent.stop: {cancel_events:?}"
    );

    // Compaction pressure keeps canonical history while emitting native
    // compaction evidence.
    create_profile(
        &base,
        "compaction",
        Some(json!({
            "strategy": "truncation",
            "context_budget": 700,
            "preserve_recent": 2,
            "max_retries": 2
        })),
    );
    let mut last = Value::Null;
    for turn in 1..=5 {
        let input = format!("turn{turn}:{}", "x".repeat(900));
        let task_id = submit(&base, "acceptance-compaction", &input, Some("compaction"));
        last = wait_task(&base, &task_id);
        assert_eq!(last["state"], "succeeded", "{last}");
    }
    let last_id = last["id"].as_str().expect("compaction task");
    let events = events(&base, last_id);
    let compacted = event_kind(&events, "context.compacted").expect("compaction evidence");
    assert_eq!(compacted["payload"]["strategy"], "truncation");
    assert!(
        compacted["payload"]["events_after"]
            .as_u64()
            .unwrap_or(u64::MAX)
            <= compacted["payload"]["events_before"].as_u64().unwrap_or(0),
        "{compacted}"
    );

    // An unavailable MCP server fails the call truthfully and performs no
    // external effect.
    let data = tempfile::tempdir().expect("data");
    let broken = acceptance(data.path(), "acceptance-mcp-down", true, "exit_on_start");
    let served = serve(&broken);
    let base = served.base.clone();
    create_profile(&base, PROFILE, None);
    let task_id = submit(
        &base,
        "acceptance-mcp-down",
        "call:mcp__fixture__record_effect {\"value\":\"never\"}",
        Some(PROFILE),
    );
    let task = wait_task(&base, &task_id);
    assert_ne!(task["state"], "succeeded", "{task}");
    assert!(
        std::fs::read_to_string(&broken.mcp_effect)
            .unwrap_or_default()
            .is_empty(),
        "an unavailable MCP server must not record an effect"
    );
    let servers = get(&base, "/v1/mcp/servers");
    assert_ne!(servers["servers"][0]["state"], "connected", "{servers}");
}
