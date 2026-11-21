//! Permission modes and exact approvals through a real Worker runtime.
//!
//! These tests submit durable tasks to a started Worker and observe external
//! behavior: files on disk, task events, audit records, approval records, and
//! capability inventory. They do not call the policy engine or kernel internals.

use common::{Approval, ApprovalDecision, Limits, StreamEvent, SubmitTaskRequest, Task, TaskState};
use runtime::policy::{PermissionMode, PermissionPolicy};
use runtime::worker::{Worker, WorkerInstance};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const WRITE_TOOLS: &[&str] = &["read_file", "write_file", "apply_patch"];

fn fixture(data: &Path, tools: &[&str], permissions: &str) -> PathBuf {
    let assets = data.join("assets");
    let agents = assets.join("agents");
    let prompts = assets.join("prompts");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::write(
        agents.join("tools.toml"),
        format!(
            "name = \"tools\"\ndescription = \"permission agent\"\nmodel = \"deterministic\"\nprompt = \"tools\"\ntools = [{}]\n",
            tools
                .iter()
                .map(|tool| format!("\"{tool}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
    .expect("agent definition");
    std::fs::write(prompts.join("tools.md"), "Use the supplied tools.").expect("prompt");
    let config = data.join("worker.toml");
    std::fs::write(
        &config,
        format!(
            "instance = \"permissions-{}\"\nnamespace = \"local\"\ndefault_agent = \"tools\"\ntoken = \"dev-token\"\ntoken_scopes = [\"*\"]\n\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n\n[tools]\nworkspace = \"workspace\"\ntimeout_ms = 120000\nmax_output_bytes = 1048576\nmax_parallel = 2\n\n[permissions]\n{}",
            data.file_name()
                .expect("temp dir name")
                .to_string_lossy(),
            data.display(),
            permissions
        ),
    )
    .expect("worker config");
    config
}

fn policy_toml(mode: &str, wait_ms: u64, rules: &str) -> String {
    format!("mode = \"{mode}\"\napproval_wait_ms = {wait_ms}\napproval_ttl_ms = 60000\n{rules}")
}

async fn start(data: &Path, permissions: &str) -> WorkerInstance {
    let config = fixture(data, WRITE_TOOLS, permissions);
    Worker::new("permissions", "0.1.0")
        .file_tools()
        .start(&config)
        .await
        .expect("worker starts")
}

fn admin() -> common::Principal {
    common::Principal::new("local", "local", &["*"])
}

async fn submit(runtime: &runtime::Runtime, input: &str, session: &str) -> String {
    let request = SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: "tools".into(),
        session_id: session.into(),
        input: input.into(),
        idempotency_key: format!("{session}-{}", common::canonical_digest(&input)),
        profile: None,
        team: None,
        worker: None,
        limits: Limits::default(),
    };
    runtime
        .submit_task(&admin(), request)
        .await
        .expect("task admitted")
        .task_id
}

async fn run(runtime: &runtime::Runtime, input: &str, session: &str) -> Task {
    let task_id = submit(runtime, input, session).await;
    wait(runtime, &task_id).await
}

async fn wait(runtime: &runtime::Runtime, task_id: &str) -> Task {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let task = runtime.task_unscoped(task_id).await.expect("task exists");
        if task.state.is_terminal() {
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn events(runtime: &runtime::Runtime, task_id: &str) -> Vec<StreamEvent> {
    runtime
        .events_since(task_id, 0, 10_000)
        .await
        .expect("events")
        .events
}

async fn pending(runtime: &runtime::Runtime) -> Vec<Approval> {
    runtime.approvals(&admin(), true).await.expect("approvals")
}

async fn wait_pending(runtime: &runtime::Runtime) -> Approval {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(approval) = pending(runtime).await.into_iter().next() {
            return approval;
        }
        assert!(Instant::now() < deadline, "no approval request appeared");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_event(runtime: &runtime::Runtime, task_id: &str, kind: &str) -> StreamEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(event) = events(runtime, task_id)
            .await
            .into_iter()
            .find(|event| event.kind == kind)
        {
            return event;
        }
        assert!(Instant::now() < deadline, "event '{kind}' did not appear");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn output(task: &Task) -> String {
    task.output.clone().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_mode_produces_the_documented_allow_ask_or_deny_outcome() {
    let data = tempfile::tempdir().expect("data");
    let workspace = data.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::write(workspace.join("existing.txt"), "preexisting").expect("seed file");

    let cases = [
        // mode, wait, expected file, expected output, expected pending approval
        ("plan", 0, false, "permission.denied", false),
        ("default", 0, false, "approval.pending", true),
        ("accept-edits", 0, true, "", false),
        ("full-auto", 0, true, "", false),
    ];
    for (mode, wait, expect_file, expect_output, expect_pending) in cases {
        let instance = start(data.path(), &policy_toml(mode, wait, "")).await;
        let runtime = instance.runtime();
        let input = format!(
            "mcp:write_file {}",
            json!({"path": format!("{mode}.txt"), "content": "written"})
        );
        let task = run(&runtime, &input, &format!("mode-{mode}")).await;
        assert_eq!(task.state, TaskState::Succeeded, "{mode}: {task:?}");
        let path = workspace.join(format!("{mode}.txt"));
        assert_eq!(
            path.exists(),
            expect_file,
            "{mode}: unexpected file state; output={}",
            output(&task)
        );
        if !expect_output.is_empty() {
            assert!(
                output(&task).contains(expect_output),
                "{mode}: {}",
                output(&task)
            );
        }
        let events = events(&runtime, &task.id).await;
        let decision = events
            .iter()
            .find(|event| event.kind == "permission.decision")
            .expect("permission decision event");
        assert_eq!(decision.payload["mode"], mode);
        assert_eq!(decision.payload["tool"], "write_file");
        let approvals: Vec<_> = pending(&runtime)
            .await
            .into_iter()
            .filter(|approval| approval.task_id == task.id)
            .collect();
        assert_eq!(
            !approvals.is_empty(),
            expect_pending,
            "{mode}: unexpected approvals {approvals:?}"
        );
        // Reads remain available in every mode.
        let read = run(
            &runtime,
            "mcp:read_file {\"path\":\"existing.txt\"}",
            &format!("read-{mode}"),
        )
        .await;
        assert!(output(&read).contains("preexisting"), "{mode}: read denied");
        instance.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_binds_the_exact_action_and_authorizes_only_that_retry() {
    let data = tempfile::tempdir().expect("data");
    let instance = start(data.path(), &policy_toml("default", 10_000, "")).await;
    let runtime = instance.runtime();
    let task_id = submit(
        &runtime,
        "tool-loop:2 write_file {\"path\":\"notes/a.txt\",\"content\":\"approved\"}",
        "approval",
    )
    .await;
    let approval = wait_pending(&runtime).await;
    assert_eq!(approval.operation, "write_file");
    assert_eq!(approval.mode.as_deref(), Some("default"));
    assert_eq!(approval.requested_by, "local");
    let action = approval.action.as_ref().expect("exact permission action");
    assert_eq!(action.tool, "write_file");
    assert_eq!(action.paths, vec!["notes/a.txt".to_string()]);
    assert_eq!(action.sandbox_profile, "workspace-write");
    assert_eq!(action.principal, "local");
    assert!(!action.arguments_digest.is_empty());
    assert!(
        approval.target.contains("notes/a.txt"),
        "{}",
        approval.target
    );

    let task = runtime.task_unscoped(&task_id).await.expect("task");
    assert_eq!(action.profile_revision, task.profile_revision);
    let requested = wait_event(&runtime, &task_id, "approval.requested").await;
    assert_eq!(
        requested.payload["action"],
        serde_json::to_value(action).unwrap()
    );

    let decided = runtime
        .decide_approval(
            &admin(),
            &approval.id,
            ApprovalDecision {
                decision: "approved".into(),
                digest: approval.digest.clone(),
                answer: None,
            },
        )
        .await
        .expect("approval decided");
    assert_eq!(decided.status, "approved");

    let task = wait(&runtime, &task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert_eq!(
        std::fs::read_to_string(data.path().join("workspace/notes/a.txt")).unwrap(),
        "approved"
    );
    let events = events(&runtime, &task.id).await;
    let decisions: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "permission.decision")
        .collect();
    assert_eq!(decisions.len(), 2, "both loop calls must be decided");
    for decision in decisions {
        assert_eq!(decision.payload["evaluated"], "ask");
        assert_eq!(decision.payload["decision"], "allow");
        assert_eq!(decision.payload["approval"]["status"], "approved");
    }
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_decision_produces_a_truthful_pending_outcome_without_executing() {
    let data = tempfile::tempdir().expect("data");
    let instance = start(data.path(), &policy_toml("default", 0, "")).await;
    let runtime = instance.runtime();
    let task = run(
        &runtime,
        "call:write_file {\"path\":\"pending.txt\",\"content\":\"not-written\"}",
        "pending",
    )
    .await;
    assert!(
        output(&task).contains("approval.pending"),
        "{}",
        output(&task)
    );
    assert!(!data.path().join("workspace/pending.txt").exists());
    let events = events(&runtime, &task.id).await;
    assert!(events.iter().any(|event| event.kind == "approval.pending"));
    let decision = events
        .iter()
        .find(|event| event.kind == "permission.decision")
        .expect("permission decision");
    assert_eq!(decision.payload["decision"], "ask");
    assert_eq!(decision.payload["evaluated"], "ask");
    let approval = pending(&runtime).await.into_iter().next().expect("pending");
    assert_eq!(approval.action.as_ref().unwrap().tool, "write_file");
    // The pending decision remains a durable decision point after the turn.
    let decided = runtime
        .decide_approval(
            &admin(),
            &approval.id,
            ApprovalDecision {
                decision: "approved".into(),
                digest: approval.digest.clone(),
                answer: None,
            },
        )
        .await
        .expect("late decision is recorded");
    assert_eq!(decided.status, "approved");
    assert!(!data.path().join("workspace/pending.txt").exists());
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deny_rule_blocks_execution_and_writes_a_redacted_audit_entry() {
    let data = tempfile::tempdir().expect("data");
    let rules = "\n[[permissions.rules]]\nid = \"deny-notes\"\ndecision = \"deny\"\ntools = [\"write_file\"]\npaths = [\"notes/*\"]\n";
    let instance = start(data.path(), &policy_toml("full-auto", 0, rules)).await;
    let runtime = instance.runtime();
    let task = run(
        &runtime,
        "mcp:write_file {\"path\":\"notes/secret.txt\",\"content\":\"SUPER-SECRET-CONTENT\"}",
        "deny",
    )
    .await;
    assert!(
        output(&task).contains("permission.denied"),
        "{}",
        output(&task)
    );
    assert!(!data.path().join("workspace/notes/secret.txt").exists());
    let events = events(&runtime, &task.id).await;
    let decision = events
        .iter()
        .find(|event| event.kind == "permission.decision")
        .expect("permission decision event");
    assert_eq!(decision.payload["decision"], "deny");
    assert_eq!(decision.payload["rule"], "deny-notes");
    assert_eq!(decision.payload["mode"], "full-auto");
    let serialized = serde_json::to_string(&events).unwrap();
    assert!(!serialized.contains("SUPER-SECRET-CONTENT"));

    let audit = runtime.audit_records(&task.id).await;
    let entry = audit
        .iter()
        .find(|record| record.action == "permission.decision")
        .expect("permission audit entry");
    assert_eq!(entry.outcome, "deny");
    assert!(entry.detail.contains("rule=deny-notes"), "{}", entry.detail);
    assert!(entry.detail.contains("mode=full-auto"));
    assert!(entry.detail.contains("decision=deny"));
    assert!(!entry.detail.contains("SUPER-SECRET-CONTENT"));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_rule_is_scoped_to_the_matching_path() {
    let data = tempfile::tempdir().expect("data");
    let rules = "\n[[permissions.rules]]\nid = \"allow-notes\"\ndecision = \"allow\"\ntools = [\"write_file\"]\npaths = [\"notes/*\"]\n";
    let instance = start(data.path(), &policy_toml("default", 0, rules)).await;
    let runtime = instance.runtime();
    let allowed = run(
        &runtime,
        "mcp:write_file {\"path\":\"notes/a.txt\",\"content\":\"allowed\"}",
        "allow",
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(data.path().join("workspace/notes/a.txt")).unwrap(),
        "allowed"
    );
    let decision = events(&runtime, &allowed.id)
        .await
        .into_iter()
        .find(|event| event.kind == "permission.decision")
        .expect("decision");
    assert_eq!(decision.payload["decision"], "allow");
    assert_eq!(decision.payload["rule"], "allow-notes");
    assert_eq!(decision.payload["evaluated"], "allow");

    let other = run(
        &runtime,
        "mcp:write_file {\"path\":\"secrets.txt\",\"content\":\"no\"}",
        "allow",
    )
    .await;
    assert!(
        output(&other).contains("approval.pending"),
        "{}",
        output(&other)
    );
    assert!(!data.path().join("workspace/secrets.txt").exists());
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_changes_apply_to_later_calls_and_are_recorded_in_evidence() {
    let data = tempfile::tempdir().expect("data");
    let instance = start(data.path(), &policy_toml("plan", 0, "")).await;
    let runtime = instance.runtime();
    let caller = common::Principal::new("caller", "local", &["task:submit"]);
    let denied = runtime
        .set_permission_policy(
            &caller,
            PermissionPolicy {
                mode: PermissionMode::FullAuto,
                ..PermissionPolicy::default()
            },
        )
        .await
        .expect_err("a caller cannot change the instance policy");
    assert_eq!(denied.code, "scope.denied");

    let denied = run(
        &runtime,
        "mcp:write_file {\"path\":\"before.txt\",\"content\":\"denied\"}",
        "change",
    )
    .await;
    assert!(output(&denied).contains("permission.denied"));
    let before = events(&runtime, &denied.id)
        .await
        .into_iter()
        .find(|event| event.kind == "permission.decision")
        .expect("decision");
    assert_eq!(before.payload["policy"]["revision"], 1);
    assert_eq!(before.payload["mode"], "plan");

    let snapshot = runtime
        .set_permission_policy(
            &admin(),
            PermissionPolicy {
                mode: PermissionMode::FullAuto,
                ..PermissionPolicy::default()
            },
        )
        .await
        .expect("policy updated");
    assert_eq!(snapshot.revision, 2);

    let allowed = run(
        &runtime,
        "mcp:write_file {\"path\":\"after.txt\",\"content\":\"allowed\"}",
        "change",
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(data.path().join("workspace/after.txt")).unwrap(),
        "allowed"
    );
    let after = events(&runtime, &allowed.id)
        .await
        .into_iter()
        .find(|event| event.kind == "permission.decision")
        .expect("decision");
    assert_eq!(after.payload["policy"]["revision"], 2);
    assert_eq!(after.payload["mode"], "full-auto");
    // Earlier evidence keeps the policy that actually decided it.
    let unchanged = events(&runtime, &denied.id)
        .await
        .into_iter()
        .find(|event| event.kind == "permission.decision")
        .expect("decision");
    assert_eq!(unchanged.payload["policy"]["revision"], 1);

    let audit = runtime.audit_records("policy-2").await;
    assert!(audit
        .iter()
        .any(|record| record.action == "permission.policy" && record.outcome == "applied"));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_approvals_survive_restart_and_a_changed_action_needs_a_new_decision() {
    let data = tempfile::tempdir().expect("data");
    let permissions = policy_toml("default", 0, "");
    let instance = start(data.path(), &permissions).await;
    let runtime = instance.runtime();
    let first = run(
        &runtime,
        "mcp:write_file {\"path\":\"first.txt\",\"content\":\"one\"}",
        "restart",
    )
    .await;
    let approval = pending(&runtime).await.into_iter().next().expect("pending");
    let digest = approval.digest.clone();
    let action = approval.action.clone().expect("action");
    instance.shutdown().await;

    let instance = start(data.path(), &permissions).await;
    let runtime = instance.runtime();
    let recovered = pending(&runtime)
        .await
        .into_iter()
        .find(|candidate| candidate.id == approval.id)
        .expect("pending approval survived restart");
    assert_eq!(recovered.digest, digest);
    assert_eq!(recovered.action, Some(action.clone()));
    let decided = runtime
        .decide_approval(
            &admin(),
            &recovered.id,
            ApprovalDecision {
                decision: "approved".into(),
                digest: recovered.digest.clone(),
                answer: None,
            },
        )
        .await
        .expect("decision after restart");
    assert_eq!(decided.status, "approved");

    // A changed action has a different digest. The recorded approval for the
    // original action cannot authorize it, so a new pending request appears.
    let changed = run(
        &runtime,
        "mcp:write_file {\"path\":\"changed.txt\",\"content\":\"two\"}",
        "restart",
    )
    .await;
    assert!(
        output(&changed).contains("approval.pending"),
        "{}",
        output(&changed)
    );
    assert!(!data.path().join("workspace/changed.txt").exists());
    let pending_now = pending(&runtime).await;
    assert_eq!(pending_now.len(), 1);
    assert_ne!(pending_now[0].digest, digest);
    assert_ne!(pending_now[0].action.as_ref().unwrap().paths, action.paths);
    let first_events = events(&runtime, &first.id).await;
    assert!(first_events
        .iter()
        .any(|event| event.kind == "permission.decision"));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_policy_configuration_fails_before_the_worker_serves() {
    let data = tempfile::tempdir().expect("data");
    let rules = "\n[[permissions.rules]]\nid = \"\"\ndecision = \"allow\"\n";
    let config = fixture(data.path(), WRITE_TOOLS, &policy_toml("default", 0, rules));
    let error = match Worker::new("permissions", "0.1.0")
        .file_tools()
        .start(&config)
        .await
    {
        Ok(_) => panic!("invalid policy is rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, "config.permissions");
    assert!(!error.message.contains("allow"), "{}", error.message);
}
