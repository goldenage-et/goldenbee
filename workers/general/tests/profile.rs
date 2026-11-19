//! Worker-native profile management and activated native composition.

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};

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

fn cli(config: &Path, arguments: &[&str]) -> Output {
    let cwd = tempfile::tempdir().expect("unrelated cwd");
    let mut args = vec!["--config", config.to_str().expect("config path")];
    args.extend_from_slice(arguments);
    Command::new(env!("CARGO_BIN_EXE_general"))
        .args(args)
        .current_dir(cwd.path())
        .output()
        .expect("general runs")
}

fn cli_json(config: &Path, arguments: &[&str]) -> Value {
    let output = cli(config, arguments);
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn launcher(config: &Path, profile: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "dev",
            "--config",
            config.to_str().expect("config path"),
            "--profile",
            profile,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launcher starts");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"prompt:\nexit\n")
        .expect("launcher input");
    child.wait_with_output().expect("launcher exits")
}

fn spawn(config: &Path) -> Worker {
    let cwd = tempfile::tempdir().expect("unrelated cwd");
    let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .current_dir(cwd.path())
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
    Worker {
        child,
        base,
        token: "dev-token".into(),
    }
}

impl Worker {
    fn submit(&self, profile: &str, input: &str, key: &str) -> Value {
        let response = reqwest::blocking::Client::new()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "agent": "general",
                "session_id": format!("session-{key}"),
                "input": input,
                "idempotency_key": key,
                "profile": profile,
            }))
            .send()
            .expect("submit responds");
        let status = response.status();
        let body = response.text().expect("submit body");
        assert!(status.is_success(), "submit failed: {status} {body}");
        serde_json::from_str(&body).expect("submit JSON")
    }

    fn wait(&self, task_id: &str) -> Value {
        for _ in 0..500 {
            let task: Value = reqwest::blocking::Client::new()
                .get(format!("{}/v1/tasks/{task_id}", self.base))
                .bearer_auth(&self.token)
                .send()
                .expect("task responds")
                .json()
                .expect("task JSON");
            if matches!(
                task["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled" | "unknown")
            ) {
                return task;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("task did not reach a terminal state");
    }

    fn run(&self, profile: &str, input: &str, key: &str) -> Value {
        let submitted = self.submit(profile, input, key);
        self.wait(submitted["task_id"].as_str().expect("task id"))
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

fn create_research(config: &Path, instruction: &str, extra: &[&str]) {
    let mut args = vec![
        "profile",
        "create",
        "research",
        "--instruction",
        instruction,
        "--model",
        "deterministic",
        "--composition",
        "single",
        "--member",
        "general",
        "--capability",
        "general",
        "--grant",
        "task:submit",
        "--activate",
    ];
    args.extend_from_slice(extra);
    cli_json(config, &args);
}

#[test]
fn cli_management_activates_revisions_and_native_execution_uses_them() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "profiles".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: vec![("api_key".into(), "sk-export-canary".into())],
            token_scopes: vec!["*".into()],
        },
    );
    create_research(
        &fixture.config,
        "PROFILE-ONE sk-export-canary",
        &["--secret-ref", "secret://provider-key"],
    );

    let launcher = launcher(&fixture.config, "research");
    assert!(launcher.status.success());
    assert!(
        String::from_utf8_lossy(&launcher.stdout).contains("PROFILE-ONE"),
        "{}",
        String::from_utf8_lossy(&launcher.stdout)
    );

    let mut worker = spawn(&fixture.config);
    let first = worker.run("research", "prompt:", "profile-v1");
    assert!(output(&first).contains("PROFILE-ONE"), "{first}");
    assert_eq!(first["profile_revision"], 1);
    worker.stop();

    let revised = cli_json(
        &fixture.config,
        &[
            "profile",
            "revise",
            "research",
            "--expected",
            "1",
            "--instruction",
            "PROFILE-TWO",
            "--model",
            "deterministic",
            "--composition",
            "single",
            "--member",
            "general",
            "--capability",
            "general",
            "--grant",
            "task:submit",
            "--secret-ref",
            "secret://provider-key",
            "--activate",
        ],
    );
    assert_eq!(revised["active_revision"], 2);
    assert_eq!(revised["revisions"].as_array().unwrap().len(), 2);

    let shown = cli_json(
        &fixture.config,
        &["profile", "show", "research", "--revision", "1"],
    );
    assert_eq!(shown["instruction"], "PROFILE-ONE sk-export-canary");
    let exported = cli_json(&fixture.config, &["profile", "export", "research"]);
    assert!(exported["profile"]["revisions"][0]["secret_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "secret://provider-key"));
    let text = serde_json::to_string(&exported).expect("export text");
    assert!(!text.contains("sk-export-canary"), "{text}");
    assert!(text.contains("[REDACTED]"), "{text}");

    // Stale optimistic update is rejected without changing the active revision.
    let stale = cli(
        &fixture.config,
        &[
            "profile",
            "revise",
            "research",
            "--expected",
            "1",
            "--instruction",
            "STALE",
        ],
    );
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("profile.revision_conflict"));
    let after_stale = cli_json(&fixture.config, &["profile", "show", "research"]);
    assert_eq!(after_stale["active_revision"], 2);

    // Invalid model is rejected and the prior revision stays active.
    let invalid = cli(
        &fixture.config,
        &[
            "profile",
            "revise",
            "research",
            "--expected",
            "2",
            "--instruction",
            "INVALID",
            "--model",
            "absent",
        ],
    );
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("profile.model_unsupported"));
    let after_invalid = cli_json(&fixture.config, &["profile", "show", "research"]);
    assert_eq!(after_invalid["active_revision"], 2);

    let mut worker = spawn(&fixture.config);
    let second = worker.run("research", "prompt:", "profile-v2");
    assert!(output(&second).contains("PROFILE-TWO"), "{second}");
    assert_eq!(second["profile_revision"], 2);
    worker.stop();

    let deactivated = cli_json(&fixture.config, &["profile", "deactivate", "research"]);
    assert_eq!(deactivated["active_revision"], 0);
    let reactivated = cli_json(
        &fixture.config,
        &["profile", "activate", "research", "--revision", "2"],
    );
    assert_eq!(reactivated["active_revision"], 2);
}

#[test]
fn two_instances_activate_different_profiles_without_shared_state() {
    let first_data = tempfile::tempdir().expect("first data");
    let second_data = tempfile::tempdir().expect("second data");
    let first = fixture::write(
        first_data.path(),
        fixture::Options {
            instance: "profile-a".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let second = fixture::write(
        second_data.path(),
        fixture::Options {
            instance: "profile-b".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    create_research(&first.config, "PROFILE-INSTANCE-A", &[]);
    create_research(&second.config, "PROFILE-INSTANCE-B", &[]);

    let first_worker = spawn(&first.config);
    let second_worker = spawn(&second.config);
    let first_task = first_worker.run("research", "prompt:", "profile-a-task");
    let second_task = second_worker.run("research", "prompt:", "profile-b-task");
    assert!(
        output(&first_task).contains("PROFILE-INSTANCE-A"),
        "{first_task}"
    );
    assert!(
        output(&second_task).contains("PROFILE-INSTANCE-B"),
        "{second_task}"
    );
    assert!(!output(&first_task).contains("INSTANCE-B"));
    assert!(!output(&second_task).contains("INSTANCE-A"));
}

#[test]
fn denied_and_escalating_profile_updates_are_rejected() {
    let restricted_data = tempfile::tempdir().expect("restricted data");
    let restricted = fixture::write(
        restricted_data.path(),
        fixture::Options {
            instance: "restricted-profile".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec![
                "task:submit".into(),
                "task:read".into(),
                "profile:read".into(),
            ],
        },
    );
    let denied = cli(
        &restricted.config,
        &[
            "profile",
            "create",
            "research",
            "--instruction",
            "DENIED",
            "--member",
            "general",
            "--capability",
            "general",
            "--grant",
            "task:submit",
        ],
    );
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("scope.denied"));
    let denied_revise = cli(
        &restricted.config,
        &[
            "profile",
            "revise",
            "default",
            "--expected",
            "1",
            "--instruction",
            "DENIED",
        ],
    );
    assert!(!denied_revise.status.success());
    assert!(String::from_utf8_lossy(&denied_revise.stderr).contains("scope.denied"));
    let denied_activate = cli(
        &restricted.config,
        &["profile", "activate", "default", "--revision", "1"],
    );
    assert!(!denied_activate.status.success());
    assert!(String::from_utf8_lossy(&denied_activate.stderr).contains("scope.denied"));

    let profiles = cli_json(&restricted.config, &["profile", "list", "--json"]);
    assert!(
        !profiles
            .as_array()
            .unwrap()
            .iter()
            .any(|profile| profile["name"] == "research"),
        "denied create must not add a profile"
    );

    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "escalation".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let grant_data = tempfile::tempdir().expect("grant data");
    let grant_fixture = fixture::write(
        grant_data.path(),
        fixture::Options {
            instance: "instance-grants".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let config_text = std::fs::read_to_string(&grant_fixture.config).unwrap();
    std::fs::write(
        &grant_fixture.config,
        config_text.replace("\n[paths]", "\ngrants = [\"task:submit\"]\n\n[paths]"),
    )
    .unwrap();
    let outside_instance = cli(
        &grant_fixture.config,
        &[
            "profile",
            "create",
            "research",
            "--instruction",
            "GRANT",
            "--member",
            "general",
            "--capability",
            "general",
            "--grant",
            "admin",
        ],
    );
    assert!(!outside_instance.status.success());
    assert!(
        String::from_utf8_lossy(&outside_instance.stderr).contains("profile.grant_escalation"),
        "{}",
        String::from_utf8_lossy(&outside_instance.stderr)
    );

    for (label, extra, code) in [
        (
            "absent capability",
            vec!["--capability", "absent"],
            "profile.capability_unavailable",
        ),
        (
            "absent member",
            vec!["--member", "absent"],
            "profile.capability_unavailable",
        ),
        (
            "absent skill",
            vec!["--skill", "absent"],
            "profile.skill_unavailable",
        ),
    ] {
        let mut args = vec![
            "profile",
            "create",
            "research",
            "--instruction",
            "ESCALATION",
            "--capability",
            "general",
            "--grant",
            "task:submit",
        ];
        args.extend_from_slice(&extra);
        let output = cli(&fixture.config, &args);
        assert!(!output.status.success(), "{label}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(code),
            "{label}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn deleting_an_inactive_profile_leaves_the_package_and_other_profiles() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "delete-profile".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    create_research(&fixture.config, "PROFILE-DELETE", &[]);
    let default_before = cli_json(&fixture.config, &["profile", "show", "default"]);

    // An active profile cannot be removed; the package and other profiles are
    // untouched by the rejected attempt.
    let active = cli(&fixture.config, &["profile", "delete", "research"]);
    assert!(!active.status.success());
    assert!(
        String::from_utf8_lossy(&active.stderr).contains("profile.active"),
        "{}",
        String::from_utf8_lossy(&active.stderr)
    );
    assert_eq!(
        cli_json(&fixture.config, &["profile", "show", "research"])["active_revision"],
        1
    );

    cli_json(&fixture.config, &["profile", "deactivate", "research"]);
    let deleted = cli_json(&fixture.config, &["profile", "delete", "research"]);
    assert_eq!(deleted["deleted"], "research");
    let profiles = cli_json(&fixture.config, &["profile", "list", "--json"]);
    assert!(!profiles
        .as_array()
        .unwrap()
        .iter()
        .any(|profile| profile["name"] == "research"));
    // Configuration lifecycle never uninstalls or rewrites the package.
    assert_eq!(
        cli_json(&fixture.config, &["profile", "show", "default"])["revisions"],
        default_before["revisions"]
    );
    let capabilities = cli(&fixture.config, &["capabilities", "--json"]);
    assert!(
        capabilities.status.success(),
        "{}",
        String::from_utf8_lossy(&capabilities.stderr)
    );
    let version = Command::new(env!("CARGO_BIN_EXE_general"))
        .arg("--version")
        .output()
        .expect("version runs");
    assert!(version.status.success());
}
