//! Hook event ordering, timeouts, plugin trust, and plugin asset activation
//! through the real General Worker runtime.
//!
//! These tests assert external behavior: task events, capability projections,
//! plugin scripts actually executed, and fail-closed startup diagnostics. They
//! do not assert private hook dispatch internals.

use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

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
    Worker {
        child,
        base,
        token: "dev-token".into(),
    }
}

fn spawn_failure(config: &Path, log: &Path, cwd: &Path) -> (bool, String) {
    let stderr = File::create(log).expect("stderr log");
    let status = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .status()
        .expect("worker exits");
    (
        status.success(),
        std::fs::read_to_string(log).unwrap_or_default(),
    )
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
        for _ in 0..1_000 {
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
            std::thread::sleep(Duration::from_millis(10));
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

    fn capabilities(&self) -> Value {
        reqwest::blocking::Client::new()
            .get(format!("{}/v1/capabilities", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("capabilities respond")
            .json()
            .expect("capabilities JSON")
    }

    fn commands(&self) -> Value {
        reqwest::blocking::Client::new()
            .get(format!("{}/v1/commands", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("commands respond")
            .json()
            .expect("commands JSON")
    }

    fn stop(mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let _ = self.child.wait();
    }
}

struct PluginSpec<'a> {
    name: &'a str,
    log: &'a Path,
    events: &'a [(&'a str, Option<&'a str>)],
    allow_deny: bool,
    slow_post_tool_ms: u64,
    skill: Option<&'a str>,
    command: bool,
    tools: &'a [&'a str],
}

fn plugin_script(spec: &PluginSpec<'_>) -> String {
    let log = spec.log.display();
    let deny = if spec.allow_deny { "True" } else { "False" };
    let slow = spec.slow_post_tool_ms;
    format!(
        r#"#!/usr/bin/env python3
import json
import sys
import time
data = json.load(sys.stdin)
event = data.get("event", "")
payload = data.get("payload", {{}})
tool = payload.get("tool", "")
with open(r"{log}", "a") as handle:
    handle.write(f"{{event}}|{{tool}}\n")
if event == "post_tool" and {slow} > 0:
    time.sleep({slow} / 1000.0)
if event == "pre_tool" and {deny}:
    print(json.dumps({{"decision": "deny", "reason": "plugin denied this tool"}}))
elif event == "prompt_submit" and "deny-me" in payload.get("input", ""):
    print(json.dumps({{"decision": "deny", "reason": "plugin denied this prompt"}}))
else:
    print(json.dumps({{"decision": "continue"}}))
"#
    )
}

fn write_plugin(root: &Path, spec: &PluginSpec<'_>) {
    let directory = root.join(spec.name);
    std::fs::create_dir_all(&directory).expect("plugin directory");
    let script = directory.join("hook.py");
    std::fs::write(&script, plugin_script(spec)).expect("hook script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("hook executable");
    }
    if let Some(name) = spec.skill {
        let skill = directory.join("skills").join(name);
        std::fs::create_dir_all(&skill).expect("plugin skill directory");
        std::fs::write(
            skill.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: Plugin-provided skill.\n---\nPlugin instructions."
            ),
        )
        .expect("plugin skill");
    }
    if spec.command {
        let commands = directory.join("commands");
        std::fs::create_dir_all(&commands).expect("plugin command directory");
        std::fs::write(
            commands.join("plugin_cmd.toml"),
            "description = \"Plugin command\"\nprompt = \"plugin command response\"\nagent = \"general\"\n",
        )
        .expect("plugin command");
    }
    let mut manifest = String::new();
    manifest.push_str(&format!(
        "manifest_version = 1\nname = \"{}\"\nversion = \"0.1.0\"\ndescription = \"hook fixture\"\n",
        spec.name
    ));
    if !spec.tools.is_empty() {
        manifest.push_str("tools = [");
        manifest.push_str(
            &spec
                .tools
                .iter()
                .map(|tool| format!("\"{tool}\""))
                .collect::<Vec<_>>()
                .join(", "),
        );
        manifest.push_str("]\n");
    }
    if spec.command {
        manifest.push_str("[commands]\ndirectory = \"commands\"\n");
    }
    if spec.skill.is_some() {
        manifest.push_str("[skills]\ndirectory = \"skills\"\n");
    }
    for (event, matcher) in spec.events {
        manifest.push_str("[[hooks]]\n");
        manifest.push_str(&format!(
            "name = \"{event}\"\nevent = \"{event}\"\ncommand = \"hook.py\"\ntimeout_ms = 5000\n"
        ));
        if let Some(matcher) = matcher {
            manifest.push_str(&format!("matcher = \"{matcher}\"\n"));
        }
    }
    std::fs::write(directory.join("plugin.toml"), manifest).expect("plugin manifest");
}

fn append_config(config: &Path, text: &str) {
    let mut current = std::fs::read_to_string(config).expect("worker config");
    current.push_str(text);
    std::fs::write(config, current).expect("worker config update");
}

fn hook_log(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Terminal task state is committed before stop hooks run, so evidence written
/// by a stop hook is observed by polling rather than assuming it already
/// happened.
fn wait_log(log: &Path, needle: &str) -> Vec<String> {
    for _ in 0..500 {
        let entries = hook_log(log);
        if entries.iter().any(|entry| entry == needle) {
            return entries;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("missing '{needle}' in {:?}", hook_log(log));
}

fn position(entries: &[String], needle: &str) -> usize {
    entries
        .iter()
        .position(|entry| entry == needle)
        .unwrap_or_else(|| panic!("missing '{needle}' in {entries:?}"))
}

#[test]
fn plugin_hooks_fire_in_order_and_bundle_assets_activate() {
    let cwd = tempfile::tempdir().expect("cwd");
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("plugin-hooks.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "hook-events".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    write_plugin(
        &data.path().join("plugins"),
        &PluginSpec {
            name: "hook-events",
            log: &log,
            events: &[
                ("session_start", None),
                ("prompt_submit", None),
                ("pre_tool", Some("exec_command")),
                ("post_tool", Some("exec_command")),
                ("stop", None),
                ("subagent_stop", None),
                ("session_end", None),
            ],
            allow_deny: false,
            slow_post_tool_ms: 0,
            skill: Some("plugin-extra"),
            command: true,
            tools: &["exec_command"],
        },
    );
    append_config(
        &fixture.config,
        "\n[plugins]\ndirectory = \"plugins\"\n\n[plugins.allow]\nhook-events = \"*\"\n",
    );
    let worker = spawn(&fixture.config, &data.path().join("stderr.log"), cwd.path());

    let task = worker.run("general", "sandbox:printf hook-events", "hook-events");
    assert_eq!(task["state"], "succeeded", "{task}");
    let task_id = task["id"].as_str().expect("task id");

    let entries = wait_log(&log, "stop|");
    let session_start = position(&entries, "session_start|");
    let prompt = position(&entries, "prompt_submit|");
    let pre = position(&entries, "pre_tool|exec_command");
    let post = position(&entries, "post_tool|exec_command");
    let stop = position(&entries, "stop|");
    assert!(
        session_start < prompt && prompt < pre && pre < post && post < stop,
        "hook order was {entries:?}"
    );

    let capabilities = worker.capabilities();
    let plugins = capabilities["plugins"]
        .as_array()
        .expect("plugin inventory");
    assert_eq!(plugins.len(), 1, "{capabilities}");
    assert_eq!(plugins[0]["name"], "hook-events");
    assert_eq!(plugins[0]["trust"], "allowlist");
    assert_eq!(plugins[0]["skills"], 1);
    assert_eq!(plugins[0]["commands"], 1);
    assert_eq!(plugins[0]["tools"][0], "exec_command");
    let components = plugins[0]["components"]
        .as_array()
        .expect("components")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(components.contains(&"commands:1"), "{components:?}");
    assert!(components.contains(&"skills:1"), "{components:?}");
    assert!(components.contains(&"hooks:7"), "{components:?}");

    let commands = worker.commands();
    assert!(
        commands
            .as_array()
            .expect("commands")
            .iter()
            .any(|command| command["name"] == "plugin_cmd"),
        "plugin command was not activated: {commands}"
    );

    let events = worker.events(task_id);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event["kind"].as_str(), Some("hook.failed" | "hook.timeout"))),
        "clean hooks produced failure evidence: {events:?}"
    );

    worker.stop();
    let entries = hook_log(&log);
    let session_end = position(&entries, "session_end|");
    assert!(
        position(&entries, "stop|") < session_end,
        "session end must follow stop: {entries:?}"
    );
}

#[test]
fn hook_timeout_is_typed_and_does_not_corrupt_the_task() {
    let cwd = tempfile::tempdir().expect("cwd");
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("slow.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "hook-timeout".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    write_plugin(
        &data.path().join("plugins"),
        &PluginSpec {
            name: "hook-timeout",
            log: &log,
            events: &[("post_tool", Some("exec_command"))],
            allow_deny: false,
            slow_post_tool_ms: 2_000,
            skill: None,
            command: false,
            tools: &["exec_command"],
        },
    );
    append_config(
        &fixture.config,
        "\n[hooks]\ntimeout_ms = 100\n\n[plugins]\ndirectory = \"plugins\"\n\n[plugins.allow]\nhook-timeout = \"*\"\n",
    );
    let worker = spawn(&fixture.config, &data.path().join("stderr.log"), cwd.path());

    let task = worker.run("general", "sandbox:printf timeout-ok", "hook-timeout");
    assert_eq!(task["state"], "succeeded", "{task}");
    assert!(
        task["output"]
            .as_str()
            .unwrap_or_default()
            .contains("timeout-ok"),
        "the tool result must survive a post-tool hook timeout: {task}"
    );
    let task_id = task["id"].as_str().expect("task id");
    let events = worker.events(task_id);
    let timeout = events
        .iter()
        .find(|event| event["kind"] == "hook.timeout")
        .unwrap_or_else(|| panic!("typed hook.timeout event missing: {events:?}"));
    assert_eq!(timeout["payload"]["event"], "post_tool");
    assert_eq!(timeout["payload"]["kind"], "timeout");
    assert_eq!(timeout["payload"]["hook"], "plugin:hook-timeout");
    assert!(timeout["payload"]["timeout_ms"].as_u64().is_some());
    assert!(
        events.iter().any(|event| event["kind"] == "tool.completed"),
        "tool evidence must still commit: {events:?}"
    );
    assert!(
        !std::fs::read_to_string(data.path().join("stderr.log"))
            .unwrap_or_default()
            .contains("sk-"),
        "hook evidence leaked a secret marker"
    );
}

#[test]
fn plugin_cannot_override_a_denied_permission_or_prompt_block() {
    let cwd = tempfile::tempdir().expect("cwd");
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("deny.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "hook-deny".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    write_plugin(
        &data.path().join("plugins"),
        &PluginSpec {
            name: "hook-deny",
            log: &log,
            events: &[("prompt_submit", None), ("pre_tool", Some("exec_command"))],
            allow_deny: true,
            slow_post_tool_ms: 0,
            skill: None,
            command: false,
            tools: &["exec_command"],
        },
    );
    append_config(
        &fixture.config,
        "\n[permissions]\nmode = \"default\"\n\n[[permissions.rules]]\nid = \"deny-exec\"\ndecision = \"deny\"\ntools = [\"exec_command\"]\npriority = 100\n\n[plugins]\ndirectory = \"plugins\"\n\n[plugins.allow]\nhook-deny = \"*\"\n",
    );
    let worker = spawn(&fixture.config, &data.path().join("stderr.log"), cwd.path());

    let canary = data.path().join("plan-canary");
    let task = worker.run(
        "general",
        &format!("sandbox:touch {}", canary.display()),
        "plan-denied",
    );
    assert_eq!(task["state"], "succeeded", "{task}");
    assert!(
        !canary.exists(),
        "a pre-tool hook allow decision bypassed plan mode"
    );
    let events = worker.events(task["id"].as_str().expect("task id"));
    assert!(
        events.iter().any(|event| {
            event["kind"] == "permission.decision" && event["payload"]["decision"] == "deny"
        }),
        "the permission denial must remain authoritative: {events:?}"
    );
    assert!(
        !hook_log(&log)
            .iter()
            .any(|entry| entry == "pre_tool|exec_command"),
        "a pre-tool hook ran for a call the policy denied: {:?}",
        hook_log(&log)
    );

    let blocked = worker.run("general", "deny-me", "prompt-denied");
    assert_eq!(blocked["state"], "failed", "{blocked}");
    assert_eq!(blocked["error"]["code"], "hook.prompt_blocked", "{blocked}");
    assert!(hook_log(&log).iter().any(|entry| entry == "prompt_submit|"));
}

#[test]
fn untrusted_and_tampered_plugins_fail_closed() {
    let cwd = tempfile::tempdir().expect("cwd");
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("untrusted.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "plugin-untrusted".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    write_plugin(
        &data.path().join("plugins"),
        &PluginSpec {
            name: "untrusted",
            log: &log,
            events: &[],
            allow_deny: false,
            slow_post_tool_ms: 0,
            skill: Some("untrusted-skill"),
            command: false,
            tools: &["exec_command"],
        },
    );
    append_config(&fixture.config, "\n[plugins]\ndirectory = \"plugins\"\n");
    let (success, stderr) = spawn_failure(&fixture.config, &log, cwd.path());
    assert!(!success, "untrusted plugin must prevent startup");
    assert!(
        stderr.contains("plugin.untrusted"),
        "diagnostic must name the trust failure: {stderr}"
    );

    append_config(
        &fixture.config,
        "\n[plugins.allow]\nuntrusted = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\n",
    );
    let (success, stderr) = spawn_failure(&fixture.config, &log, cwd.path());
    assert!(!success, "tampered plugin must prevent startup");
    assert!(
        stderr.contains("plugin.tampered"),
        "diagnostic must name the digest mismatch: {stderr}"
    );
}

#[test]
fn signed_plugin_activates_with_recorded_signer() {
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};

    let cwd = tempfile::tempdir().expect("cwd");
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("signed.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "plugin-signed".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    write_plugin(
        &data.path().join("plugins"),
        &PluginSpec {
            name: "signed",
            log: &log,
            events: &[],
            allow_deny: false,
            slow_post_tool_ms: 0,
            skill: Some("signed-skill"),
            command: false,
            tools: &["exec_command"],
        },
    );
    let root = data.path().join("plugins").join("signed");
    let digest = runtime::plugins::trust::content_digest(&root).expect("content digest");
    let manifest_path = root.join("plugin.toml");
    let mut text = std::fs::read_to_string(&manifest_path).expect("manifest");
    text.push_str(&format!(
        "\n[trust]\nsigner = \"test-signer\"\ndigest = \"{digest}\"\n"
    ));
    std::fs::write(&manifest_path, text).expect("signed manifest");

    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let public =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes());
    let manifest = runtime::plugins::manifest::parse(&manifest_path).expect("manifest parses");
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        signing
            .sign(manifest.signing_digest().as_bytes())
            .to_bytes(),
    );
    let mut text = std::fs::read_to_string(&manifest_path).expect("manifest");
    text.push_str(&format!("signature = \"{signature}\"\n"));
    std::fs::write(&manifest_path, text).expect("signed manifest");

    append_config(
        &fixture.config,
        &format!(
            "\n[plugins]\ndirectory = \"plugins\"\n\n[plugins.trusted_signers]\n\"test-signer\" = \"{public}\"\n"
        ),
    );
    let worker = spawn(&fixture.config, &data.path().join("stderr.log"), cwd.path());
    let capabilities = worker.capabilities();
    let plugin = &capabilities["plugins"][0];
    assert_eq!(plugin["name"], "signed", "{capabilities}");
    assert_eq!(plugin["trust"], "signature", "{capabilities}");
    assert_eq!(plugin["signer"], "test-signer", "{capabilities}");
    assert_eq!(plugin["digest"], digest, "{capabilities}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_activation_and_removal_are_idempotent() {
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("idempotent.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "plugin-idempotent".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    write_plugin(
        &data.path().join("plugins"),
        &PluginSpec {
            name: "idempotent",
            log: &log,
            events: &[("session_start", None)],
            allow_deny: false,
            slow_post_tool_ms: 0,
            skill: Some("plugin-idempotent"),
            command: false,
            tools: &["exec_command"],
        },
    );
    append_config(
        &fixture.config,
        "\n[plugins]\ndirectory = \"plugins\"\n\n[plugins.allow]\nidempotent = \"*\"\n",
    );

    let first = general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
        .expect("first start");
    assert_eq!(first.plugin_inventory().await.len(), 1);
    first.shutdown().await;
    let first_record =
        std::fs::read_to_string(data.path().join("plugins.json")).expect("activation record");
    assert!(first_record.contains("idempotent"), "{first_record}");

    let second = general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
        .expect("second start");
    assert_eq!(second.plugin_inventory().await.len(), 1);
    second.shutdown().await;
    let second_record =
        std::fs::read_to_string(data.path().join("plugins.json")).expect("activation record");
    assert_eq!(
        first_record, second_record,
        "repeated activation changed the record"
    );

    std::fs::remove_dir_all(data.path().join("plugins").join("idempotent"))
        .expect("plugin removed");
    let third = general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
        .expect("third start");
    assert!(
        third.plugin_inventory().await.is_empty(),
        "removal must be idempotent"
    );
    third.shutdown().await;
    let third_record =
        std::fs::read_to_string(data.path().join("plugins.json")).expect("activation record");
    assert!(!third_record.contains("idempotent"), "{third_record}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_skill_collision_does_not_overwrite_worker_asset() {
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("collision.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "plugin-collision".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    // The fixture owns a skill named `tone`.
    write_plugin(
        &data.path().join("plugins"),
        &PluginSpec {
            name: "collision",
            log: &log,
            events: &[],
            allow_deny: false,
            slow_post_tool_ms: 0,
            skill: Some("tone"),
            command: false,
            tools: &[],
        },
    );
    append_config(
        &fixture.config,
        "\n[plugins]\ndirectory = \"plugins\"\n\n[plugins.allow]\ncollision = \"*\"\n",
    );
    let error = match general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
    {
        Ok(_) => panic!("collision must fail closed"),
        Err(error) => error,
    };
    assert_eq!(error.code, "asset.skill_duplicate", "{error:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_stop_hook_fires_through_the_runtime() {
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("subagent.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "subagent-stop".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    write_plugin(
        &data.path().join("plugins"),
        &PluginSpec {
            name: "subagent",
            log: &log,
            events: &[("subagent_stop", None)],
            allow_deny: false,
            slow_post_tool_ms: 0,
            skill: None,
            command: false,
            tools: &[],
        },
    );
    append_config(
        &fixture.config,
        "\n[plugins]\ndirectory = \"plugins\"\n\n[plugins.allow]\nsubagent = \"*\"\n",
    );
    let instance = general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
        .expect("worker");
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    let task = runtime
        .submit_task(
            &admin,
            common::SubmitTaskRequest {
                namespace: Some("local".into()),
                agent: "general".into(),
                session_id: "subagent-session".into(),
                input: "sandbox:printf parent".into(),
                idempotency_key: "subagent-parent".into(),
                profile: None,
                team: None,
                worker: None,
                limits: common::Limits::default(),
            },
        )
        .await
        .expect("parent task");
    wait_terminal(&runtime, &task.task_id).await;
    runtime
        .notify_subagent_stop(
            &task.task_id,
            "child-1",
            "general",
            "child-session",
            "succeeded",
        )
        .await
        .expect("subagent stop accepted");
    let events: Vec<String> = runtime
        .events_since(&task.task_id, 0, 10_000)
        .await
        .expect("events")
        .events
        .into_iter()
        .map(|event| event.kind)
        .collect();
    assert!(
        events.iter().any(|kind| kind == "subagent.stop"),
        "{events:?}"
    );
    assert!(
        hook_log(&log).iter().any(|entry| entry == "subagent_stop|"),
        "external subagent-stop hook did not run: {:?}",
        hook_log(&log)
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_and_post_compact_hooks_wrap_native_compaction() {
    let data = tempfile::tempdir().expect("data");
    let log = data.path().join("compact.log");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "compact-hooks".into(),
            token: Some("dev-token".into()),
            model: "fixture".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    append_provider(&fixture.config, "fixture", 8_000);
    write_plugin(
        &data.path().join("plugins"),
        &PluginSpec {
            name: "compact",
            log: &log,
            events: &[("pre_compact", None), ("post_compact", None)],
            allow_deny: false,
            slow_post_tool_ms: 0,
            skill: None,
            command: false,
            tools: &[],
        },
    );
    append_config(
        &fixture.config,
        "\n[plugins]\ndirectory = \"plugins\"\n\n[plugins.allow]\ncompact = \"*\"\n",
    );
    let instance = general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
        .expect("worker");
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    let alice = common::Principal::new("alice", "local", &["*"]);
    runtime
        .create_profile(&admin, "local", "context", compact_revision())
        .await
        .expect("profile created");
    runtime
        .activate_profile(&admin, "local", "context", 1)
        .await
        .expect("profile activated");

    let mut last = None;
    for turn in 1..=6 {
        let submitted = runtime
            .submit_task(
                &alice,
                common::SubmitTaskRequest {
                    namespace: Some("local".into()),
                    agent: "general".into(),
                    session_id: "compact-session".into(),
                    input: long_input(turn, 1_500),
                    idempotency_key: format!("compact-{turn}"),
                    profile: Some("context".into()),
                    team: None,
                    worker: None,
                    limits: common::Limits::default(),
                },
            )
            .await
            .expect("task admitted");
        let task = wait_terminal(&runtime, &submitted.task_id).await;
        assert_eq!(task.state, common::TaskState::Succeeded, "{task:?}");
        last = Some(task);
    }
    let last = last.expect("last task");
    let events: Vec<String> = runtime
        .events_since(&last.id, 0, 10_000)
        .await
        .expect("events")
        .events
        .into_iter()
        .map(|event| event.kind)
        .collect();
    assert!(
        events.iter().any(|kind| kind == "context.compacted"),
        "native compaction must still run: {events:?}"
    );
    let entries = hook_log(&log);
    let pre = position(&entries, "pre_compact|");
    let post = position(&entries, "post_compact|");
    assert!(pre < post, "compact hook order was {entries:?}");
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.as_str() == "pre_compact|")
            .count(),
        entries
            .iter()
            .filter(|entry| entry.as_str() == "post_compact|")
            .count(),
        "every pre-compact hook must have one post-compact counterpart"
    );
    instance.shutdown().await;
}

fn compact_revision() -> common::ProfileRevision {
    common::ProfileRevision {
        revision: 0,
        instruction: "Answer from the conversation history.".into(),
        model: "fixture".into(),
        capabilities: vec!["general".into()],
        grants: vec!["task:submit".into()],
        restrictions: Vec::new(),
        composition: "single".into(),
        members: vec!["general".into()],
        skills: Vec::new(),
        mcp: Vec::new(),
        workflow: None,
        limits: common::Limits::default(),
        compaction: Some(common::ContextCompaction {
            strategy: "truncation".into(),
            context_budget: 1_300,
            preserve_recent: 3,
            max_retries: 2,
        }),
        secret_refs: Vec::new(),
        created_at: 0,
    }
}

fn long_input(turn: usize, width: usize) -> String {
    format!("turn{turn}-{}", "x".repeat(width))
}

fn append_provider(config: &Path, id: &str, window: u64) {
    let mut text = std::fs::read_to_string(config).expect("worker config");
    text.push_str(&format!(
        "\n[providers.{id}]\nadapter = \"deterministic\"\nmodel = \"{id}\"\nmodalities = [\"text\"]\ntool_calling = true\nlocal = true\nresidency = \"local\"\ncontext_window = {window}\ncost_rank = 0\ndata_class = \"local\"\n"
    ));
    std::fs::write(config, text).expect("provider config");
}

async fn wait_terminal(runtime: &runtime::Runtime, task_id: &str) -> common::Task {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let task = runtime.task_unscoped(task_id).await.expect("task exists");
        if task.state.is_terminal() {
            return task;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "task did not finish: {task:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
