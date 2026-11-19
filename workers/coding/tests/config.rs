//! Operator configuration, asset loading, launcher modes, and profile roles.

mod fixture;

use fixture::{b64, fixed_lib};
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn two_configs_outside_install_dir_and_third_cwd_are_distinct() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let first = fixture::write(
        &root.path().join("config-a"),
        fixture::Options {
            instance: "config-a".into(),
            default_agent: "coder".into(),
            prompt_canary: "PROMPT-CONFIG-A".into(),
            skill: "tone-a".into(),
            ..Default::default()
        },
    );
    let second = fixture::write(
        &root.path().join("config-b"),
        fixture::Options {
            instance: "config-b".into(),
            default_agent: "coder".into(),
            prompt_canary: "PROMPT-CONFIG-B".into(),
            skill: "tone-b".into(),
            ..Default::default()
        },
    );
    let cwd = tempfile::tempdir().unwrap();

    for (fixture, agent, canary, skill) in [
        (&first, "coder", "PROMPT-CONFIG-A", "tone-a"),
        (&second, "auditor", "PROMPT-CONFIG-B", "tone-b"),
    ] {
        let output = fixture::cli(fixture, cwd.path(), &["capabilities", "--json"]);
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let configured = value["configured"].to_string();
        assert!(configured.contains(agent), "{configured}");
        assert!(configured.contains(skill), "{configured}");

        let output = fixture::cli(
            fixture,
            cwd.path(),
            &["run", "--agent", agent, "--input", "prompt:", "--json"],
        );
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let output = value["output"].as_str().unwrap_or_default();
        assert!(output.contains(canary), "{output}");
    }

    // Same immutable executable, second config, reviewer role: no write.
    let server = fixture::start(&second, cwd.path());
    let task = server.run_with_evidence(
        "reviewer",
        &format!("coding-write:denied.txt:{}", b64("NOPE")),
        "two-config-review",
    );
    assert!(!second.worktree.join("denied.txt").exists());
    assert_eq!(task["evidence"]["accepted"], false, "{task}");
}

#[test]
fn profile_roles_are_assignments_not_extra_binaries() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        &root.path().join("config"),
        fixture::Options {
            instance: "coding-profiles".into(),
            ..Default::default()
        },
    );
    let cwd = tempfile::tempdir().unwrap();
    let base = [
        "--model",
        "deterministic",
        "--grant",
        "task:submit",
        "--grant",
        "session:write",
        "--capability",
        "developer",
        "--capability",
        "reviewer",
        "--activate",
    ];
    for (name, member, composition) in [
        ("developer-role", "developer", "single"),
        ("reviewer-role", "reviewer", "single"),
    ] {
        let mut args = vec![
            "profile",
            "create",
            name,
            "--instruction",
            "role profile",
            "--member",
            member,
            "--composition",
            composition,
        ];
        args.extend_from_slice(&base);
        let output = fixture::cli(&fixture, cwd.path(), &args);
        assert!(
            output.status.success(),
            "profile {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut master = vec![
        "profile",
        "create",
        "master-role",
        "--instruction",
        "coordinate",
        "--member",
        "developer",
        "--member",
        "reviewer",
        "--composition",
        "sequential",
    ];
    master.extend_from_slice(&base);
    let output = fixture::cli(&fixture, cwd.path(), &master);
    assert!(
        output.status.success(),
        "master profile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = fixture::cli(
        &fixture,
        cwd.path(),
        &["profile", "show", "master-role", "--json"],
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["active_revision"], 1);
    assert_eq!(value["revisions"][0]["composition"], "sequential");
    assert_eq!(
        value["revisions"][0]["members"],
        serde_json::json!(["developer", "reviewer"])
    );

    let server = fixture::start(&fixture, cwd.path());
    let developer = server.run_profile_with_evidence(
        "developer",
        "developer-role",
        &format!(
            "coding-fix:src/lib.rs:{}:sh tests/check.sh",
            b64(fixed_lib())
        ),
        "profile-developer",
    );
    assert_eq!(developer["evidence"]["accepted"], true, "{developer}");
    let reviewer = server.run_profile_with_evidence(
        "developer",
        "reviewer-role",
        &format!("coding-write:profile-denied.txt:{}", b64("NOPE")),
        "profile-reviewer",
    );
    assert!(!fixture.worktree.join("profile-denied.txt").exists());
    assert_eq!(reviewer["evidence"]["accepted"], false, "{reviewer}");
}

#[test]
fn launcher_dev_and_web_modes_execute_real_capabilities() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        &root.path().join("config"),
        fixture::Options {
            instance: "coding-launcher".into(),
            ..Default::default()
        },
    );
    let cwd = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_coding"))
        .args(["dev", "--config", fixture.config.to_str().unwrap()])
        .current_dir(cwd.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("dev launcher starts");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        let input = format!(
            "coding-fix:src/lib.rs:{}:sh tests/check.sh\nexit\n",
            b64(fixed_lib())
        );
        stdin.write_all(input.as_bytes()).unwrap();
    }
    let output = child.wait_with_output().expect("dev launcher exits");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[tool] read_file"), "{stdout}");
    assert!(stdout.contains("[tool] write_file"), "{stdout}");
    assert!(stdout.contains("[tool] exec_command"), "{stdout}");
    assert!(stdout.contains("\"exit_code\":0"), "{stdout}");
    assert!(
        !fixture.worktree.exists(),
        "dev shutdown did not clean its worktree"
    );

    let server = fixture::start(&fixture, cwd.path());
    let task = server.run_with_evidence(
        "developer",
        &format!(
            "coding-fix:src/lib.rs:{}:sh tests/check.sh",
            b64(fixed_lib())
        ),
        "serve",
    );
    assert_eq!(task["state"], "succeeded", "{task}");
    assert_eq!(task["evidence"]["accepted"], true, "{task}");
    assert!(fixture.worktree.join("src/lib.rs").exists());
}
