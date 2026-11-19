//! Cross-process A2A v1 evidence: durable native task state, idempotency,
//! cancellation, and caller isolation through a real `general serve` process.

use common::Limits;
use runtime::crypto::{issue_delegation, DelegationClaims};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

mod fixture;

const TOKEN: &str = "dev-token";
const OTHER_TOKEN: &str = "other-token";

struct Worker {
    child: Child,
    base: String,
    config: PathBuf,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Worker {
    fn start(data: &Path) -> Self {
        Self::start_named(data, "general-a2a")
    }

    fn start_named(data: &Path, instance: &str) -> Self {
        let fixture = fixture::write(
            data,
            fixture::Options {
                instance: instance.into(),
                token: Some(TOKEN.into()),
                model: "deterministic".into(),
                principals: vec![(OTHER_TOKEN.into(), "local".into(), "bob".into())],
                settings: Vec::new(),
                token_scopes: vec!["*".into()],
            },
        );
        Self::spawn(fixture.config)
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let config = self.config.clone();
        *self = Self::spawn(config);
    }

    fn spawn(config: PathBuf) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
            .args([
                "serve",
                "--config",
                config.to_str().unwrap(),
                "--addr",
                "127.0.0.1:0",
            ])
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
        assert!(base.starts_with("http://127.0.0.1:"), "{line}");
        Self {
            child,
            base,
            config,
        }
    }

    fn client(&self) -> reqwest::blocking::Client {
        reqwest::blocking::Client::new()
    }

    fn rpc(&self, token: &str, method: &str, params: Value) -> (reqwest::StatusCode, Value) {
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
            .expect("a2a responds");
        let status = response.status();
        let body = response.text().expect("a2a body");
        let value = serde_json::from_str(&body).unwrap_or_else(|_| json!({ "raw": body }));
        (status, value)
    }

    fn send(&self, token: &str, message_id: &str, context_id: &str, text: &str) -> Value {
        let (status, body) = self.rpc(
            token,
            "SendMessage",
            json!({
                "message": {
                    "messageId": message_id,
                    "role": "ROLE_USER",
                    "contextId": context_id,
                    "parts": [{"text": text}],
                }
            }),
        );
        assert_eq!(status, reqwest::StatusCode::OK, "{body}");
        body
    }

    fn get_task(&self, token: &str, task_id: &str) -> (reqwest::StatusCode, Value) {
        self.rpc(token, "GetTask", json!({ "id": task_id }))
    }

    fn rpc_version(
        &self,
        token: &str,
        version: &str,
        method: &str,
        params: Value,
    ) -> (reqwest::StatusCode, Value) {
        let response = self
            .client()
            .post(format!("{}/a2a", self.base))
            .bearer_auth(token)
            .header("A2A-Version", version)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            }))
            .send()
            .expect("a2a responds");
        let status = response.status();
        let body = response.text().expect("a2a body");
        let value = serde_json::from_str(&body).unwrap_or_else(|_| json!({ "raw": body }));
        (status, value)
    }

    fn send_stream(
        &self,
        token: &str,
        message_id: &str,
        context_id: &str,
        text: &str,
    ) -> reqwest::blocking::Response {
        self.client()
            .post(format!("{}/a2a", self.base))
            .bearer_auth(token)
            .header("A2A-Version", "1.0")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "SendStreamingMessage",
                "params": {
                    "message": {
                        "messageId": message_id,
                        "role": "ROLE_USER",
                        "contextId": context_id,
                        "parts": [{"text": text}],
                    }
                },
            }))
            .send()
            .expect("a2a stream responds")
    }

    fn subscribe_stream(&self, token: &str, task_id: &str) -> reqwest::blocking::Response {
        self.client()
            .post(format!("{}/a2a", self.base))
            .bearer_auth(token)
            .header("A2A-Version", "1.0")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "SubscribeToTask",
                "params": { "id": task_id },
            }))
            .send()
            .expect("a2a subscribe responds")
    }

    fn card(&self) -> Value {
        self.client()
            .get(format!("{}/.well-known/agent.json", self.base))
            .send()
            .expect("agent card responds")
            .json()
            .expect("agent card is JSON")
    }

    fn wait_state(&self, token: &str, task_id: &str, states: &[&str]) -> Value {
        for _ in 0..400 {
            let (status, body) = self.get_task(token, task_id);
            assert_eq!(status, reqwest::StatusCode::OK, "{body}");
            if let Some(state) = body["result"]["status"]["state"].as_str() {
                if states.contains(&state) {
                    return body;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("task {task_id} never reached {states:?}");
    }
}

fn delegation(instance: &str) -> String {
    issue_delegation(
        b"local-development-delegation-secret",
        &DelegationClaims {
            issuer: "gateway".into(),
            audience: instance.into(),
            principal: "caller".into(),
            namespace: "local".into(),
            scopes: vec!["task:submit".into(), "task:read".into()],
            capabilities: vec!["general".into()],
            expires_at: common::now_ms() + 30_000,
            depth: 0,
            max_depth: 2,
            limits: Limits::default(),
            token_id: "a2a-delegation".into(),
            team: None,
            path: Vec::new(),
        },
    )
    .expect("delegation issues")
}

fn task_state(task: &Value) -> &str {
    task["result"]["status"]["state"]
        .as_str()
        .expect("task state")
}

fn artifact_text(task: &Value) -> String {
    task["result"]["artifacts"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|artifact| artifact["parts"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str())
        .collect()
}

#[test]
fn a2a_submit_survives_process_restart() {
    let data = tempfile::tempdir().unwrap();
    let mut worker = Worker::start(data.path());
    let created = worker.send(TOKEN, "m-restart", "ctx-restart", "hello restart");
    let task_id = created["result"]["id"]
        .as_str()
        .expect("task id")
        .to_string();
    assert_eq!(task_state(&created), "TASK_STATE_COMPLETED");
    assert!(
        artifact_text(&created).contains("echo: hello restart"),
        "{created}"
    );

    worker.restart();

    let (status, restored) = worker.get_task(TOKEN, &task_id);
    assert_eq!(status, reqwest::StatusCode::OK, "{restored}");
    assert_eq!(restored["result"]["id"], task_id);
    assert_eq!(task_state(&restored), "TASK_STATE_COMPLETED");
    assert!(
        artifact_text(&restored).contains("echo: hello restart"),
        "{restored}"
    );

    // A foreign caller cannot read, cancel, or list the task after the restart.
    let (status, foreign) = worker.get_task(OTHER_TOKEN, &task_id);
    assert_eq!(status, reqwest::StatusCode::OK, "{foreign}");
    assert_eq!(foreign["error"]["code"], -32001, "{foreign}");
    assert!(foreign.get("result").is_none(), "{foreign}");

    let (_, foreign_cancel) = worker.rpc(OTHER_TOKEN, "CancelTask", json!({ "id": task_id }));
    assert_eq!(foreign_cancel["error"]["code"], -32001, "{foreign_cancel}");
    let (_, foreign_list) = worker.rpc(OTHER_TOKEN, "ListTasks", json!({}));
    assert_eq!(foreign_list["result"].as_array().unwrap().len(), 0);

    let (_, still) = worker.get_task(TOKEN, &task_id);
    assert_eq!(task_state(&still), "TASK_STATE_COMPLETED", "{still}");

    let capabilities: Value = worker
        .client()
        .get(format!("{}/v1/capabilities", worker.base))
        .bearer_auth(TOKEN)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(capabilities["a2a_task_storage"], "sqlite");
    assert_eq!(capabilities["a2a_durable"], true);
}

fn read_events(response: reqwest::blocking::Response, limit: usize) -> Vec<Value> {
    let mut reader = BufReader::new(response);
    let mut events = Vec::new();
    let mut line = String::new();
    while events.len() < limit {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if let Some(payload) = line.strip_prefix("data: ") {
            events.push(serde_json::from_str(payload.trim()).expect("SSE data is JSON"));
        }
    }
    events
}

#[test]
fn a2a_durable_idempotency_replays_original_task() {
    let data = tempfile::tempdir().unwrap();
    let mut worker = Worker::start(data.path());
    let created = worker.send(TOKEN, "m-duplicate", "ctx-duplicate", "replay me");
    let task_id = created["result"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        created["result"]["metadata"]["goldenbee.invocations"],
        json!(1)
    );

    worker.restart();

    let replay = worker.send(TOKEN, "m-duplicate", "ctx-duplicate", "replay me");
    assert_eq!(replay["result"]["id"], task_id.as_str(), "{replay}");
    assert_eq!(
        replay["result"]["metadata"]["goldenbee.invocations"],
        json!(1),
        "a replayed message id must not run the objective again"
    );
    assert_eq!(artifact_text(&replay), artifact_text(&created));

    let changed = worker.send(TOKEN, "m-duplicate", "ctx-duplicate", "different body");
    assert_eq!(changed["error"]["code"], -32602, "{changed}");
    assert!(changed.get("result").is_none(), "{changed}");
}

#[test]
fn a2a_agent_card_advertises_only_implemented_behavior() {
    let data = tempfile::tempdir().unwrap();
    let worker = Worker::start(data.path());
    let card = worker.card();
    assert_eq!(card["name"], "general");
    assert_eq!(card["supportedInterfaces"][0]["protocolBinding"], "JSONRPC");
    assert_eq!(card["supportedInterfaces"][0]["protocolVersion"], "1.0");
    assert_eq!(
        card["supportedInterfaces"][0]["url"],
        format!("{}/a2a", worker.base)
    );
    assert_eq!(card["capabilities"]["streaming"], true);
    assert_eq!(card["capabilities"]["pushNotifications"], false);
    assert_eq!(card["capabilities"]["extendedAgentCard"], false);
    let rendered = card.to_string();
    assert!(!rendered.contains(TOKEN), "card leaks a token");
    assert!(!rendered.contains(OTHER_TOKEN), "card leaks a token");

    let (_, push) = worker.rpc(
        TOKEN,
        "CreateTaskPushNotificationConfig",
        json!({ "taskId": "any", "url": "https://example.invalid/hook" }),
    );
    assert_eq!(push["error"]["code"], -32003, "{push}");
    let (_, extended) = worker.rpc(TOKEN, "GetExtendedAgentCard", json!({}));
    assert_eq!(extended["error"]["code"], -32007, "{extended}");
}

#[test]
fn a2a_lost_subscription_does_not_resubmit() {
    let data = tempfile::tempdir().unwrap();
    let worker = Worker::start(data.path());
    let response = worker.send_stream(TOKEN, "m-lost", "ctx-lost", "delay:2000 lost subscription");
    let first = read_events(response, 1);
    let task_id = first[0]["result"]["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        first[0]["result"]["task"]["status"]["state"],
        "TASK_STATE_SUBMITTED"
    );

    // The client goes away before completion. Subscribe replays the canonical
    // snapshot; it neither resumes nor resubmits the objective.
    drop(first);
    let snapshot = worker.subscribe_stream(TOKEN, &task_id);
    let snapshots = read_events(snapshot, 2);
    assert_eq!(
        snapshots[0]["result"]["task"]["id"],
        task_id.as_str(),
        "{snapshots:?}"
    );
    assert_eq!(
        snapshots[1]["result"]["statusUpdate"]["taskId"],
        task_id.as_str(),
        "{snapshots:?}"
    );
    let task = worker.wait_state(TOKEN, &task_id, &["TASK_STATE_COMPLETED"]);
    assert_eq!(
        task["result"]["metadata"]["goldenbee.invocations"],
        json!(1)
    );
    assert!(artifact_text(&task).contains("lost subscription"), "{task}");

    let replay = worker.send_stream(TOKEN, "m-lost", "ctx-lost", "delay:2000 lost subscription");
    let again = read_events(replay, 1);
    assert_eq!(
        again[0]["result"]["task"]["id"],
        task_id.as_str(),
        "{again:?}"
    );
    let (_, listed) = worker.rpc(TOKEN, "ListTasks", json!({}));
    assert_eq!(listed["result"].as_array().unwrap().len(), 1, "{listed}");
}

#[test]
fn a2a_cancel_is_retained_and_not_overwritten() {
    let data = tempfile::tempdir().unwrap();
    let worker = Worker::start(data.path());
    let response = worker.send_stream(TOKEN, "m-cancel", "ctx-cancel", "delay:2000 cancel me");
    let events = read_events(response, 2);
    let task_id = events[0]["result"]["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        events.iter().any(|event| event["result"]["statusUpdate"]["status"]["state"]
            == "TASK_STATE_WORKING"),
        "{events:?}"
    );

    let (status, canceled) = worker.rpc(TOKEN, "CancelTask", json!({ "id": task_id }));
    assert_eq!(status, reqwest::StatusCode::OK, "{canceled}");
    assert_eq!(canceled["result"]["status"]["state"], "TASK_STATE_CANCELED");

    std::thread::sleep(std::time::Duration::from_millis(500));
    let (_, after) = worker.get_task(TOKEN, &task_id);
    assert_eq!(
        after["result"]["status"]["state"], "TASK_STATE_CANCELED",
        "{after}"
    );
    assert!(after["result"]["artifacts"].is_null(), "{after}");
}

#[test]
fn a2a_restart_keeps_interruption_truthful() {
    let data = tempfile::tempdir().unwrap();
    let mut worker = Worker::start(data.path());
    let response = worker.send_stream(TOKEN, "m-working", "ctx-working", "delay:3000 interrupted");
    let events = read_events(response, 2);
    let task_id = events[0]["result"]["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    drop(events);

    worker.restart();

    let (status, task) = worker.get_task(TOKEN, &task_id);
    assert_eq!(status, reqwest::StatusCode::OK, "{task}");
    assert_eq!(
        task["result"]["status"]["state"], "TASK_STATE_WORKING",
        "{task}"
    );
    assert_eq!(
        task["result"]["metadata"]["goldenbee.recovery"], "interrupted",
        "{task}"
    );
    // Reading again is safe and does not resubmit the objective.
    let (_, again) = worker.get_task(TOKEN, &task_id);
    assert_eq!(again["result"]["id"], task_id.as_str());
    assert_eq!(
        again["result"]["metadata"]["goldenbee.invocations"],
        json!(1)
    );
}

#[test]
fn a2a_rejects_malformed_and_unsupported_requests_without_creating_tasks() {
    let data = tempfile::tempdir().unwrap();
    let worker = Worker::start(data.path());

    let (_, before) = worker.rpc(TOKEN, "ListTasks", json!({}));
    assert_eq!(before["result"].as_array().unwrap().len(), 0);

    let (_, empty_parts) = worker.rpc(
        TOKEN,
        "SendMessage",
        json!({
            "message": {
                "messageId": "m-empty",
                "role": "ROLE_USER",
                "parts": [],
            }
        }),
    );
    assert_eq!(empty_parts["error"]["code"], -32602, "{empty_parts}");

    let (_, oversized) = worker.rpc(
        TOKEN,
        "SendMessage",
        json!({
            "message": {
                "messageId": "m-oversized",
                "role": "ROLE_USER",
                "parts": [{"text": "hi"}],
                "metadata": {"blob": "x".repeat(70_000)},
            }
        }),
    );
    assert_eq!(oversized["error"]["code"], -32602, "{oversized}");

    let (_, unknown) = worker.rpc(TOKEN, "NotAMethod", json!({}));
    assert_eq!(unknown["error"]["code"], -32601, "{unknown}");

    let (_, empty_id) = worker.rpc(TOKEN, "GetTask", json!({ "id": "" }));
    assert_eq!(empty_id["error"]["code"], -32602, "{empty_id}");

    let (status, version) = worker.rpc_version(
        TOKEN,
        "2.0",
        "SendMessage",
        json!({
            "message": {
                "messageId": "m-version",
                "role": "ROLE_USER",
                "parts": [{"text": "hi"}],
            }
        }),
    );
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{version}");
    assert_eq!(
        version["error"]["details"][0]["reason"], "VERSION_NOT_SUPPORTED",
        "{version}"
    );

    let (_, after) = worker.rpc(TOKEN, "ListTasks", json!({}));
    assert_eq!(
        after["result"].as_array().unwrap().len(),
        0,
        "rejected requests must not create tasks"
    );
}

#[test]
fn a2a_task_stores_are_per_instance() {
    let first_data = tempfile::tempdir().unwrap();
    let second_data = tempfile::tempdir().unwrap();
    let first = Worker::start_named(first_data.path(), "a2a-instance-one");
    let second = Worker::start_named(second_data.path(), "a2a-instance-two");

    let created = first.send(TOKEN, "m-shared", "ctx-shared", "first instance");
    let first_id = created["result"]["id"].as_str().unwrap().to_string();
    let other = second.send(TOKEN, "m-shared", "ctx-shared", "second instance");
    let second_id = other["result"]["id"].as_str().unwrap().to_string();
    assert_ne!(first_id, second_id);

    let (_, foreign) = second.get_task(TOKEN, &first_id);
    assert_eq!(foreign["error"]["code"], -32001, "{foreign}");
    let (_, own) = second.get_task(TOKEN, &second_id);
    assert_eq!(own["result"]["id"], second_id.as_str(), "{own}");
    assert!(artifact_text(&own).contains("second instance"), "{own}");
    assert!(!artifact_text(&own).contains("first instance"), "{own}");
}

#[test]
fn a2a_accepts_gateway_delegation_scoped_to_the_instance() {
    let data = tempfile::tempdir().unwrap();
    let worker = Worker::start(data.path());
    let token = delegation("general-a2a");
    let created = worker.send(
        &token,
        "m-delegated",
        "ctx-delegated",
        "delegated objective",
    );
    assert_eq!(task_state(&created), "TASK_STATE_COMPLETED", "{created}");
    let task_id = created["result"]["id"].as_str().unwrap();
    let (_, restored) = worker.get_task(&token, task_id);
    assert_eq!(restored["result"]["id"], task_id, "{restored}");

    // A delegation for another Worker Instance is not accepted here.
    let wrong = delegation("another-instance");
    let (status, denied) = worker.get_task(&wrong, task_id);
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED, "{denied}");
}
