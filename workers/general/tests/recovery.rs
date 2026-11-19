//! Cross-process evidence for ticket 22: an ordinary General Master Worker
//! coordinates independent remote Worker instances, evaluates objective
//! criteria against observed evidence, and recovers durable plans without
//! creating duplicate children.

use common::{Limits, SubmitTaskRequest};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SECRET: &str = "ticket-22-recovery-delegation-secret";
const MASTER_TOKEN: &str = "master-token";
const B_TOKEN: &str = "remote-b-token";
const C_TOKEN: &str = "remote-c-token";

struct Worker {
    child: Option<Child>,
    base: String,
    addr: String,
    config: PathBuf,
    data: PathBuf,
    env: Vec<(&'static str, String)>,
    instance: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Default)]
struct PeerSpec {
    id: String,
    identity: String,
    endpoint: String,
    capabilities: Vec<String>,
    max_depth: u32,
    max_fanout: u32,
}

fn write_config(data: &Path, instance: &str, token: &str, peers: &[PeerSpec]) -> PathBuf {
    let assets = data.join("assets");
    let agents = assets.join("agents");
    let prompts = assets.join("prompts");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::write(
        prompts.join("general.md"),
        "You are the Goldenbee General Worker. Answer usefully and concisely.",
    )
    .expect("general prompt");
    std::fs::write(
        agents.join("general.toml"),
        "name = \"general\"\ndescription = \"general native ADK agent\"\nmodel = \"deterministic\"\nprompt = \"general\"\n",
    )
    .expect("agent definition");
    let mut config = format!(
        "instance = \"{instance}\"\nnamespace = \"local\"\ndefault_agent = \"general\"\ntoken = \"{token}\"\ntoken_scopes = [\"*\"]\n",
    );
    config.push_str(&format!(
        "\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n",
        data.display()
    ));
    for peer in peers {
        config.push_str(&format!(
            "\n[peers.{}]\nidentity = \"{}\"\nendpoint = \"{}\"\ncapabilities = [{}]\ntoken_ttl_ms = 30000\nmax_depth = {}\nmax_fanout = {}\nlimits = {{ max_depth = {}, max_fanout = {}, max_duration_ms = 30000 }}\n",
            peer.id,
            peer.identity,
            peer.endpoint,
            peer.capabilities
                .iter()
                .map(|capability| format!("\"{capability}\""))
                .collect::<Vec<_>>()
                .join(", "),
            peer.max_depth,
            peer.max_fanout,
            peer.max_depth,
            peer.max_fanout,
        ));
    }
    let path = data.join("worker.toml");
    std::fs::write(&path, config).expect("worker config");
    path
}

fn spawn(
    config: PathBuf,
    data: PathBuf,
    instance: String,
    env: Vec<(&'static str, String)>,
    addr: &str,
) -> Worker {
    let mut command = Command::new(env!("CARGO_BIN_EXE_general"));
    command
        .args([
            "serve",
            "--config",
            config.to_str().unwrap(),
            "--addr",
            addr,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GOLDENBEE_DELEGATION_SECRET", SECRET);
    for (key, value) in &env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("worker serves");
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
    assert!(base.starts_with("http://127.0.0.1:"), "{line}");
    let addr = base.strip_prefix("http://").unwrap_or(&base).to_string();
    let health = reqwest::blocking::Client::new()
        .get(format!("{base}/v1/health"))
        .send()
        .expect("health responds")
        .json::<Value>()
        .expect("health JSON");
    assert_eq!(health["instance"], instance, "{health}");
    Worker {
        child: Some(child),
        base,
        addr,
        config,
        data,
        env,
        instance,
    }
}

impl Worker {
    fn start(data: &Path, instance: &str, token: &str, peers: &[PeerSpec]) -> Self {
        let config = write_config(data, instance, token, peers);
        spawn(
            config,
            data.to_path_buf(),
            instance.to_string(),
            Vec::new(),
            "127.0.0.1:0",
        )
    }

    fn restart(&mut self) {
        self.stop();
        *self = spawn(
            self.config.clone(),
            self.data.clone(),
            self.instance.clone(),
            self.env.clone(),
            &self.addr,
        );
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn wait_exit(&mut self) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(child) = self.child.as_mut() {
                if let Some(status) = child.try_wait().expect("child state") {
                    self.child = None;
                    return status.code().unwrap_or(-1);
                }
            } else {
                return -1;
            }
            assert!(Instant::now() < deadline, "worker did not exit");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn client(&self) -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("http client")
    }

    fn get(&self, token: &str, path: &str) -> (reqwest::StatusCode, Value) {
        let response = self
            .client()
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .expect("GET responds");
        let status = response.status();
        let body = response.text().unwrap_or_default();
        (
            status,
            serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body})),
        )
    }

    fn post(&self, token: &str, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
        let response = self
            .client()
            .post(format!("{}{path}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .expect("POST responds");
        let status = response.status();
        let body = response.text().unwrap_or_default();
        (
            status,
            serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body})),
        )
    }

    fn wait_ready(&self, token: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (status, health) = self.get(token, "/v1/status");
            if status.is_success() && health["ready"] == true {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not become ready: {health}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn submit_parent(&self, key: &str) -> String {
        let request = SubmitTaskRequest {
            namespace: None,
            agent: "general".into(),
            session_id: format!("parent-{key}"),
            input: "master objective".into(),
            idempotency_key: key.into(),
            profile: None,
            team: None,
            worker: None,
            limits: Limits::default(),
        };
        let (status, body) = self.post(
            MASTER_TOKEN,
            "/v1/tasks",
            serde_json::to_value(request).unwrap(),
        );
        assert!(status.is_success(), "{body}");
        body["task_id"]
            .as_str()
            .expect("parent task id")
            .to_string()
    }

    fn a2a(&self, token: &str, method: &str, params: Value) -> (reqwest::StatusCode, Value) {
        let response = self
            .client()
            .post(format!("{}/a2a", self.base))
            .bearer_auth(token)
            .header("A2A-Version", "1.0")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            }))
            .send()
            .expect("A2A responds");
        let status = response.status();
        let body = response.text().unwrap_or_default();
        (
            status,
            serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body})),
        )
    }

    fn remote_tasks(&self, token: &str) -> Vec<Value> {
        let (status, body) = self.a2a(token, "ListTasks", json!({}));
        assert!(status.is_success(), "{body}");
        body["result"].as_array().cloned().unwrap_or_default()
    }

    fn plan(&self, id: &str) -> Value {
        let (status, body) = self.get(MASTER_TOKEN, &format!("/v1/plans/{id}"));
        assert!(status.is_success(), "{body}");
        body
    }

    fn wait_plan(&self, id: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let plan = self.plan(id);
            if predicate(&plan) {
                return plan;
            }
            assert!(
                Instant::now() < deadline,
                "plan did not reach the expected state: {plan}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

fn peer(id: &str, identity: &str, endpoint: &str, capabilities: &[&str]) -> PeerSpec {
    PeerSpec {
        id: id.into(),
        identity: identity.into(),
        endpoint: endpoint.into(),
        capabilities: capabilities.iter().map(|value| value.to_string()).collect(),
        max_depth: 2,
        max_fanout: 2,
    }
}

fn artifact_digest(task: &Value) -> Option<String> {
    let artifacts = task["result"]["artifacts"].as_array()?;
    let artifact = artifacts.first()?;
    let parts = artifact["parts"].as_array()?;
    let text = parts
        .iter()
        .filter_map(|part| part["text"].as_str())
        .collect::<String>();
    (!text.is_empty()).then(|| common::bytes_digest(text.as_bytes()))
}

fn children(plan: &Value) -> &Vec<Value> {
    plan["children"].as_array().expect("plan children")
}

fn child<'a>(plan: &'a Value, id: &str) -> &'a Value {
    children(plan)
        .iter()
        .find(|child| child["id"] == id)
        .unwrap_or_else(|| panic!("child '{id}' is present: {plan}"))
}

#[test]
fn master_plan_two_remote_workers_evaluate_observed_artifact_evidence() {
    let b_data = tempfile::tempdir().unwrap();
    let c_data = tempfile::tempdir().unwrap();
    let b = Worker::start(b_data.path(), "remote-b", B_TOKEN, &[]);
    let c = Worker::start(c_data.path(), "remote-c", C_TOKEN, &[]);
    let a_data = tempfile::tempdir().unwrap();
    let a = Worker::start(
        a_data.path(),
        "master-a",
        MASTER_TOKEN,
        &[
            peer("remote-b", "remote-b", &b.base, &["general"]),
            peer("remote-c", "remote-c", &c.base, &["general"]),
        ],
    );
    let parent = a.submit_parent("master-plan-parent");

    let body = json!({
        "parent_task_id": parent,
        "objective": "aggregate two independent specialist results",
        "criteria": [
            {"id": "b-output", "description": "b reports its result", "child": "child-b",
             "evidence": {"kind": "output_contains", "text": "alpha artifact objective"}},
            {"id": "b-artifact", "description": "b produced immutable evidence", "child": "child-b",
             "evidence": {"kind": "artifact_digest"}},
            {"id": "c-output", "description": "c reports its result", "child": "child-c",
             "evidence": {"kind": "output_contains", "text": "beta artifact objective"}},
            {"id": "c-artifact", "description": "c produced immutable evidence", "child": "child-c",
             "evidence": {"kind": "artifact_digest"}}
        ],
        "children": [
            {"id": "child-b", "peer": "remote-b", "capability": "general",
             "objective": "alpha artifact objective"},
            {"id": "child-c", "peer": "remote-c", "capability": "general",
             "objective": "beta artifact objective"}
        ]
    });
    let (status, created) = a.post(MASTER_TOKEN, "/v1/plans", body.clone());
    assert_eq!(status, reqwest::StatusCode::OK, "{created}");
    let plan_id = created["id"].as_str().expect("plan id").to_string();
    assert_eq!(created["parent_task_id"], parent.as_str());
    assert_eq!(created["provenance"]["instance"], "master-a");
    assert_eq!(created["provenance"]["package"], "general");
    assert_eq!(created["provenance"]["profile"], "default");

    // Duplicate creation is idempotent and dispatches nothing.
    let (status, replay) = a.post(MASTER_TOKEN, "/v1/plans", body);
    assert_eq!(status, reqwest::StatusCode::OK, "{replay}");
    assert_eq!(replay["id"], plan_id.as_str());
    assert_eq!(
        replay["children"].as_array().unwrap().len(),
        children(&created).len()
    );

    let (status, dispatched) = a.post(
        MASTER_TOKEN,
        &format!("/v1/plans/{plan_id}/dispatch"),
        json!({"wait_ms": 15000}),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{dispatched}");
    let plan = a.wait_plan(&plan_id, |plan| {
        plan["state"] == "verified"
            && children(plan)
                .iter()
                .all(|child| child["state"] == "completed")
    });
    assert_eq!(plan["state"], "verified", "{plan}");
    assert_eq!(plan["evaluation"]["outcome"], "verified", "{plan}");
    assert_eq!(plan["evaluation"]["missing"], json!([]), "{plan}");
    assert_eq!(plan["evaluation"]["conflicting"], json!([]), "{plan}");

    let b_task = a2a_task(
        &b,
        B_TOKEN,
        child(&plan, "child-b")["remote_task_id"].as_str().unwrap(),
    );
    let c_task = a2a_task(
        &c,
        C_TOKEN,
        child(&plan, "child-c")["remote_task_id"].as_str().unwrap(),
    );
    assert_eq!(
        b_task["result"]["metadata"]["goldenbee.parent_task"],
        parent
    );
    assert_eq!(
        b_task["result"]["metadata"]["goldenbee.parent_instance"],
        "master-a"
    );
    assert_eq!(b_task["result"]["metadata"]["goldenbee.invocations"], 1);
    assert_eq!(
        c_task["result"]["metadata"]["goldenbee.parent_task"],
        parent
    );
    assert_eq!(c_task["result"]["metadata"]["goldenbee.invocations"], 1);

    let b_digest = artifact_digest(&b_task).expect("b artifact digest");
    let c_digest = artifact_digest(&c_task).expect("c artifact digest");
    assert_eq!(
        child(&plan, "child-b")["artifacts"][0]["digest"],
        b_digest.as_str(),
        "{plan}"
    );
    assert_eq!(
        child(&plan, "child-c")["artifacts"][0]["digest"],
        c_digest.as_str(),
        "{plan}"
    );
    assert_eq!(b.remote_tasks(B_TOKEN).len(), 1);
    assert_eq!(c.remote_tasks(C_TOKEN).len(), 1);
}

fn a2a_task(worker: &Worker, token: &str, id: &str) -> Value {
    let (status, body) = worker.a2a(token, "GetTask", json!({"id": id, "historyLength": 10}));
    assert!(status.is_success(), "{body}");
    body
}

fn binding_for_key(worker: &Worker, key: &str) -> Option<Value> {
    let (status, body) = worker.get(MASTER_TOKEN, "/v1/delegations");
    assert!(status.is_success(), "{body}");
    body.as_array()?
        .iter()
        .find(|binding| binding["idempotency_key"] == key)
        .cloned()
}

fn active_remote_state(state: &str) -> bool {
    matches!(state, "submitted" | "working" | "prepared" | "unknown")
}

#[test]
fn master_plan_recovery_keeps_complete_and_active_child_without_duplicates() {
    let b_data = tempfile::tempdir().unwrap();
    let c_data = tempfile::tempdir().unwrap();
    let b = Worker::start(b_data.path(), "remote-b", B_TOKEN, &[]);
    let c = Worker::start(c_data.path(), "remote-c", C_TOKEN, &[]);
    let a_data = tempfile::tempdir().unwrap();
    let mut a = Worker::start(
        a_data.path(),
        "master-a",
        MASTER_TOKEN,
        &[
            peer("remote-b", "remote-b", &b.base, &["general"]),
            peer("remote-c", "remote-c", &c.base, &["general"]),
        ],
    );
    let parent = a.submit_parent("restart-parent");
    let body = json!({
        "parent_task_id": parent,
        "objective": "finish one child and recover the active child",
        "criteria": [
            {"id": "b-output", "child": "child-b",
             "evidence": {"kind": "output_contains", "text": "fast result"}},
            {"id": "b-artifact", "child": "child-b",
             "evidence": {"kind": "artifact_digest"}},
            {"id": "c-output", "child": "child-c",
             "evidence": {"kind": "output_contains", "text": "slow result"}},
            {"id": "c-artifact", "child": "child-c",
             "evidence": {"kind": "artifact_digest"}}
        ],
        "children": [
            {"id": "child-b", "peer": "remote-b", "capability": "general",
             "objective": "fast result"},
            {"id": "child-c", "peer": "remote-c", "capability": "general",
             "objective": "delay:4000 slow result"}
        ]
    });
    let (status, created) = a.post(MASTER_TOKEN, "/v1/plans", body);
    assert_eq!(status, reqwest::StatusCode::OK, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    let c_key = child(&created, "child-c")["idempotency_key"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = a.post(
        MASTER_TOKEN,
        &format!("/v1/plans/{id}/dispatch"),
        json!({"wait_ms": 0}),
    );
    assert_eq!(status, reqwest::StatusCode::OK);

    // Kill the Master with child-b complete and child-c active.
    let (before, c_binding) = {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let plan = a.plan(&id);
            let b_done = child(&plan, "child-b")["state"] == "completed";
            let binding = binding_for_key(&a, &c_key);
            let c_active = binding
                .as_ref()
                .is_some_and(|binding| binding["state"].as_str().is_some_and(active_remote_state));
            if b_done && c_active {
                break (plan, binding.expect("active child binding"));
            }
            assert!(
                Instant::now() < deadline,
                "expected one complete and one active child: {plan}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    let b_remote = child(&before, "child-b")["remote_task_id"]
        .as_str()
        .unwrap()
        .to_string();
    // The active child's native task id is observable on the remote A2A store
    // even before its synchronous acknowledgment reaches the parent.
    let c_remote = c.remote_tasks(C_TOKEN)[0]["id"]
        .as_str()
        .expect("active child remote id")
        .to_string();
    let _ = c_binding;
    a.stop();
    assert_eq!(b.remote_tasks(B_TOKEN).len(), 1);
    assert_eq!(c.remote_tasks(C_TOKEN).len(), 1);

    a.restart();
    let (status, recovered) = a.post(
        MASTER_TOKEN,
        &format!("/v1/plans/{id}/recover"),
        json!({"wait_ms": 15000}),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{recovered}");
    assert_eq!(recovered["id"], id.as_str());
    assert_eq!(recovered["objective"], before["objective"]);
    assert_eq!(recovered["criteria"], before["criteria"]);
    assert_eq!(
        recovered["provenance"]["instance"], "master-a",
        "{recovered}"
    );
    assert_eq!(recovered["provenance"]["package"], "general");
    assert_eq!(recovered["provenance"]["profile"], "default");
    assert_eq!(
        recovered["provenance"]["profile_revision"],
        before["provenance"]["profile_revision"]
    );
    assert_eq!(recovered["state"], "verified", "{recovered}");
    assert_eq!(child(&recovered, "child-b")["state"], "completed");
    assert_eq!(child(&recovered, "child-c")["state"], "completed");
    assert_eq!(
        child(&recovered, "child-b")["remote_task_id"],
        b_remote.as_str()
    );
    assert_eq!(
        child(&recovered, "child-c")["remote_task_id"],
        c_remote.as_str()
    );
    assert_eq!(recovered["recovery"]["duplicates"], 0, "{recovered}");
    assert_eq!(
        recovered["recovery"]["queried"].as_array().unwrap().len(),
        2,
        "{recovered}"
    );
    assert_eq!(b.remote_tasks(B_TOKEN).len(), 1, "no duplicate child");
    assert_eq!(c.remote_tasks(C_TOKEN).len(), 1, "no duplicate child");
    assert!(
        child(&recovered, "child-c")["artifacts"][0]["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"),
        "{recovered}"
    );
}

#[test]
fn master_plan_lost_ack_and_unknown_child_stay_reconcilable() {
    let b_data = tempfile::tempdir().unwrap();
    let mut b = Worker::start(b_data.path(), "remote-b", B_TOKEN, &[]);
    let fault_dir = tempfile::tempdir().unwrap();
    let fault_marker = fault_dir.path().join("after-peer-send");
    std::fs::write(&fault_marker, b"1").unwrap();
    assert!(fault_marker.exists());
    let a_data = tempfile::tempdir().unwrap();
    let config = write_config(
        a_data.path(),
        "master-a",
        MASTER_TOKEN,
        &[peer("remote-b", "remote-b", &b.base, &["general"])],
    );
    let mut a = spawn(
        config,
        a_data.path().to_path_buf(),
        "master-a".into(),
        vec![(
            "GOLDENBEE_FAULT_DIR",
            fault_dir.path().display().to_string(),
        )],
        "127.0.0.1:0",
    );
    let parent = a.submit_parent("lost-ack-parent");
    let (status, created) = a.post(
        MASTER_TOKEN,
        "/v1/plans",
        json!({
            "parent_task_id": parent,
            "objective": "recover an acknowledged child once",
            "criteria": [
                {"id": "output", "child": "child-b",
                 "evidence": {"kind": "output_contains", "text": "ack lost objective"}},
                {"id": "artifact", "child": "child-b",
                 "evidence": {"kind": "artifact_digest"}}
            ],
            "children": [
                {"id": "child-b", "peer": "remote-b", "capability": "general",
                 "objective": "ack lost objective"}
            ]
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    let _ = a.post(
        MASTER_TOKEN,
        &format!("/v1/plans/{id}/dispatch"),
        json!({"wait_ms": 0}),
    );
    assert_eq!(a.wait_exit(), 75, "fault exits the Master");
    let remote_tasks = b.remote_tasks(B_TOKEN);
    assert_eq!(remote_tasks.len(), 1, "the child was accepted exactly once");
    let remote_id = remote_tasks[0]["id"].as_str().unwrap().to_string();

    // The remote child becomes unreachable before the parent can reconcile.
    b.stop();
    a.restart();
    a.wait_ready(MASTER_TOKEN);
    let (status, uncertain) = a.post(
        MASTER_TOKEN,
        &format!("/v1/plans/{id}/recover"),
        json!({"wait_ms": 500}),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{uncertain}");
    assert_eq!(child(&uncertain, "child-b")["state"], "unreachable");
    assert_eq!(
        uncertain["evaluation"]["outcome"], "unverified",
        "{uncertain}"
    );
    assert_eq!(
        child(&uncertain, "child-b")["last_error"]["code"],
        "peer.unreachable",
        "{uncertain}"
    );
    assert!(
        child(&uncertain, "child-b")["binding_id"]
            .as_str()
            .is_some(),
        "the deterministic stored handle survives the acknowledgment-loss window: {uncertain}"
    );

    // Restarting the same remote instance makes the uncertain child
    // reconcilable without a replacement child or a repeated objective.
    b.restart();
    b.wait_ready(B_TOKEN);
    let (status, recovered) = a.post(
        MASTER_TOKEN,
        &format!("/v1/plans/{id}/recover"),
        json!({"wait_ms": 15000}),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{recovered}");
    assert_eq!(recovered["state"], "verified", "{recovered}");
    assert_eq!(child(&recovered, "child-b")["state"], "completed");
    assert_eq!(
        child(&recovered, "child-b")["remote_task_id"],
        remote_id.as_str()
    );
    assert_eq!(recovered["recovery"]["duplicates"], 0);
    assert_eq!(b.remote_tasks(B_TOKEN).len(), 1, "no replacement child");
}

#[test]
fn master_plan_unreachable_child_is_bounded_partial_and_escalatable() {
    let b_data = tempfile::tempdir().unwrap();
    let c_data = tempfile::tempdir().unwrap();
    let b = Worker::start(b_data.path(), "remote-b", B_TOKEN, &[]);
    let mut c = Worker::start(c_data.path(), "remote-c", C_TOKEN, &[]);
    c.stop();
    let a_data = tempfile::tempdir().unwrap();
    let a = Worker::start(
        a_data.path(),
        "master-a",
        MASTER_TOKEN,
        &[
            peer("remote-b", "remote-b", &b.base, &["general"]),
            peer("remote-c", "remote-c", &c.base, &["general"]),
        ],
    );
    let parent = a.submit_parent("bounded-parent");
    let (status, created) = a.post(
        MASTER_TOKEN,
        "/v1/plans",
        json!({
            "parent_task_id": parent,
            "objective": "report partial evidence when one specialist is down",
            "criteria": [
                {"id": "b-output", "child": "child-b",
                 "evidence": {"kind": "output_contains", "text": "bounded okay"}},
                {"id": "c-output", "child": "child-c",
                 "evidence": {"kind": "output_contains", "text": "unreachable result"}}
            ],
            "children": [
                {"id": "child-b", "peer": "remote-b", "capability": "general",
                 "objective": "bounded okay",
                 "limits": {"max_depth": 9, "max_fanout": 9}},
                {"id": "child-c", "peer": "remote-c", "capability": "general",
                 "objective": "unreachable result",
                 "limits": {"max_depth": 9, "max_fanout": 9}}
            ]
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    let started = Instant::now();
    let (status, dispatched) = a.post(
        MASTER_TOKEN,
        &format!("/v1/plans/{id}/dispatch"),
        json!({"wait_ms": 1500}),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{dispatched}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "bounded dispatch must not wait forever: {dispatched}"
    );
    let plan = a.wait_plan(&id, |plan| {
        plan["state"] == "partial"
            && child(plan, "child-b")["state"] == "completed"
            && child(plan, "child-c")["state"] == "unreachable"
    });
    assert_eq!(plan["state"], "partial", "{plan}");
    assert_eq!(plan["evaluation"]["outcome"], "partial", "{plan}");
    assert_eq!(plan["evaluation"]["missing"], json!(["c-output"]), "{plan}");
    assert_eq!(
        plan["evaluation"]["incomplete"],
        json!(["child-c"]),
        "{plan}"
    );
    assert_eq!(
        child(&plan, "child-c")["last_error"]["code"],
        "peer.unreachable",
        "{plan}"
    );

    // Frozen authority is never widened to find a replacement.
    let (status, bindings) = a.get(MASTER_TOKEN, "/v1/delegations");
    assert_eq!(status, reqwest::StatusCode::OK, "{bindings}");
    let binding = bindings
        .as_array()
        .unwrap()
        .iter()
        .find(|binding| binding["peer"] == child(&plan, "child-b")["peer"])
        .expect("healthy child binding");
    assert!(
        binding["limits"]["max_depth"].as_u64().unwrap() <= 2,
        "{binding}"
    );
    assert!(
        binding["limits"]["max_fanout"].as_u64().unwrap() <= 2,
        "{binding}"
    );

    let (status, escalated) = a.post(
        MASTER_TOKEN,
        &format!("/v1/plans/{id}/escalate"),
        json!({"reason": "specialist remote-c remains unreachable"}),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{escalated}");
    assert_eq!(escalated["state"], "escalated", "{escalated}");
    assert_eq!(
        escalated["escalation"]["reason"],
        "specialist remote-c remains unreachable"
    );
    assert_eq!(b.remote_tasks(B_TOKEN).len(), 1);
    c.restart();
    c.wait_ready(C_TOKEN);
    assert!(c.remote_tasks(C_TOKEN).is_empty(), "no replacement child");
}

#[test]
fn master_plan_cancels_exact_children_and_denies_unauthorized_targets() {
    let b_data = tempfile::tempdir().unwrap();
    let c_data = tempfile::tempdir().unwrap();
    let b = Worker::start(b_data.path(), "remote-b", B_TOKEN, &[]);
    let c = Worker::start(c_data.path(), "remote-c", C_TOKEN, &[]);
    let a_data = tempfile::tempdir().unwrap();
    let a = Worker::start(
        a_data.path(),
        "master-a",
        MASTER_TOKEN,
        &[
            peer("remote-b", "remote-b", &b.base, &["general"]),
            peer("remote-c", "remote-c", &c.base, &["general"]),
        ],
    );
    let parent = a.submit_parent("cancel-parent");
    let (status, created) = a.post(
        MASTER_TOKEN,
        "/v1/plans",
        json!({
            "parent_task_id": parent,
            "objective": "cancel both long children",
            "criteria": [
                {"id": "b", "child": "child-b",
                 "evidence": {"kind": "output_contains", "text": "never returned"}},
                {"id": "c", "child": "child-c",
                 "evidence": {"kind": "output_contains", "text": "never returned"}}
            ],
            "children": [
                {"id": "child-b", "peer": "remote-b", "capability": "general",
                 "objective": "delay:5000 never returned b"},
                {"id": "child-c", "peer": "remote-c", "capability": "general",
                 "objective": "delay:5000 never returned c"}
            ]
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    let (status, _) = a.post(
        MASTER_TOKEN,
        &format!("/v1/plans/{id}/dispatch"),
        json!({"wait_ms": 0}),
    );
    assert_eq!(status, reqwest::StatusCode::OK);

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, bindings) = a.get(MASTER_TOKEN, "/v1/delegations");
        let working = bindings
            .as_array()
            .map(|bindings| {
                bindings.iter().filter(|binding| {
                    matches!(
                        binding["state"].as_str(),
                        Some("prepared" | "submitted" | "working")
                    )
                })
            })
            .map(Iterator::count)
            .unwrap_or(0);
        if working == 2 {
            break;
        }
        assert!(Instant::now() < deadline, "both children became active");
        std::thread::sleep(Duration::from_millis(25));
    }

    let (status, canceled) = a.post(MASTER_TOKEN, &format!("/v1/plans/{id}/cancel"), json!({}));
    assert_eq!(status, reqwest::StatusCode::OK, "{canceled}");
    assert_eq!(canceled["state"], "canceled", "{canceled}");
    assert_eq!(child(&canceled, "child-b")["state"], "canceled");
    assert_eq!(child(&canceled, "child-c")["state"], "canceled");

    for (worker, token) in [(&b, B_TOKEN), (&c, C_TOKEN)] {
        let tasks = worker.remote_tasks(token);
        assert_eq!(tasks.len(), 1);
        let id = tasks[0]["id"].as_str().unwrap();
        let (status, task) = worker.a2a(token, "GetTask", json!({"id": id, "historyLength": 10}));
        assert_eq!(status, reqwest::StatusCode::OK, "{task}");
        assert_eq!(task["result"]["status"]["state"], "TASK_STATE_CANCELED");
    }

    // An unrecognized peer is denied before any remote task exists.
    let (status, denied) = a.post(
        MASTER_TOKEN,
        "/v1/plans",
        json!({
            "parent_task_id": parent,
            "objective": "attempt an unauthorized target",
            "criteria": [
                {"id": "ghost", "child": "child-ghost",
                 "evidence": {"kind": "output_contains", "text": "unauthorized objective"}}
            ],
            "children": [
                {"id": "child-ghost", "peer": "not-authorized", "capability": "general",
                 "objective": "unauthorized objective"}
            ]
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{denied}");
    let denied_id = denied["id"].as_str().unwrap().to_string();
    let (status, denied) = a.post(
        MASTER_TOKEN,
        &format!("/v1/plans/{denied_id}/dispatch"),
        json!({"wait_ms": 2000}),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{denied}");
    let denied = a.wait_plan(&denied_id, |plan| {
        child(plan, "child-ghost")["state"] == "rejected"
    });
    assert_eq!(
        child(&denied, "child-ghost")["last_error"]["code"],
        "peer.not_found",
        "{denied}"
    );
    assert_eq!(b.remote_tasks(B_TOKEN).len(), 1);
    assert_eq!(c.remote_tasks(C_TOKEN).len(), 1);
}

#[test]
fn master_plan_does_not_verify_a_child_success_claim() {
    let b_data = tempfile::tempdir().unwrap();
    let b = Worker::start(b_data.path(), "remote-b", B_TOKEN, &[]);
    let a_data = tempfile::tempdir().unwrap();
    let a = Worker::start(
        a_data.path(),
        "master-a",
        MASTER_TOKEN,
        &[peer("remote-b", "remote-b", &b.base, &["general"])],
    );
    let parent = a.submit_parent("claim-parent");
    let (status, created) = a.post(
        MASTER_TOKEN,
        "/v1/plans",
        json!({
            "parent_task_id": parent,
            "objective": "verify required proof, not a success assertion",
            "criteria": [
                {"id": "required-proof", "child": "child-b",
                 "evidence": {"kind": "output_contains", "text": "required proof"}},
                {"id": "exact-artifact", "child": "child-b",
                 "evidence": {"kind": "artifact_digest",
                              "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000"}}
            ],
            "children": [
                {"id": "child-b", "peer": "remote-b", "capability": "general",
                 "objective": "claim success only"}
            ]
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    let (status, _) = a.post(
        MASTER_TOKEN,
        &format!("/v1/plans/{id}/dispatch"),
        json!({"wait_ms": 5000}),
    );
    assert_eq!(status, reqwest::StatusCode::OK);
    let plan = a.wait_plan(&id, |plan| {
        child(plan, "child-b")["state"] == "completed"
            && plan["evaluation"]["outcome"] == "conflicting"
    });
    assert_eq!(child(&plan, "child-b")["state"], "completed", "{plan}");
    assert_eq!(plan["state"], "conflicting", "{plan}");
    assert_eq!(
        plan["evaluation"]["missing"],
        json!(["required-proof"]),
        "{plan}"
    );
    assert_eq!(
        plan["evaluation"]["conflicting"],
        json!(["exact-artifact"]),
        "{plan}"
    );
    assert_ne!(plan["state"], "verified", "{plan}");
}
