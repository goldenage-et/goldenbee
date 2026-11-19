//! Crash and reconciliation evidence for protected external effects.
//!
//! These tests drive the real fixture Worker over public `/v1` routes. The
//! fixture's external ledger counts actual effects; assertions never trust a
//! reported task status alone.

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TOOL: &str = "ledger_write";

struct Worker {
    child: Child,
    data: tempfile::TempDir,
    config: PathBuf,
    base: String,
    env: Vec<(String, String)>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn config_for_with(data: &Path, instance: &str, max_active: Option<usize>) -> PathBuf {
    let agents = data.join("assets/agents");
    let prompts = data.join("assets/prompts");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::write(
        agents.join("effect.toml"),
        format!(
            "name = \"effect\"\ndescription = \"Protected effect agent\"\nmodel = \"deterministic\"\nprompt = \"effect\"\ntools = [\"{TOOL}\"]\n"
        ),
    )
    .expect("agent definition");
    std::fs::write(
        prompts.join("effect.md"),
        "Write the requested ledger entry.",
    )
    .expect("prompt");
    std::fs::create_dir_all(data.join("state")).expect("state directory");
    std::fs::create_dir_all(data.join("external")).expect("external directory");
    std::fs::create_dir_all(data.join("faults")).expect("fault directory");
    let capacity = max_active
        .map(|value| format!("max_active = {value}\n"))
        .unwrap_or_default();
    let text = format!(
        "instance = \"{instance}\"\nnamespace = \"local\"\ndefault_agent = \"effect\"\ntoken = \"dev-token\"\ntoken_scopes = [\"*\"]\n{capacity}\n[[tokens]]\ntoken = \"other\"\nnamespace = \"local\"\nprincipal = \"other\"\nscopes = [\"task:read\"]\n\n[[tokens]]\ntoken = \"approver\"\nnamespace = \"local\"\nprincipal = \"approver\"\nscopes = [\"approval:read\", \"approval:decide\", \"task:read\"]\n\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n",
        data.join("state").display()
    );
    let path = data.join("worker.toml");
    std::fs::write(&path, text).expect("worker config");
    path
}

#[allow(clippy::zombie_processes)]
fn spawn_worker(config: &Path, env: &[(String, String)]) -> (Child, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_effect-fixture"));
    command
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("effect fixture serves");
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("listener address is announced");
    // Keep the read end open for the process lifetime. The shared runtime can
    // write diagnostics after startup, and a closed pipe would terminate it.
    std::thread::spawn(move || {
        let mut ignored = String::new();
        while reader.read_line(&mut ignored).is_ok_and(|read| read > 0) {
            ignored.clear();
        }
    });
    let base = line
        .split("listening on ")
        .nth(1)
        .expect("address announcement")
        .trim()
        .to_string();
    assert!(base.starts_with("http://127.0.0.1:"), "{line}");
    // The listener announces its bound address before the accept loop is
    // guaranteed to serve; wait for the public liveness route.
    let client = reqwest::blocking::Client::new();
    let ready = Instant::now();
    while ready.elapsed() < Duration::from_secs(5) {
        if client
            .get(format!("{base}/v1/health"))
            .send()
            .is_ok_and(|response| response.status().is_success())
        {
            return (child, base);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("worker did not become ready at {base}");
}

impl Worker {
    fn start(options: &[(&str, &str)]) -> Self {
        Self::start_with_capacity(None, options)
    }

    fn start_with_capacity(max_active: Option<usize>, options: &[(&str, &str)]) -> Self {
        let data = tempfile::tempdir().expect("temporary data directory");
        let instance = format!(
            "effect-{}",
            data.path().file_name().unwrap().to_string_lossy()
        );
        let config = config_for_with(data.path(), &instance, max_active);
        let mut env = vec![
            (
                "GOLDENBEE_EFFECT_DIR".to_string(),
                data.path().join("external").display().to_string(),
            ),
            (
                "GOLDENBEE_FAULT_DIR".to_string(),
                data.path().join("faults").display().to_string(),
            ),
        ];
        for (key, value) in options {
            env.push(((*key).to_string(), (*value).to_string()));
        }
        let (child, base) = spawn_worker(&config, &env);
        Self {
            child,
            data,
            config,
            base,
            env,
        }
    }

    fn set_env(&mut self, key: &str, value: &str) {
        self.env.retain(|(existing, _)| existing != key);
        self.env.push((key.to_string(), value.to_string()));
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (child, base) = spawn_worker(&self.config, &self.env);
        self.child = child;
        self.base = base;
    }

    fn wait_for_fault_exit(&mut self) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(15) {
            match self.child.try_wait().expect("worker status is observable") {
                Some(status) => {
                    assert_eq!(
                        status.code(),
                        Some(75),
                        "the injected boundary must terminate the process: {status:?}"
                    );
                    return;
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("the injected boundary did not terminate the worker");
    }

    fn install_fault(&self, marker: &str) {
        std::fs::write(self.data.path().join("faults").join(marker), "").expect("fault marker");
    }

    fn submit(&self, key: &str, entry: &str) -> String {
        self.submit_raw(
            "session-1",
            &format!("call:{TOOL} {{\"entry\":\"{entry}\"}}"),
            key,
        )
    }

    fn submit_raw(&self, session: &str, input: &str, key: &str) -> String {
        let response = reqwest::blocking::Client::new()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth("dev-token")
            .json(&serde_json::json!({
                "agent": "effect",
                "session_id": session,
                "input": input,
                "idempotency_key": key,
            }))
            .send()
            .expect("submit request is attempted");
        let status = response.status();
        let body = response.text().expect("submit body");
        assert!(status.is_success(), "submit failed: {status} {body}");
        let value: Value = serde_json::from_str(&body).expect("submit JSON");
        value["task_id"].as_str().expect("task id").to_string()
    }

    fn task(&self, id: &str) -> Value {
        let response = reqwest::blocking::Client::new()
            .get(format!("{}/v1/tasks/{id}", self.base))
            .bearer_auth("dev-token")
            .send()
            .expect("task request is attempted");
        let status = response.status();
        let body = response.text().expect("task body");
        assert!(status.is_success(), "task read failed: {status} {body}");
        serde_json::from_str(&body).expect("task JSON")
    }

    fn operations(&self, id: &str) -> Value {
        let response = self.operations_with_token("dev-token", id);
        let status = response.status();
        let body = response.text().expect("operations body");
        assert!(
            status.is_success(),
            "operations read failed: {status} {body}"
        );
        serde_json::from_str(&body).expect("operations JSON")
    }

    fn operations_with_token(&self, token: &str, id: &str) -> reqwest::blocking::Response {
        reqwest::blocking::Client::new()
            .get(format!("{}/v1/tasks/{id}/operations", self.base))
            .bearer_auth(token)
            .send()
            .expect("operations request is attempted")
    }

    fn events(&self, id: &str) -> Value {
        let response = reqwest::blocking::Client::new()
            .get(format!("{}/v1/tasks/{id}/events?after=0", self.base))
            .bearer_auth("dev-token")
            .send()
            .expect("events request is attempted");
        let body = response.text().expect("events body");
        serde_json::from_str(&body).expect("events JSON")
    }

    fn wait_state(&self, id: &str, wanted: &[&str]) -> Value {
        let started = Instant::now();
        loop {
            let task = self.task(id);
            let state = task["state"].as_str().unwrap_or_default();
            if wanted.contains(&state) {
                return task;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "task stayed in {state}: {task}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_operation_status(&self, id: &str, status: &str) -> Value {
        let started = Instant::now();
        loop {
            let value = self.operations(id);
            if value["operations"].as_array().is_some_and(|operations| {
                operations
                    .iter()
                    .any(|operation| operation["status"] == status)
            }) {
                return value;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "operation never reached {status}: {value}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn effect_count(&self) -> usize {
        std::fs::read_to_string(self.data.path().join("external/effects.log"))
            .map(|value| value.lines().filter(|line| !line.trim().is_empty()).count())
            .unwrap_or(0)
    }

    fn approvals_with_token(&self, token: &str, pending_only: bool) -> reqwest::blocking::Response {
        reqwest::blocking::Client::new()
            .get(format!("{}/v1/approvals?pending={pending_only}", self.base))
            .bearer_auth(token)
            .send()
            .expect("approval list request is attempted")
    }

    fn approvals(&self) -> Value {
        let response = self.approvals_with_token("dev-token", true);
        let status = response.status();
        let body = response.text().expect("approval list body");
        assert!(status.is_success(), "approval list failed: {status} {body}");
        serde_json::from_str(&body).expect("approval list JSON")
    }

    fn approval_for(&self, task_id: &str) -> Value {
        let started = Instant::now();
        loop {
            let approvals = self.approvals();
            if let Some(approval) = approvals.as_array().and_then(|values| {
                values
                    .iter()
                    .find(|approval| approval["task_id"] == task_id)
                    .cloned()
            }) {
                return approval;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "task {task_id} never exposed a pending approval: {approvals}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn approval_by_id(&self, approval_id: &str) -> Value {
        let response = reqwest::blocking::Client::new()
            .get(format!("{}/v1/approvals/{approval_id}", self.base))
            .bearer_auth("dev-token")
            .send()
            .expect("approval request is attempted");
        let status = response.status();
        let body = response.text().expect("approval body");
        assert!(status.is_success(), "approval read failed: {status} {body}");
        serde_json::from_str(&body).expect("approval JSON")
    }

    fn decide(
        &self,
        token: &str,
        approval_id: &str,
        decision: &str,
        digest: &str,
    ) -> reqwest::blocking::Response {
        reqwest::blocking::Client::new()
            .post(format!("{}/v1/approvals/{approval_id}/decision", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({"decision": decision, "digest": digest}))
            .send()
            .expect("approval decision is attempted")
    }

    fn revoke(&self, token: &str, approval_id: &str) -> reqwest::blocking::Response {
        reqwest::blocking::Client::new()
            .post(format!("{}/v1/approvals/{approval_id}/revoke", self.base))
            .bearer_auth(token)
            .send()
            .expect("approval revocation is attempted")
    }
}

fn is_state(value: &Value, wanted: &str) -> bool {
    value["state"].as_str() == Some(wanted)
}

#[test]
fn confirmed_effect_persists_identity_before_dispatch_and_receipt_after_response() {
    let worker = Worker::start(&[]);
    let id = worker.submit("normal-1", "one");
    let task = worker.wait_state(&id, &["succeeded"]);
    assert_eq!(task["attempt"], 1);
    let operations = worker.wait_operation_status(&id, "committed");
    assert_eq!(operations["task_id"], id);
    assert_eq!(operations["attempt"], 1);
    assert_eq!(
        operations["native_invocation_id"],
        task["native_invocation_id"]
    );
    let operation = &operations["operations"][0];
    assert_eq!(operation["tool"], TOOL);
    assert_eq!(operation["attempt"], 1);
    assert!(operation["invocation_id"].as_str().is_some());
    assert!(operation["external_ref"].as_str().is_some());
    assert_eq!(operation["receipt"], "accepted");
    assert_eq!(worker.effect_count(), 1);

    let events = worker.events(&id);
    let kinds: Vec<_> = events["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"operation.reserved"), "{kinds:?}");
    assert!(kinds.contains(&"operation.committed"), "{kinds:?}");
    assert!(is_state(&task, "succeeded"));
}

#[test]
fn crash_after_commitment_reconciles_to_one_effect_without_redispatch() {
    let mut worker = Worker::start(&[]);
    worker.install_fault("after-effect-dispatch");
    let id = worker.submit("crash-commit", "two");
    worker.wait_for_fault_exit();
    assert_eq!(worker.effect_count(), 1, "the external effect committed");

    worker.restart();
    let operations = worker.wait_operation_status(&id, "committed");
    assert_eq!(operations["operations"][0]["status"], "committed");
    assert_eq!(
        worker.effect_count(),
        1,
        "reconciliation preserves one effect"
    );
    let task = worker.task(&id);
    assert!(
        is_state(&task, "recovering"),
        "the native outcome stays explicitly uncertain: {task}"
    );

    // A restart is idempotent: a committed receipt is never dispatched again.
    worker.restart();
    let operations = worker.operations(&id);
    assert_eq!(operations["operations"][0]["status"], "committed");
    assert_eq!(worker.effect_count(), 1);
}

#[test]
fn crash_before_dispatch_reconciles_absent_and_does_not_retry_automatically() {
    let mut worker = Worker::start(&[]);
    worker.install_fault("after-effect-reservation");
    let id = worker.submit("crash-reserve", "three");
    worker.wait_for_fault_exit();
    assert_eq!(worker.effect_count(), 0);

    worker.restart();
    let operations = worker.wait_operation_status(&id, "absent");
    assert_eq!(operations["operations"][0]["status"], "absent");
    assert_eq!(worker.effect_count(), 0, "no automatic retry after restart");
    let task = worker.task(&id);
    assert!(
        is_state(&task, "recovering"),
        "a known-absent effect does not claim task success: {task}"
    );
}

#[test]
fn crash_after_receipt_keeps_the_committed_receipt_without_duplicate_effect() {
    let mut worker = Worker::start(&[]);
    worker.install_fault("after-effect-receipt");
    let id = worker.submit("crash-receipt", "four");
    worker.wait_for_fault_exit();
    assert_eq!(worker.effect_count(), 1);

    worker.restart();
    let operations = worker.operations(&id);
    assert_eq!(operations["operations"][0]["status"], "committed");
    assert_eq!(worker.effect_count(), 1);
    let task = worker.task(&id);
    assert!(is_state(&task, "recovering"), "{task}");
}

#[test]
fn unavailable_status_interface_leaves_the_task_unknown_and_never_retries() {
    let mut worker = Worker::start(&[]);
    worker.install_fault("after-effect-dispatch");
    let id = worker.submit("unknown-status", "five");
    worker.wait_for_fault_exit();
    assert_eq!(worker.effect_count(), 1);

    worker.set_env("GOLDENBEE_EFFECT_STATUS", "unavailable");
    worker.restart();
    let operations = worker.wait_operation_status(&id, "unknown");
    assert_eq!(operations["operations"][0]["status"], "unknown");
    assert_eq!(
        worker.effect_count(),
        1,
        "unknown is not permission to retry"
    );
    let task = worker.wait_state(&id, &["unknown"]);
    assert!(is_state(&task, "unknown"));
    assert!(task["uncertainty"].as_str().is_some());
}

#[test]
fn known_absent_or_failed_effect_retries_only_under_documented_policy_and_task_identity() {
    let worker = Worker::start(&[
        ("GOLDENBEE_EFFECT_BEHAVIOR", "fail-once"),
        ("GOLDENBEE_EFFECT_RETRY", "1"),
    ]);
    let id = worker.submit("retry-1", "six");
    let task = worker.wait_state(&id, &["succeeded"]);
    assert_eq!(task["attempt"], 1, "the task identity is preserved");
    let operations = worker.wait_operation_status(&id, "committed");
    let operation = &operations["operations"][0];
    assert_eq!(
        operation["attempt"], 2,
        "the operation records both attempts"
    );
    assert_eq!(operation["task_id"], id);
    assert!(operation["policy"]
        .as_str()
        .unwrap_or_default()
        .contains("max_attempts=2"));
    assert_eq!(
        worker.effect_count(),
        1,
        "a known-failed attempt applied nothing"
    );

    let events = worker.events(&id);
    let kinds: Vec<_> = events["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"operation.retry"), "{kinds:?}");
}

#[test]
fn known_failed_effect_without_policy_authorization_is_not_retried() {
    let worker = Worker::start(&[("GOLDENBEE_EFFECT_BEHAVIOR", "fail-once")]);
    let id = worker.submit("no-retry-1", "seven");
    let task = worker.wait_state(&id, &["failed"]);
    assert_eq!(task["error"]["code"], "effect.failed");
    let operations = worker.operations(&id);
    let operation = &operations["operations"][0];
    assert_eq!(operation["status"], "failed");
    assert_eq!(operation["attempt"], 1);
    assert_eq!(worker.effect_count(), 0);
    assert!(operation["policy"]
        .as_str()
        .unwrap_or_default()
        .contains("max_attempts=1"));
}

#[test]
fn operation_receipts_are_scoped_to_the_owning_task_and_caller() {
    let worker = Worker::start(&[]);
    let id = worker.submit("scope-1", "nine");
    worker.wait_state(&id, &["succeeded"]);
    assert_eq!(
        worker.operations(&id)["operations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let denied = worker.operations_with_token("other", &id);
    assert_eq!(denied.status(), 403);
    let body = denied.text().expect("denied body");
    assert!(!body.contains("external_ref"), "{body}");
}

#[test]
fn protected_effect_pauses_and_exposes_its_exact_operation_to_an_authorized_approver() {
    let worker = Worker::start(&[("GOLDENBEE_EFFECT_APPROVAL", "1")]);
    let id = worker.submit("approval-pause", "approval-target");
    let task = worker.wait_state(&id, &["waiting_approval"]);
    assert_eq!(worker.effect_count(), 0, "approval must precede the effect");

    let approval = worker.approval_for(&id);
    assert_eq!(approval["task_id"], id);
    assert_eq!(approval["operation"], TOOL);
    assert_eq!(approval["target"], "approval-target");
    assert_eq!(approval["risk"], "external-write");
    assert_eq!(approval["status"], "pending");
    assert!(
        approval["digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "{approval}"
    );
    assert!(
        approval["expires_at"].as_i64().unwrap_or_default()
            > approval["created_at"].as_i64().unwrap_or_default(),
        "{approval}"
    );

    // The request is visible only to an approver with the read grant.
    let denied = worker.approvals_with_token("other", true);
    assert_eq!(denied.status(), 403);
    assert!(is_state(&task, "waiting_approval"));
    assert_eq!(worker.effect_count(), 0);
}

#[test]
fn approval_of_the_unchanged_operation_resumes_one_effect_with_receipt_and_audit() {
    let worker = Worker::start(&[("GOLDENBEE_EFFECT_APPROVAL", "1")]);
    let id = worker.submit("approval-resume", "approved-target");
    worker.wait_state(&id, &["waiting_approval"]);
    let approval = worker.approval_for(&id);
    let approval_id = approval["id"].as_str().expect("approval id").to_string();
    let digest = approval["digest"].as_str().expect("digest").to_string();
    assert_eq!(worker.effect_count(), 0);

    let response = worker.decide("dev-token", &approval_id, "approved", &digest);
    assert_eq!(response.status(), 200);
    let task = worker.wait_state(&id, &["succeeded"]);
    assert!(is_state(&task, "succeeded"));
    assert_eq!(worker.effect_count(), 1);

    let recorded = worker.approval_by_id(&approval_id);
    assert_eq!(recorded["status"], "approved");
    assert_eq!(recorded["decided_by"], "local");
    assert_eq!(recorded["digest"], digest);
    assert!(recorded["receipt"].as_str().is_some(), "{recorded}");

    let events = worker.events(&id).to_string();
    assert!(events.contains("approval.approved"), "{events}");
    assert!(events.contains("approval.receipt"), "{events}");
    assert!(events.contains("operation.committed"), "{events}");

    // The same decision is a replay, not a second effect or duplicate audit.
    let repeated = worker.decide("dev-token", &approval_id, "approved", &digest);
    assert_eq!(repeated.status(), 200);
    let events = worker.events(&id).to_string();
    assert_eq!(
        events.matches("\"kind\":\"approval.approved\"").count(),
        1,
        "{events}"
    );
    assert_eq!(worker.effect_count(), 1);

    // A conflicting decision cannot replace the recorded authorization.
    let conflict = worker.decide("dev-token", &approval_id, "rejected", &digest);
    assert_eq!(conflict.status(), 409);
    let body = conflict.text().expect("conflict body");
    assert!(body.contains("approval.conflict"), "{body}");
    assert_eq!(worker.effect_count(), 1);
}

#[test]
fn rejection_blocks_the_effect_and_keeps_the_recorded_outcome() {
    let worker = Worker::start(&[("GOLDENBEE_EFFECT_APPROVAL", "1")]);
    let id = worker.submit("approval-reject", "rejected-target");
    worker.wait_state(&id, &["waiting_approval"]);
    let approval = worker.approval_for(&id);
    let approval_id = approval["id"].as_str().expect("approval id").to_string();
    let digest = approval["digest"].as_str().expect("digest").to_string();

    let response = worker.decide("dev-token", &approval_id, "rejected", &digest);
    assert_eq!(response.status(), 200);
    worker.wait_state(&id, &["succeeded", "failed"]);
    assert_eq!(worker.effect_count(), 0, "rejection must block the effect");
    assert_eq!(worker.approval_by_id(&approval_id)["status"], "rejected");

    let repeated = worker.decide("dev-token", &approval_id, "rejected", &digest);
    assert_eq!(repeated.status(), 200);
    let conflict = worker.decide("dev-token", &approval_id, "approved", &digest);
    assert_eq!(conflict.status(), 409);
    let body = conflict.text().expect("conflict body");
    assert!(body.contains("approval.conflict"), "{body}");
    assert_eq!(worker.effect_count(), 0);
}

#[test]
fn expiry_revocation_and_wrong_approver_block_the_effect() {
    let worker = Worker::start(&[
        ("GOLDENBEE_EFFECT_APPROVAL", "1"),
        ("GOLDENBEE_EFFECT_APPROVAL_TTL_MS", "150"),
    ]);
    let id = worker.submit("approval-expire", "expired-target");
    worker.wait_state(&id, &["waiting_approval"]);
    let approval = worker.approval_for(&id);
    let approval_id = approval["id"].as_str().expect("approval id").to_string();
    let digest = approval["digest"].as_str().expect("digest").to_string();
    std::thread::sleep(Duration::from_millis(250));
    let expired = worker.decide("dev-token", &approval_id, "approved", &digest);
    assert_eq!(expired.status(), 409);
    let body = expired.text().expect("expired body");
    assert!(body.contains("approval.expired"), "{body}");
    worker.wait_state(&id, &["succeeded", "failed"]);
    assert_eq!(worker.effect_count(), 0);

    let worker = Worker::start(&[("GOLDENBEE_EFFECT_APPROVAL", "1")]);
    let id = worker.submit("approval-revoke", "revoked-target");
    worker.wait_state(&id, &["waiting_approval"]);
    let approval = worker.approval_for(&id);
    let approval_id = approval["id"].as_str().expect("approval id").to_string();
    let digest = approval["digest"].as_str().expect("digest").to_string();
    let revoked = worker.revoke("dev-token", &approval_id);
    assert_eq!(revoked.status(), 200);
    assert_eq!(worker.approval_by_id(&approval_id)["status"], "revoked");
    let denied = worker.decide("dev-token", &approval_id, "approved", &digest);
    assert_eq!(denied.status(), 409);
    let body = denied.text().expect("revoked decision body");
    assert!(body.contains("approval.revoked"), "{body}");
    worker.wait_state(&id, &["succeeded", "failed"]);
    assert_eq!(worker.effect_count(), 0);

    let worker = Worker::start(&[("GOLDENBEE_EFFECT_APPROVAL", "1")]);
    let id = worker.submit("approval-wrong", "wrong-target");
    worker.wait_state(&id, &["waiting_approval"]);
    let approval = worker.approval_for(&id);
    let approval_id = approval["id"].as_str().expect("approval id").to_string();
    let digest = approval["digest"].as_str().expect("digest").to_string();
    let wrong = worker.decide("approver", &approval_id, "approved", &digest);
    assert_eq!(wrong.status(), 403);
    assert_eq!(
        worker.approval_by_id(&approval_id)["status"],
        "pending",
        "a foreign approver cannot consume the request"
    );
    let right = worker.decide("dev-token", &approval_id, "approved", &digest);
    assert_eq!(right.status(), 200);
    worker.wait_state(&id, &["succeeded"]);
    assert_eq!(worker.effect_count(), 1);
}

#[test]
fn a_changed_argument_digest_cannot_authorize_the_operation() {
    let worker = Worker::start(&[("GOLDENBEE_EFFECT_APPROVAL", "1")]);
    let id = worker.submit("approval-changed", "original-target");
    worker.wait_state(&id, &["waiting_approval"]);
    let approval = worker.approval_for(&id);
    let approval_id = approval["id"].as_str().expect("approval id").to_string();
    let digest = approval["digest"].as_str().expect("digest").to_string();

    let changed = worker.decide(
        "dev-token",
        &approval_id,
        "approved",
        "sha256:changed-arguments",
    );
    assert_eq!(changed.status(), 409);
    let body = changed.text().expect("changed body");
    assert!(body.contains("approval.digest_mismatch"), "{body}");
    assert_eq!(worker.effect_count(), 0);
    assert_eq!(worker.approval_by_id(&approval_id)["status"], "pending");

    let exact = worker.decide("dev-token", &approval_id, "approved", &digest);
    assert_eq!(exact.status(), 200);
    worker.wait_state(&id, &["succeeded"]);
    assert_eq!(worker.effect_count(), 1);
}

#[test]
fn crash_after_an_approved_effect_commit_preserves_one_effect() {
    let mut worker = Worker::start(&[("GOLDENBEE_EFFECT_APPROVAL", "1")]);
    let id = worker.submit("approval-effect-crash", "committed-target");
    worker.wait_state(&id, &["waiting_approval"]);
    let approval = worker.approval_for(&id);
    let approval_id = approval["id"].as_str().expect("approval id").to_string();
    let digest = approval["digest"].as_str().expect("digest").to_string();
    worker.install_fault("after-effect-dispatch");

    let _ = worker.decide("dev-token", &approval_id, "approved", &digest);
    worker.wait_for_fault_exit();
    assert_eq!(worker.effect_count(), 1, "the external effect committed");

    worker.restart();
    assert_eq!(worker.effect_count(), 1);
    assert_eq!(worker.approval_by_id(&approval_id)["status"], "approved");
    let operations = worker.wait_operation_status(&id, "committed");
    assert_eq!(operations["operations"][0]["status"], "committed");
    // Replaying the decision never dispatches a second protected effect.
    let replay = worker.decide("dev-token", &approval_id, "approved", &digest);
    assert_eq!(replay.status(), 200);
    assert_eq!(worker.effect_count(), 1);
}

#[test]
fn approval_wait_does_not_monopolize_execution_capacity() {
    let worker = Worker::start_with_capacity(Some(1), &[("GOLDENBEE_EFFECT_APPROVAL", "1")]);
    let id = worker.submit("capacity-paused", "capacity-target");
    worker.wait_state(&id, &["waiting_approval"]);
    assert_eq!(worker.effect_count(), 0);

    // The single execution slot is available while the approval waits.
    let quick = worker.submit_raw("session-2", "quick follow-up", "capacity-quick");
    let quick_task = worker.wait_state(&quick, &["succeeded"]);
    assert!(is_state(&quick_task, "succeeded"));

    let approval = worker.approval_for(&id);
    let approval_id = approval["id"].as_str().expect("approval id").to_string();
    let digest = approval["digest"].as_str().expect("digest").to_string();
    let approved = worker.decide("dev-token", &approval_id, "approved", &digest);
    assert_eq!(approved.status(), 200);
    worker.wait_state(&id, &["succeeded"]);
    assert_eq!(worker.effect_count(), 1);
}

#[test]
fn approval_evidence_redacts_secret_material() {
    let worker = Worker::start(&[
        ("GOLDENBEE_EFFECT_APPROVAL", "1"),
        ("GOLDENBEE_EFFECT_BEHAVIOR", "secret"),
    ]);
    let id = worker.submit("approval-secret", "secret-target");
    worker.wait_state(&id, &["waiting_approval"]);
    let approval = worker.approval_for(&id);
    let approval_id = approval["id"].as_str().expect("approval id").to_string();
    let digest = approval["digest"].as_str().expect("digest").to_string();
    let approved = worker.decide("dev-token", &approval_id, "approved", &digest);
    assert_eq!(approved.status(), 200);
    worker.wait_state(&id, &["succeeded"]);

    let body = worker.approval_by_id(&approval_id).to_string();
    assert!(
        !body.contains("sk-canary"),
        "approval leaked a secret: {body}"
    );
    assert!(body.contains("[REDACTED]"), "{body}");
    let events = worker.events(&id).to_string();
    assert!(
        !events.contains("sk-canary"),
        "events leaked a secret: {events}"
    );
}

#[test]
fn evidence_redacts_secret_material_from_receipts_and_events() {
    let worker = Worker::start(&[("GOLDENBEE_EFFECT_BEHAVIOR", "secret")]);
    let id = worker.submit("secret-1", "eight");
    worker.wait_state(&id, &["succeeded"]);
    let body = worker.operations(&id).to_string();
    assert!(
        !body.contains("sk-canary"),
        "receipt leaked a secret: {body}"
    );
    assert!(body.contains("[REDACTED]"), "{body}");
    let events = worker.events(&id).to_string();
    assert!(
        !events.contains("sk-canary"),
        "events leaked a secret: {events}"
    );
}
