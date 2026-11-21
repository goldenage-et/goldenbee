//! Public approval journey through Gateway to the Worker that owns the effect.
//!
//! These tests start the real `gateway` process, the real protected-effect
//! Worker fixture, and observe the external ledger. Gateway forwards a decision
//! with the exact digest; it never records an approval outcome or executes the
//! protected operation.

use common::{
    ApprovalDecision, GatewayApproval, GatewaySubmitRequest, Limits, SocketCommand,
    SocketCommandFrame, SocketOutcome, SocketScope,
};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const CALLER_TOKEN: &str = "gateway-token";
const TENANT_TOKEN: &str = "tenant-token";
const FOREIGN_APPROVER_TOKEN: &str = "foreign-approver-token";
const DELEGATION_SECRET: &str = "t15-shared-delegation-secret";
const TOOL: &str = "ledger_write";

fn binary(name: &str) -> PathBuf {
    if let Ok(path) = std::env::var(format!("CARGO_BIN_EXE_{name}")) {
        return PathBuf::from(path);
    }
    let current = std::env::current_exe().expect("test executable path");
    let mut directory = current.parent();
    while let Some(candidate_directory) = directory {
        let candidate = candidate_directory.join(name);
        if candidate.is_file() {
            return candidate;
        }
        directory = candidate_directory.parent();
    }
    panic!("{name} is missing; build workspace binaries before running this test");
}

struct Worker {
    child: Child,
    data: tempfile::TempDir,
    config: PathBuf,
    env: Vec<(String, String)>,
    base: String,
    instance: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn worker_config(data: &Path, instance: &str) -> PathBuf {
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
    let text = format!(
        "instance = \"{instance}\"\nnamespace = \"local\"\ndefault_agent = \"effect\"\ntoken = \"dev-token\"\ntoken_scopes = [\"*\"]\n\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n",
        data.join("state").display()
    );
    let path = data.join("worker.toml");
    std::fs::write(&path, text).expect("worker config");
    path
}

impl Worker {
    fn start(approval_ttl_ms: Option<u64>) -> Self {
        let data = tempfile::tempdir().expect("worker data directory");
        let instance = format!(
            "effect-gateway-{}",
            data.path().file_name().unwrap().to_string_lossy()
        );
        let config = worker_config(data.path(), &instance);
        let mut env = vec![
            (
                "GOLDENBEE_EFFECT_DIR".to_string(),
                data.path().join("external").display().to_string(),
            ),
            ("GOLDENBEE_EFFECT_APPROVAL".to_string(), "1".to_string()),
            (
                "GOLDENBEE_DELEGATION_SECRET".to_string(),
                DELEGATION_SECRET.to_string(),
            ),
        ];
        if let Some(ttl) = approval_ttl_ms {
            env.push((
                "GOLDENBEE_EFFECT_APPROVAL_TTL_MS".to_string(),
                ttl.to_string(),
            ));
        }
        let (child, base) = spawn_worker(&config, &env, "127.0.0.1:0");
        Self {
            child,
            data,
            config,
            env,
            base,
            instance,
        }
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn restart(&mut self) {
        self.stop();
        let address = self
            .base
            .strip_prefix("http://")
            .expect("worker base is HTTP")
            .to_string();
        let (child, base) = spawn_worker(&self.config, &self.env, &address);
        self.child = child;
        self.base = base;
    }

    fn effect_count(&self) -> usize {
        std::fs::read_to_string(self.data.path().join("external/effects.log"))
            .map(|value| value.lines().filter(|line| !line.trim().is_empty()).count())
            .unwrap_or(0)
    }
}

#[allow(clippy::zombie_processes)]
fn spawn_worker(config: &Path, env: &[(String, String)], addr: &str) -> (Child, String) {
    let mut command = Command::new(binary("effect-fixture"));
    command
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            addr,
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

struct Stack {
    _config: tempfile::TempDir,
    config_path: PathBuf,
    fault_dir: PathBuf,
    gateway: Child,
    _stdout: BufReader<ChildStdout>,
    pub base: String,
    pub worker: Worker,
}

impl Drop for Stack {
    fn drop(&mut self) {
        let _ = self.gateway.kill();
        let _ = self.gateway.wait();
    }
}

impl Stack {
    fn start(approval_ttl_ms: Option<u64>) -> Self {
        let worker = Worker::start(approval_ttl_ms);
        let config = tempfile::tempdir().expect("gateway configuration directory");
        let path = config.path().join("gateway.json");
        let fault_dir = config.path().join("faults");
        std::fs::create_dir_all(&fault_dir).expect("fault directory");
        let document = serde_json::json!({
            "listen": "127.0.0.1:0",
            "data_dir": config.path().join("state"),
            "registry": config.path().join("registry.json"),
            "callers": [
                {
                    "token": CALLER_TOKEN,
                    "id": "caller-1",
                    "namespace": "local",
                    "scopes": ["task:submit", "task:read", "approval:read", "approval:decide", "admin"],
                    "workers": ["effect-a"]
                },
                {
                    "token": TENANT_TOKEN,
                    "id": "caller-2",
                    "namespace": "tenant-b",
                    "scopes": ["task:submit", "task:read", "approval:read", "approval:decide"],
                    "workers": ["effect-a"]
                },
                {
                    "token": FOREIGN_APPROVER_TOKEN,
                    "id": "caller-3",
                    "namespace": "local",
                    "scopes": ["approval:read", "approval:decide"],
                    "workers": ["effect-a"]
                }
            ],
            "workers": [
                {"id": "effect-a", "endpoint": worker.base, "instance": worker.instance}
            ]
        });
        std::fs::write(
            &path,
            serde_json::to_vec(&document).expect("configuration JSON"),
        )
        .expect("gateway configuration is written");
        let (gateway, stdout, base) = spawn_gateway(&path, &fault_dir);
        Self {
            _config: config,
            config_path: path,
            fault_dir,
            gateway,
            _stdout: stdout,
            base,
            worker,
        }
    }

    fn restart_gateway(&mut self) {
        let _ = self.gateway.kill();
        let _ = self.gateway.wait();
        let (gateway, stdout, base) = spawn_gateway(&self.config_path, &self.fault_dir);
        self.gateway = gateway;
        self._stdout = stdout;
        self.base = base;
    }

    fn wait_for_gateway_fault_exit(&mut self) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            match self
                .gateway
                .try_wait()
                .expect("gb-gateway status is observable")
            {
                Some(status) => {
                    assert_eq!(
                        status.code(),
                        Some(75),
                        "the injected fault must terminate the gateway"
                    );
                    return;
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("the injected approval fault did not terminate the gateway");
    }

    fn install_fault(&self, marker: &str) {
        std::fs::write(self.fault_dir.join(marker), "").expect("fault marker");
    }

    fn submit(&self, key: &str, entry: &str) -> String {
        self.submit_with(CALLER_TOKEN, key, entry)
    }

    fn submit_with(&self, token: &str, key: &str, entry: &str) -> String {
        let session = format!("session-{key}");
        let response = client()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "worker": "effect-a",
                "agent": "effect",
                "session_id": session,
                "input": format!("call:{TOOL} {{\"entry\":\"{entry}\"}}"),
                "idempotency_key": key
            }))
            .send()
            .expect("gateway submit responds");
        let status = response.status();
        let body = response.text().expect("submit body");
        assert!(status.is_success(), "submit failed: {status} {body}");
        let value: Value = serde_json::from_str(&body).expect("submit JSON");
        value["task_id"].as_str().expect("execution id").to_string()
    }

    fn task(&self, id: &str) -> Value {
        let response = client()
            .get(format!("{}/v1/tasks/{id}", self.base))
            .bearer_auth(CALLER_TOKEN)
            .send()
            .expect("gateway task responds");
        let status = response.status();
        let body = response.text().expect("task body");
        assert!(status.is_success(), "task failed: {status} {body}");
        serde_json::from_str(&body).expect("task JSON")
    }

    fn wait_state(&self, id: &str, wanted: &str) -> Value {
        let started = Instant::now();
        loop {
            let task = self.task(id);
            if task["task"]["state"].as_str() == Some(wanted) {
                return task;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "task stayed in {:?}: {task}",
                task["task"]["state"]
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn approvals(&self, token: &str, id: &str, pending: bool) -> reqwest::blocking::Response {
        client()
            .get(format!(
                "{}/v1/tasks/{id}/approvals?pending={pending}",
                self.base
            ))
            .bearer_auth(token)
            .send()
            .expect("gateway approvals respond")
    }

    fn pending(&self, id: &str) -> GatewayApproval {
        let started = Instant::now();
        loop {
            let response = self.approvals(CALLER_TOKEN, id, true);
            let status = response.status();
            let body = response.text().expect("approval list body");
            assert!(status.is_success(), "approval list failed: {status} {body}");
            let values: Vec<GatewayApproval> =
                serde_json::from_str(&body).expect("approval list JSON");
            if let Some(approval) = values
                .into_iter()
                .find(|approval| approval.approval.status == "pending")
            {
                return approval;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "task {id} never exposed a pending approval: {body}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn decide(
        &self,
        token: &str,
        id: &str,
        approval: &str,
        decision: &str,
        digest: &str,
    ) -> reqwest::blocking::Response {
        self.try_decide(token, id, approval, decision, digest)
            .expect("gateway decision responds")
    }

    fn try_decide(
        &self,
        token: &str,
        id: &str,
        approval: &str,
        decision: &str,
        digest: &str,
    ) -> Result<reqwest::blocking::Response, reqwest::Error> {
        client()
            .post(format!(
                "{}/v1/tasks/{id}/approvals/{approval}/decision",
                self.base
            ))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "decision": decision,
                "digest": digest
            }))
            .send()
    }

    fn approval(&self, token: &str, id: &str, approval: &str) -> reqwest::blocking::Response {
        client()
            .get(format!("{}/v1/tasks/{id}/approvals/{approval}", self.base))
            .bearer_auth(token)
            .send()
            .expect("gateway approval responds")
    }

    fn events(&self, id: &str) -> Value {
        let response = client()
            .get(format!(
                "{}/v1/tasks/{id}/events?after=0&limit=1000",
                self.base
            ))
            .bearer_auth(CALLER_TOKEN)
            .send()
            .expect("gateway events respond");
        let status = response.status();
        let body = response.text().expect("events body");
        assert!(status.is_success(), "events failed: {status} {body}");
        serde_json::from_str(&body).expect("events JSON")
    }

    fn audit_for(&self, worker: Option<&str>) -> Value {
        let suffix = worker
            .map(|worker| format!("?worker={worker}"))
            .unwrap_or_default();
        let response = client()
            .get(format!("{}/v1/admin/workers/audit{suffix}", self.base))
            .bearer_auth(CALLER_TOKEN)
            .send()
            .expect("audit responds");
        let status = response.status();
        let body = response.text().expect("audit body");
        assert!(status.is_success(), "audit failed: {status} {body}");
        serde_json::from_str(&body).expect("audit JSON")
    }

    fn wait_for_audit(&self, action: &str, outcome: &str) -> Value {
        self.wait_for_audit_for(Some("effect-a"), action, outcome)
    }

    fn wait_for_audit_for(&self, worker: Option<&str>, action: &str, outcome: &str) -> Value {
        let started = Instant::now();
        loop {
            let entries = self.audit_for(worker);
            if let Some(entry) = entries.as_array().and_then(|entries| {
                entries
                    .iter()
                    .rev()
                    .find(|entry| entry["action"] == action && entry["outcome"] == outcome)
            }) {
                return entry.clone();
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "audit never recorded {action}/{outcome}: {entries}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn spawn_gateway(path: &Path, fault_dir: &Path) -> (Child, BufReader<ChildStdout>, String) {
    let mut gateway = Command::new(binary("gb-gateway"))
        .args([
            "serve",
            "--addr",
            "127.0.0.1:0",
            "--config",
            path.to_str().expect("configuration path"),
        ])
        .env("GOLDENBEE_DELEGATION_SECRET", DELEGATION_SECRET)
        .env("GOLDENBEE_GATEWAY_FAULT_DIR", fault_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("gateway starts");
    let stdout = gateway.stdout.take().expect("gateway stdout is piped");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("gateway announces its listener");
    let base = line
        .split("listening on ")
        .nth(1)
        .unwrap_or_else(|| panic!("gateway address announcement: {line}"))
        .trim()
        .to_string();
    (gateway, reader, base)
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::new()
}

fn status_body(response: reqwest::blocking::Response) -> (reqwest::StatusCode, String) {
    let status = response.status();
    (status, response.text().expect("error body"))
}

fn sdk() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn sdk_submit(runtime: &tokio::runtime::Runtime, base: &str, key: &str, entry: &str) -> String {
    let client = sdk::Client::new(base, CALLER_TOKEN);
    runtime
        .block_on(client.submit(&GatewaySubmitRequest {
            worker: Some("effect-a".into()),
            agent: "effect".into(),
            session_id: format!("sdk-session-{key}"),
            input: format!("call:{TOOL} {{\"entry\":\"{entry}\"}}"),
            idempotency_key: key.into(),
            profile: None,
            team: None,
            team_version: None,
            limits: Limits::default(),
        }))
        .expect("sdk submit is accepted")
        .task_id
}

#[test]
fn gateway_forwards_the_exact_digest_and_approval_resumes_one_worker_effect() {
    let stack = Stack::start(None);
    let task_id = sdk_submit(&sdk(), &stack.base, "approval-forward", "approved-target");
    stack.wait_state(&task_id, "waiting_approval");
    assert_eq!(
        stack.worker.effect_count(),
        0,
        "approval must precede effect"
    );

    let pending = stack.pending(&task_id);
    assert_eq!(
        pending.task_id, task_id,
        "Gateway execution id is projected"
    );
    assert_eq!(pending.worker, "effect-a");
    assert_eq!(pending.approval.operation, TOOL);
    assert_eq!(pending.approval.target, "approved-target");
    assert_eq!(pending.approval.status, "pending");
    assert!(pending.approval.digest.starts_with("sha256:"));

    let runtime = sdk();
    let client = sdk::Client::new(&stack.base, CALLER_TOKEN);
    let sdk_pending = runtime
        .block_on(client.approvals(&task_id, true))
        .expect("sdk pending approvals");
    assert_eq!(sdk_pending.len(), 1);
    assert_eq!(sdk_pending[0].approval.digest, pending.approval.digest);
    let decided = runtime
        .block_on(client.decide_approval(
            &task_id,
            &pending.approval.id,
            &ApprovalDecision {
                decision: "approved".into(),
                digest: pending.approval.digest.clone(),
                answer: None,
            },
        ))
        .expect("sdk decision is forwarded");
    assert_eq!(decided.task_id, task_id);
    assert_eq!(decided.approval.status, "approved");

    stack.wait_state(&task_id, "succeeded");
    assert_eq!(stack.worker.effect_count(), 1);

    let recorded: GatewayApproval = serde_json::from_str(
        &stack
            .approval(CALLER_TOKEN, &task_id, &pending.approval.id)
            .text()
            .expect("approval body"),
    )
    .expect("approval JSON");
    assert_eq!(recorded.approval.digest, pending.approval.digest);
    assert!(recorded.approval.receipt.is_some(), "{recorded:?}");

    let events = stack.events(&task_id).to_string();
    assert!(events.contains("approval.approved"), "{events}");
    assert!(events.contains("approval.receipt"), "{events}");
    assert!(events.contains("operation.committed"), "{events}");

    // Duplicate delivery is the recorded outcome, not a second effect.
    let (status, _) = status_body(stack.decide(
        CALLER_TOKEN,
        &task_id,
        &pending.approval.id,
        "approved",
        &pending.approval.digest,
    ));
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(stack.worker.effect_count(), 1);
}

#[test]
fn rejection_and_expiry_block_the_effect_through_the_gateway() {
    // Keep rejection deterministic: task startup and the decision round trip
    // can exceed a very short approval TTL under a loaded CI runner.
    let stack = Stack::start(Some(5_000));
    let rejected = stack.submit("approval-reject", "rejected-target");
    stack.wait_state(&rejected, "waiting_approval");
    let approval = stack.pending(&rejected);
    let (status, body) = status_body(stack.decide(
        CALLER_TOKEN,
        &rejected,
        &approval.approval.id,
        "rejected",
        &approval.approval.digest,
    ));
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    stack.wait_state(&rejected, "succeeded");
    assert_eq!(
        stack.worker.effect_count(),
        0,
        "rejection blocked the effect"
    );

    // Use a separate short-lived approval for the expiry assertion.
    let stack = Stack::start(Some(150));
    let expired = stack.submit("approval-expire", "expired-target");
    stack.wait_state(&expired, "waiting_approval");
    let approval = stack.pending(&expired);
    std::thread::sleep(Duration::from_millis(250));
    let (status, body) = status_body(stack.decide(
        CALLER_TOKEN,
        &expired,
        &approval.approval.id,
        "approved",
        &approval.approval.digest,
    ));
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{body}");
    assert!(body.contains("approval.expired"), "{body}");
    assert_eq!(stack.worker.effect_count(), 0, "expiry blocked the effect");
}

#[test]
fn wrong_namespace_and_foreign_approval_references_are_rejected() {
    let stack = Stack::start(None);
    let first = stack.submit("approval-first", "first-target");
    stack.wait_state(&first, "waiting_approval");
    let second = stack.submit("approval-second", "second-target");
    stack.wait_state(&second, "waiting_approval");
    let first_approval = stack.pending(&first);
    let second_approval = stack.pending(&second);

    let (status, body) = status_body(stack.approvals(TENANT_TOKEN, &first, true));
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("route.task.not_found"), "{body}");
    let entry = stack.wait_for_audit_for(Some("unresolved"), "approval.denied", "denied");
    assert_eq!(entry["principal"], "caller-2", "{entry}");
    assert!(
        entry["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("route.task.not_found"),
        "{entry}"
    );

    let (status, body) =
        status_body(stack.approval(CALLER_TOKEN, &first, &second_approval.approval.id));
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("approval.not_found"), "{body}");

    let (status, body) = status_body(stack.decide(
        CALLER_TOKEN,
        &first,
        &second_approval.approval.id,
        "approved",
        &second_approval.approval.digest,
    ));
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{body}");
    assert_eq!(stack.worker.effect_count(), 0);

    // A changed digest is rejected by the Gateway before forwarding.
    let (status, body) = status_body(stack.decide(
        CALLER_TOKEN,
        &first,
        &first_approval.approval.id,
        "approved",
        "sha256:changed-arguments",
    ));
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{body}");
    assert!(body.contains("approval.digest_mismatch"), "{body}");
    assert_eq!(stack.worker.effect_count(), 0);

    // The exact operation still executes, proving only the changed binding was denied.
    let (status, body) = status_body(stack.decide(
        CALLER_TOKEN,
        &first,
        &first_approval.approval.id,
        "approved",
        &first_approval.approval.digest,
    ));
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    stack.wait_state(&first, "succeeded");
    assert_eq!(stack.worker.effect_count(), 1);
}

#[test]
fn gateway_restart_and_lost_ack_keep_one_decision_and_one_effect() {
    let mut stack = Stack::start(None);
    let task_id = stack.submit("approval-restart", "restart-target");
    stack.wait_state(&task_id, "waiting_approval");
    let pending = stack.pending(&task_id);

    stack.install_fault("after-approval-forward");
    let _ = stack
        .try_decide(
            CALLER_TOKEN,
            &task_id,
            &pending.approval.id,
            "approved",
            &pending.approval.digest,
        )
        .expect_err("the lost acknowledgment is an uncertain transport result");
    stack.wait_for_gateway_fault_exit();
    stack.restart_gateway();

    let started = Instant::now();
    while stack.worker.effect_count() == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the forwarded decision never produced its effect"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(stack.worker.effect_count(), 1);

    let (status, body) = status_body(stack.decide(
        CALLER_TOKEN,
        &task_id,
        &pending.approval.id,
        "approved",
        &pending.approval.digest,
    ));
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(
        stack.worker.effect_count(),
        1,
        "duplicate delivery is idempotent"
    );

    stack.wait_state(&task_id, "succeeded");
    // Durable approval and terminal events replay after reconnect.
    let events = stack.events(&task_id).to_string();
    assert!(events.contains("approval.approved"), "{events}");
    assert_eq!(
        events.matches("\"kind\":\"approval.approved\"").count(),
        1,
        "{events}"
    );
}

#[test]
fn unreachable_worker_is_not_reported_approved_or_executed() {
    let mut stack = Stack::start(None);
    let task_id = stack.submit("approval-unreachable", "unreachable-target");
    stack.wait_state(&task_id, "waiting_approval");
    let pending = stack.pending(&task_id);
    stack.worker.stop();

    let (status, body) = status_body(stack.decide(
        CALLER_TOKEN,
        &task_id,
        &pending.approval.id,
        "approved",
        &pending.approval.digest,
    ));
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("route.worker_unavailable"), "{body}");
    assert_eq!(stack.worker.effect_count(), 0);

    stack.worker.restart();
    let started = Instant::now();
    let mut decided = false;
    let mut last = String::new();
    while started.elapsed() < Duration::from_secs(10) {
        let response = stack.decide(
            CALLER_TOKEN,
            &task_id,
            &pending.approval.id,
            "approved",
            &pending.approval.digest,
        );
        if response.status().is_success() {
            decided = true;
            break;
        }
        last = response.text().unwrap_or_default();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        decided,
        "the decision becomes forwardable after the Worker returns: {last}"
    );

    // The Worker re-owned the interrupted invocation. Its adapter has no
    // resumable checkpoint for this task, so approval does not fabricate
    // execution: the task stays recovering and the effect stays absent.
    let task = stack.wait_state(&task_id, "recovering");
    assert_eq!(task["task"]["state"], "recovering");
    assert_eq!(stack.worker.effect_count(), 0);
    let recorded: GatewayApproval = serde_json::from_str(
        &stack
            .approval(CALLER_TOKEN, &task_id, &pending.approval.id)
            .text()
            .expect("approval body"),
    )
    .expect("approval JSON");
    assert_eq!(recorded.approval.status, "approved");
}

#[test]
fn unauthorized_and_denied_decisions_are_audited() {
    let stack = Stack::start(None);
    let task_id = stack.submit("approval-audit", "audit-target");
    stack.wait_state(&task_id, "waiting_approval");
    let pending = stack.pending(&task_id);

    // A caller without approval authority is rejected at the Gateway boundary.
    let (status, body) = status_body(stack.decide(
        FOREIGN_APPROVER_TOKEN,
        &task_id,
        &pending.approval.id,
        "approved",
        &pending.approval.digest,
    ));
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{body}");
    assert_eq!(stack.worker.effect_count(), 0);

    // A changed digest is denied at the Gateway and retained in the audit projection.
    let (status, body) = status_body(stack.decide(
        CALLER_TOKEN,
        &task_id,
        &pending.approval.id,
        "approved",
        "sha256:wrong-operation",
    ));
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{body}");
    let entry = stack.wait_for_audit("approval.denied", "denied");
    assert!(
        entry["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("approval.digest_mismatch"),
        "{entry}"
    );
    assert_eq!(entry["principal"], "caller-1");
    assert!(
        !entry.to_string().contains("sha256:wrong-operation"),
        "{entry}"
    );
}

#[test]
fn socket_approval_replays_the_recorded_worker_outcome_without_a_second_effect() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let stack = Stack::start(None);
    let task_id = stack.submit("socket-approval", "one");
    let pending = stack.pending(&task_id);
    let frame = SocketCommandFrame::new(
        "approve-socket",
        SocketScope {
            worker: None,
            task_id: Some(task_id.clone()),
        },
        SocketCommand::Approval {
            task_id: task_id.clone(),
            approval_id: pending.approval.id.clone(),
            decision: ApprovalDecision {
                decision: "approved".into(),
                digest: pending.approval.digest.clone(),
                answer: None,
            },
        },
    );
    let socket = sdk::Socket::new(&stack.base, CALLER_TOKEN);

    let mut session = runtime.block_on(socket.connect()).expect("connect");
    let outcome = runtime
        .block_on(session.command(&frame))
        .expect("approval ack");
    let SocketOutcome::Approval { approval } = outcome else {
        panic!("unexpected approval outcome: {outcome:?}");
    };
    assert_eq!(approval.approval.status, "approved");
    drop(session);

    // A reconnect repeats the same acknowledged mutation; the Worker's
    // recorded outcome is returned and no second protected effect is written.
    let mut again = runtime.block_on(socket.connect()).expect("reconnect");
    let replay = runtime
        .block_on(again.command(&frame))
        .expect("approval replay ack");
    let SocketOutcome::Approval { approval } = replay else {
        panic!("unexpected approval replay outcome: {replay:?}");
    };
    assert_eq!(approval.approval.status, "approved");
    stack.wait_state(&task_id, "succeeded");
    assert_eq!(stack.worker.effect_count(), 1);
    assert_eq!(stack.task(&task_id)["task"]["state"], "succeeded");
}
