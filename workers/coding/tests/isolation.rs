//! Containment, exclusive ownership, and cancellation cleanup.

mod fixture;

use fixture::{b64, fixed_lib};

#[test]
fn read_only_reviewer_cannot_write_or_execute() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        &root.path().join("config"),
        fixture::Options {
            instance: "coding-review".into(),
            default_agent: "reviewer".into(),
            ..Default::default()
        },
    );
    let cwd = tempfile::tempdir().unwrap();
    let server = fixture::start(&fixture, cwd.path());
    let canary = root.path().join("review-host-canary");
    let input = format!(
        "coding-fix:src/lib.rs:{}:touch {}",
        b64(fixed_lib()),
        canary.display()
    );
    let task = server.run_with_evidence("reviewer", &input, "reviewer");

    assert!(!canary.exists(), "reviewer executed a host command");
    let output = task["output"].as_str().unwrap_or_default();
    assert!(
        output.contains("Tool write_file not found") || output.contains("read_only"),
        "{output}"
    );
    let source = std::fs::read_to_string(fixture.worktree.join("src/lib.rs")).unwrap();
    assert!(
        source.contains("a - b"),
        "reviewer changed the checkout: {source}"
    );
    assert_eq!(task["evidence"]["accepted"], false, "{task}");
}

#[test]
fn write_outside_authorized_root_is_denied() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        &root.path().join("config"),
        fixture::Options {
            instance: "coding-escape".into(),
            ..Default::default()
        },
    );
    let cwd = tempfile::tempdir().unwrap();
    let server = fixture::start(&fixture, cwd.path());

    let relative = server.run_with_evidence(
        "developer",
        &format!("coding-write:../escape.txt:{}", b64("ESCAPED")),
        "relative",
    );
    assert_eq!(relative["evidence"]["accepted"], false);
    assert!(!fixture.root.join("worktrees/escape.txt").exists());
    assert!(!fixture.root.join("escape.txt").exists());

    let outside = root.path().join("absolute-escape.txt");
    let absolute = server.run_with_evidence(
        "developer",
        &format!("coding-write:{}:{}", outside.display(), b64("ESCAPED")),
        "absolute",
    );
    assert_eq!(absolute["evidence"]["accepted"], false);
    assert!(!outside.exists(), "absolute write escaped the worktree");
}

#[test]
fn sandbox_escape_attempt_cannot_touch_the_host() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        &root.path().join("config"),
        fixture::Options {
            instance: "coding-sandbox".into(),
            ..Default::default()
        },
    );
    let cwd = tempfile::tempdir().unwrap();
    let server = fixture::start(&fixture, cwd.path());
    let host_canary = root.path().join("sandbox-host-canary");
    let input = format!(
        "coding-check:sh -c 'test ! -e {} && echo isolated'",
        host_canary.display()
    );
    let task = server.run_with_evidence("developer", &input, "sandbox");
    let output = task["output"].as_str().unwrap_or_default();
    assert!(output.contains("isolated"), "{task}");
    assert!(!host_canary.exists(), "sandbox reached the host path");

    let write_attempt = server.run_with_evidence(
        "developer",
        &format!("coding-check:sh -c 'touch {}'", host_canary.display()),
        "sandbox-write",
    );
    assert!(!host_canary.exists(), "{write_attempt}");
}

#[test]
fn two_instances_edit_separate_worktrees() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    fixture::create_repo(&workspace.join("repo"));
    let first = fixture::write(
        &root.path().join("a"),
        fixture::Options {
            instance: "coding-instance-a".into(),
            workspace_root: Some(workspace.clone()),
            data_dir: Some(root.path().join("data-a")),
            ..Default::default()
        },
    );
    let second = fixture::write(
        &root.path().join("b"),
        fixture::Options {
            instance: "coding-instance-b".into(),
            workspace_root: Some(workspace.clone()),
            data_dir: Some(root.path().join("data-b")),
            ..Default::default()
        },
    );
    assert_ne!(first.worktree, second.worktree);
    let cwd = tempfile::tempdir().unwrap();
    let first_server = fixture::start(&first, cwd.path());
    let second_server = fixture::start(&second, cwd.path());

    let first_content = format!("{}// INSTANCE-A", fixed_lib());
    let second_content = format!("{}// INSTANCE-B", fixed_lib());
    let first_task = first_server.run_with_evidence(
        "developer",
        &format!(
            "coding-fix:src/lib.rs:{}:sh tests/check.sh",
            b64(&first_content)
        ),
        "a",
    );
    let second_task = second_server.run_with_evidence(
        "developer",
        &format!(
            "coding-fix:src/lib.rs:{}:sh tests/check.sh",
            b64(&second_content)
        ),
        "b",
    );
    assert_eq!(first_task["evidence"]["accepted"], true, "{first_task}");
    assert_eq!(second_task["evidence"]["accepted"], true, "{second_task}");
    assert_eq!(first_task["evidence"]["checks"][0]["target_dir"], "target");
    assert_eq!(second_task["evidence"]["checks"][0]["target_dir"], "target");
    assert_eq!(first_task["evidence"]["changed_outside_worktree"], false);
    assert_eq!(second_task["evidence"]["changed_outside_worktree"], false);

    let first_source = std::fs::read_to_string(first.worktree.join("src/lib.rs")).unwrap();
    let second_source = std::fs::read_to_string(second.worktree.join("src/lib.rs")).unwrap();
    assert!(first_source.contains("INSTANCE-A"));
    assert!(!first_source.contains("INSTANCE-B"));
    assert!(second_source.contains("INSTANCE-B"));
    assert!(!second_source.contains("INSTANCE-A"));
}

#[test]
fn second_writer_in_one_instance_is_rejected() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        &root.path().join("config"),
        fixture::Options {
            instance: "coding-busy".into(),
            ..Default::default()
        },
    );
    let cwd = tempfile::tempdir().unwrap();
    let server = fixture::start(&fixture, cwd.path());
    let input = format!("coding-fix:src/lib.rs:{}:sh -c 'sleep 2'", b64(fixed_lib()));
    let (_, first) = server.submit_status("developer", &input, "busy-first");
    let first_id = first["task_id"].as_str().unwrap().to_string();
    let (_, second) = server.submit_status(
        "developer",
        &format!("coding-write:src/lib.rs:{}", b64(fixture::broken_lib())),
        "busy-second",
    );
    let second_id = second["task_id"].as_str().unwrap().to_string();
    let second_task = server.wait_terminal(&second_id);
    assert_eq!(second_task["state"], "failed", "{second_task}");
    assert_eq!(
        second_task["error"]["code"], "coding.workspace_busy",
        "{second_task}"
    );
    let first_task = server.wait_terminal(&first_id);
    assert_eq!(first_task["state"], "succeeded", "{first_task}");
}

#[test]
fn cancellation_cleans_worktree_and_children() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        &root.path().join("config"),
        fixture::Options {
            instance: "coding-cancel".into(),
            ..Default::default()
        },
    );
    let cwd = tempfile::tempdir().unwrap();
    let server = fixture::start(&fixture, cwd.path());
    let canary = root.path().join("cancel-child-canary");
    let input = format!(
        "coding-fix:src/lib.rs:{}:sh -c 'sleep 30; touch {}'",
        b64(fixed_lib()),
        canary.display()
    );
    let (_, submitted) = server.submit_status("developer", &input, "cancel");
    let id = submitted["task_id"].as_str().unwrap().to_string();
    assert!(fixture::wait_until(
        std::time::Duration::from_secs(10),
        || { fixture.worktree.join("src/lib.rs").exists() }
    ));
    std::thread::sleep(std::time::Duration::from_millis(300));
    server.cancel(&id);
    let task = server.wait_evidence(&id);
    assert_eq!(task["state"], "cancelled", "{task}");
    assert!(
        task["evidence"]["unverified"]
            .to_string()
            .contains("checks were not accepted"),
        "{task}"
    );
    assert!(
        fixture::wait_until(std::time::Duration::from_secs(10), || {
            !fixture.worktree.exists()
        }),
        "cancelled worktree was not removed"
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(!canary.exists(), "cancelled child process kept running");
}
