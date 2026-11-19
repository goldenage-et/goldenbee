//! Cross-process evidence for ticket 18: two independent Worker processes
//! delegate through native A2A, preserve the remote task identity and parent
//! correlation, recover without resubmitting, and cancel the exact child.

use common::delegation::DelegationClaims;
use common::{Limits, SubmitTaskRequest};
use runtime::crypto::issue_delegation;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SECRET: &str = "peer-18-test-delegation-secret";
const A_TOKEN: &str = "peer-a-token";
const B_TOKEN: &str = "peer-b-token";

struct Worker {
    child: Child,
    base: String,
    config: PathBuf,
    data: PathBuf,
    env: Vec<(&'static str, String)>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
        prompts.join("echo.md"),
        "Repeat the user's message. Keep it short.",
    )
    .expect("echo prompt");
    for name in ["general", "echo"] {
        std::fs::write(
            agents.join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\ndescription = \"{name} native ADK agent\"\nmodel = \"deterministic\"\nprompt = \"{name}\"\n"
            ),
        )
        .expect("agent definition");
    }
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

fn spawn(config: PathBuf, data: PathBuf, env: Vec<(&'static str, String)>) -> Worker {
    let mut command = Command::new(env!("CARGO_BIN_EXE_general"));
    command
        .args([
            "serve",
            "--config",
            config.to_str().unwrap(),
            "--addr",
            "127.0.0.1:0",
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
    Worker {
        child,
        base,
        config,
        data,
        env,
    }
}

impl Worker {
    fn start(data: &Path, instance: &str, token: &str, peers: &[PeerSpec]) -> Self {
        let config = write_config(data, instance, token, peers);
        spawn(config, data.to_path_buf(), Vec::new())
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        *self = spawn(self.config.clone(), self.data.clone(), self.env.clone());
    }

    fn wait_exit(&mut self) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().expect("child state") {
                return status.code().unwrap_or(-1);
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

    fn submit_parent(&self, key: &str) -> String {
        let request = SubmitTaskRequest {
            namespace: None,
            agent: "general".into(),
            session_id: format!("parent-{key}"),
            input: "parent objective".into(),
            idempotency_key: key.into(),
            profile: None,
            team: None,
            worker: None,
            limits: Limits::default(),
        };
        let (status, body) =
            self.post(A_TOKEN, "/v1/tasks", serde_json::to_value(request).unwrap());
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

    fn list_tasks(&self, token: &str) -> Vec<Value> {
        let (status, body) = self.a2a(token, "ListTasks", json!({}));
        assert!(status.is_success(), "{body}");
        body["result"].as_array().cloned().unwrap_or_default()
    }
}

fn delegation(instance: &str, token_id: &str, expires_in_ms: i64) -> String {
    issue_delegation(
        SECRET.as_bytes(),
        &DelegationClaims {
            issuer: "test-gateway".into(),
            audience: instance.into(),
            principal: "local".into(),
            namespace: "local".into(),
            scopes: vec!["*".into()],
            capabilities: vec!["general".into(), "echo".into()],
            expires_at: common::now_ms() + expires_in_ms,
            depth: 0,
            max_depth: 3,
            limits: Limits::default(),
            token_id: token_id.into(),
            team: None,
            path: Vec::new(),
        },
    )
    .expect("delegation issues")
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

fn delegate_body(objective: &str, capability: &str, key: &str) -> Value {
    json!({
        "capability": capability,
        "objective": objective,
        "idempotency_key": key,
        "limits": {},
    })
}

#[test]
fn peer_delegation_preserves_remote_task_and_parent_correlation() {
    let a_data = tempfile::tempdir().unwrap();
    let b_data = tempfile::tempdir().unwrap();
    let b = Worker::start(b_data.path(), "peer-b", B_TOKEN, &[]);
    let a = Worker::start(
        a_data.path(),
        "peer-a",
        A_TOKEN,
        &[peer("peer-b", "peer-b", &b.base, &["general"])],
    );
    let parent = a.submit_parent("parent-1");
    let (status, delegated) = a.post(
        A_TOKEN,
        "/v1/peers/peer-b/delegate",
        json!({
            "capability": "general",
            "objective": "remote objective",
            "parent_task_id": parent,
            "idempotency_key": "delegate-1",
            "limits": {},
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{delegated}");
    let remote_id = delegated["remote_task_id"]
        .as_str()
        .expect("remote task id")
        .to_string();
    assert_eq!(delegated["parent_task_id"], parent.as_str());
    assert_eq!(delegated["parent_instance"], "peer-a");
    assert_eq!(delegated["state"], "completed", "{delegated}");
    assert!(
        delegated["output"]
            .as_str()
            .is_some_and(|output| output.contains("remote objective")),
        "{delegated}"
    );

    // The peer record identifies the installed instance and only the effective
    // advertised+authorized capability.
    let (status, peers) = a.get(A_TOKEN, "/v1/peers");
    assert_eq!(status, reqwest::StatusCode::OK, "{peers}");
    let record = &peers.as_array().expect("peer list")[0];
    assert_eq!(record["id"], "peer-b");
    assert_eq!(record["instance"], "peer-b");
    assert_eq!(record["package"], "general");
    assert_eq!(record["capabilities"], json!(["general"]));
    assert_eq!(record["tls"], false);

    // Query the exact remote task through the native A2A surface and prove the
    // parent correlation survived on the child.
    let token = delegation("peer-b", "inspect-1", 60_000);
    let (status, task) = b.a2a(
        &token,
        "GetTask",
        json!({ "id": remote_id, "historyLength": 10 }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{task}");
    assert_eq!(task["result"]["id"], remote_id.as_str());
    assert_eq!(
        task["result"]["metadata"]["goldenbee.parent_task"],
        parent.as_str(),
        "{task}"
    );
    assert_eq!(
        task["result"]["metadata"]["goldenbee.parent_instance"], "peer-a",
        "{task}"
    );
    assert_eq!(
        task["result"]["metadata"]["goldenbee.capability"], "general",
        "{task}"
    );

    // A repeated idempotent request reattaches the same child.
    let (status, replay) = a.post(
        A_TOKEN,
        "/v1/peers/peer-b/delegate",
        json!({
            "capability": "general",
            "objective": "remote objective",
            "parent_task_id": parent,
            "idempotency_key": "delegate-1",
            "limits": {},
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{replay}");
    assert_eq!(replay["remote_task_id"], remote_id.as_str());
    assert_eq!(b.list_tasks(B_TOKEN).len(), 1, "one child invocation");
}

#[test]
fn peer_policy_denies_forbidden_capability_identity_and_replay() {
    let a_data = tempfile::tempdir().unwrap();
    let b_data = tempfile::tempdir().unwrap();
    let b = Worker::start(b_data.path(), "peer-b", B_TOKEN, &[]);
    let mut specs = vec![peer("peer-b", "peer-b", &b.base, &["general"])];
    specs.push(peer("wrong", "not-peer-b", &b.base, &["general"]));
    let a = Worker::start(a_data.path(), "peer-a", A_TOKEN, &specs);

    let (status, denied) = a.post(
        A_TOKEN,
        "/v1/peers/peer-b/delegate",
        delegate_body("forbidden objective", "echo", "forbidden-1"),
    );
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "{denied}");
    assert_eq!(denied["code"], "peer.capability_denied", "{denied}");
    assert!(b.list_tasks(B_TOKEN).is_empty());

    let (status, unknown) = a.post(
        A_TOKEN,
        "/v1/peers/unknown/delegate",
        delegate_body("unknown objective", "general", "unknown-1"),
    );
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{unknown}");
    assert_eq!(unknown["code"], "peer.not_found", "{unknown}");

    let (status, mismatch) = a.post(
        A_TOKEN,
        "/v1/peers/wrong/delegate",
        delegate_body("wrong identity", "general", "wrong-1"),
    );
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "{mismatch}");
    assert_eq!(mismatch["code"], "peer.identity_mismatch", "{mismatch}");
    assert!(b.list_tasks(B_TOKEN).is_empty());

    // A valid delegated token is bound to one objective. Reusing it for a
    // different message id is a durable replay and creates no second task.
    let token = delegation("peer-b", "replay-token", 60_000);
    let message = |id: &str, text: &str| {
        json!({
            "message": {
                "messageId": id,
                "role": "ROLE_USER",
                "parts": [{"text": text}],
                "metadata": {"goldenbee.capability": "general"},
            }
        })
    };
    let (status, first) = b.a2a(&token, "SendMessage", message("replay-message-1", "first"));
    assert_eq!(status, reqwest::StatusCode::OK, "{first}");
    assert!(first.get("result").is_some(), "{first}");
    let (_, replay) = b.a2a(&token, "SendMessage", message("replay-message-2", "second"));
    assert_eq!(replay["error"]["code"], -32602, "{replay}");
    assert!(
        replay["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("delegation.replayed")),
        "{replay}"
    );
    assert_eq!(b.list_tasks(B_TOKEN).len(), 1, "{replay}");
}

#[test]
fn peer_lost_ack_and_restart_reconnect_one_child() {
    let a_data = tempfile::tempdir().unwrap();
    let b_data = tempfile::tempdir().unwrap();
    let b = Worker::start(b_data.path(), "peer-b", B_TOKEN, &[]);
    let fault_dir = tempfile::tempdir().unwrap();
    std::fs::write(fault_dir.path().join("after-peer-send"), b"1").unwrap();
    let a_config = write_config(
        a_data.path(),
        "peer-a",
        A_TOKEN,
        &[peer("peer-b", "peer-b", &b.base, &["general"])],
    );
    let mut a = spawn(
        a_config,
        a_data.path().to_path_buf(),
        vec![(
            "GOLDENBEE_FAULT_DIR",
            fault_dir.path().display().to_string(),
        )],
    );

    let delegated = a
        .client()
        .post(format!("{}/v1/peers/peer-b/delegate", a.base))
        .bearer_auth(A_TOKEN)
        .json(&delegate_body(
            "lost acknowledgment objective",
            "general",
            "lost-ack",
        ))
        .send();
    // The fault intentionally kills the parent after the peer accepted.
    match &delegated {
        Ok(response) => assert!(
            !response.status().is_success(),
            "fault should interrupt the acknowledgment"
        ),
        Err(error) => assert!(error.is_request() || error.is_body(), "{error}"),
    }
    drop(delegated);
    assert_eq!(a.wait_exit(), 75, "fault point exits the parent");
    assert_eq!(b.list_tasks(B_TOKEN).len(), 1);

    a.restart();
    let (status, bindings) = a.get(A_TOKEN, "/v1/delegations");
    assert_eq!(status, reqwest::StatusCode::OK, "{bindings}");
    let bindings = bindings.as_array().expect("delegations");
    assert_eq!(bindings.len(), 1, "{bindings:?}");
    let id = bindings[0]["id"].as_str().unwrap().to_string();
    let (status, recovered) = a.get(A_TOKEN, &format!("/v1/delegations/{id}"));
    assert_eq!(status, reqwest::StatusCode::OK, "{recovered}");
    let remote_id = recovered["remote_task_id"]
        .as_str()
        .expect("recovered remote id")
        .to_string();
    assert_eq!(recovered["state"], "completed", "{recovered}");
    assert!(
        recovered["output"]
            .as_str()
            .is_some_and(|output| output.contains("lost acknowledgment")),
        "{recovered}"
    );
    assert_eq!(b.list_tasks(B_TOKEN).len(), 1, "no resubmission");

    a.restart();
    let (status, again) = a.post(A_TOKEN, &format!("/v1/delegations/{id}/recover"), json!({}));
    assert_eq!(status, reqwest::StatusCode::OK, "{again}");
    assert_eq!(again["remote_task_id"], remote_id.as_str());
    assert_eq!(
        b.list_tasks(B_TOKEN).len(),
        1,
        "one native child invocation"
    );

    // Repeating the original delegation request also reattaches, not resubmits.
    let (status, replay) = a.post(
        A_TOKEN,
        "/v1/peers/peer-b/delegate",
        delegate_body("lost acknowledgment objective", "general", "lost-ack"),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{replay}");
    assert_eq!(replay["remote_task_id"], remote_id.as_str());
    assert_eq!(b.list_tasks(B_TOKEN).len(), 1);
}

#[test]
fn peer_cancels_exact_child_and_reports_unreachable_truthfully() {
    let a_data = tempfile::tempdir().unwrap();
    let b_data = tempfile::tempdir().unwrap();
    let b = Worker::start(b_data.path(), "peer-b", B_TOKEN, &[]);
    let a = Worker::start(
        a_data.path(),
        "peer-a",
        A_TOKEN,
        &[peer("peer-b", "peer-b", &b.base, &["general"])],
    );

    let base = a.base.clone();
    let client = a.client();
    let delegated = std::thread::spawn(move || {
        client
            .post(format!("{base}/v1/peers/peer-b/delegate"))
            .bearer_auth(A_TOKEN)
            .json(&delegate_body(
                "delay:3000 cancel objective",
                "general",
                "cancel-1",
            ))
            .send()
            .map(|response| response.status())
    });
    let binding = {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let (_, bindings) = a.get(A_TOKEN, "/v1/delegations");
            if let Some(binding) = bindings
                .as_array()
                .and_then(|bindings| bindings.first())
                .cloned()
            {
                break binding;
            }
            assert!(Instant::now() < deadline, "delegation binding appeared");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let id = binding["id"].as_str().unwrap().to_string();
    let deadline = Instant::now() + Duration::from_secs(8);
    while b.list_tasks(B_TOKEN).is_empty() {
        assert!(Instant::now() < deadline, "remote task was submitted");
        std::thread::sleep(Duration::from_millis(50));
    }
    let (status, canceled) = a.post(A_TOKEN, &format!("/v1/delegations/{id}/cancel"), json!({}));
    assert_eq!(status, reqwest::StatusCode::OK, "{canceled}");
    assert_eq!(canceled["state"], "canceled", "{canceled}");
    let remote_id = canceled["remote_task_id"]
        .as_str()
        .expect("remote task id after cancel")
        .to_string();
    assert_eq!(
        canceled["remote_state"], "TASK_STATE_CANCELED",
        "{canceled}"
    );
    let _ = delegated.join().expect("delegate thread");

    let token = delegation("peer-b", "inspect-cancel", 60_000);
    let (_, task) = b.a2a(&token, "GetTask", json!({ "id": remote_id }));
    assert_eq!(
        task["result"]["status"]["state"], "TASK_STATE_CANCELED",
        "{task}"
    );

    // Kill the authorized peer. The parent must report unreachable with
    // actionable recovery rather than a fabricated task failure.
    drop(b);
    std::thread::sleep(Duration::from_millis(200));
    let (status, unreachable) = a.post(
        A_TOKEN,
        "/v1/peers/peer-b/delegate",
        delegate_body("unreachable objective", "general", "unreachable-1"),
    );
    assert_eq!(
        status,
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "{unreachable}"
    );
    assert_eq!(unreachable["code"], "peer.unreachable", "{unreachable}");
    assert!(
        unreachable["recovery"]
            .as_str()
            .is_some_and(|recovery| !recovery.is_empty()),
        "{unreachable}"
    );
    assert_eq!(unreachable["retryable"], true, "{unreachable}");
}

#[test]
fn peer_child_authority_never_widens_beyond_the_parent() {
    let a_data = tempfile::tempdir().unwrap();
    let b_data = tempfile::tempdir().unwrap();
    let b = Worker::start(b_data.path(), "peer-b", B_TOKEN, &[]);
    let a = Worker::start(
        a_data.path(),
        "peer-a",
        A_TOKEN,
        &[peer("peer-b", "peer-b", &b.base, &["general"])],
    );
    let delegated = a.post(
        A_TOKEN,
        "/v1/peers/peer-b/delegate",
        json!({
            "capability": "general",
            "objective": "narrowed objective",
            "idempotency_key": "narrowed-1",
            "limits": {
                "max_depth": 9,
                "max_fanout": 9,
                "max_tokens": 1_000,
                "max_model_calls": 2
            }
        }),
    );
    assert_eq!(delegated.0, reqwest::StatusCode::OK, "{delegated:?}");
    assert!(
        delegated.1["max_depth"].as_u64().unwrap() <= 2,
        "{delegated:?}"
    );
    assert!(
        delegated.1["limits"]["max_fanout"].as_u64().unwrap() <= 2,
        "{delegated:?}"
    );
    assert!(
        delegated.1["limits"]["max_tokens"].as_u64().unwrap() <= 1_000,
        "{delegated:?}"
    );
    assert_eq!(delegated.1["capabilities"], json!(["general"]));

    // A parent delegation token near its expiry cannot yield a broader child.
    let token = delegation("peer-a", "parent-near-expiry", 60_000);
    let (status, delegated) = a
        .client()
        .post(format!("{}/v1/peers/peer-b/delegate", a.base))
        .bearer_auth(&token)
        .json(&delegate_body(
            "delegated parent objective",
            "general",
            "parent-1",
        ))
        .send()
        .map(|response| {
            let status = response.status();
            let body: Value = response.json().unwrap_or_default();
            (status, body)
        })
        .expect("delegated parent responds");
    assert_eq!(status, reqwest::StatusCode::OK, "{delegated}");
    let authority_expiry = delegated["authority_expires_at"].as_i64().unwrap();
    let parent_expiry = common::now_ms() + 60_000;
    assert!(authority_expiry <= parent_expiry, "{delegated}");
    assert_eq!(delegated["capabilities"], json!(["general"]));

    // An expired parent token is rejected before a child is prepared.
    let expired = delegation("peer-a", "expired-parent", -1_000);
    let before = b.list_tasks(B_TOKEN).len();
    let (status, denied) = a.post(A_TOKEN, "/v1/peers/peer-b/resolve", json!({}));
    assert_eq!(status, reqwest::StatusCode::OK, "{denied}");
    let response = a
        .client()
        .post(format!("{}/v1/peers/peer-b/delegate", a.base))
        .bearer_auth(&expired)
        .json(&delegate_body("expired objective", "general", "expired-1"))
        .send()
        .expect("expired request responds");
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(b.list_tasks(B_TOKEN).len(), before);
}
