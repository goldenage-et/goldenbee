//! Public `gb-instance` command surface and stable diagnostics.

use std::process::{Command, Output};

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gb-instance"))
        .args(arguments)
        .output()
        .expect("gb-instance runs")
}

#[test]
fn help_and_version_describe_the_separate_lifecycle_commands() {
    for flag in ["--help", "-h", "help"] {
        let output = run(&[flag]);
        assert_eq!(output.status.code(), Some(0));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Usage: gb-instance"), "{stdout}");
        for command in [
            "create", "start", "inspect", "stop", "profile", "trust", "upgrade", "recover",
        ] {
            assert!(stdout.contains(command), "help omits {command}: {stdout}");
        }
        assert!(output.stderr.is_empty());
    }
    for flag in ["--version", "-V"] {
        let output = run(&[flag]);
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("gb-instance {}\n", env!("CARGO_PKG_VERSION"))
        );
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn missing_or_unknown_commands_fail_without_echoing_secrets() {
    let cases: Vec<Vec<&str>> = vec![
        vec!["task", "submit"],
        vec!["--unknown"],
        vec!["--help", "extra"],
        vec!["create"],
        vec!["inspect"],
        vec!["upgrade"],
        vec!["recover"],
    ];
    for arguments in cases {
        let output = run(&arguments);
        assert_ne!(output.status.code(), Some(0), "{arguments:?}");
        assert!(String::from_utf8_lossy(&output.stdout).is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("gb-instance:"), "{stderr}");
        assert!(stderr.contains("instance."), "{stderr}");
    }

    let secret = "scaffold-secret-canary-do-not-log";
    let output = run(&["--definitely-not-an-option", secret]);
    assert_ne!(output.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
}

#[test]
fn upgrade_requires_an_explicit_verified_release_and_instance() {
    let root = tempfile::tempdir().expect("root");
    let output = run(&[
        "upgrade",
        "--root",
        root.path().to_str().unwrap(),
        "--instance",
        "worker-a",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("instance.usage"), "{stderr}");
}

#[test]
fn invalid_instance_ids_are_rejected_before_any_release_read() {
    let root = tempfile::tempdir().expect("root");
    let output = run(&[
        "create",
        "--root",
        root.path().to_str().unwrap(),
        "--release",
        root.path().join("missing-release").to_str().unwrap(),
        "--id",
        "../escape",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("instance.id_invalid"), "{stderr}");
    assert!(!root.path().join("../escape").exists());
}
