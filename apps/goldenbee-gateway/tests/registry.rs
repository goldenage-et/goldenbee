//! Ticket 19 cross-process proof: authenticated registration, leases,
//! reconciliation, filtered discovery, and Gateway restart persistence.
//!
//! The Workers and Gateway are real child processes. Registration and renewal
//! travel over HTTP with Worker control tokens; every descriptor is verified
//! against the live Worker management routes.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

const DELEGATION_SECRET: &str = "t19-shared-delegation-secret";
const ADMIN_TOKEN: &str = "t19-admin-token";
const CALLER_A_TOKEN: &str = "t19-caller-a-token";
const CALLER_B_TOKEN: &str = "t19-caller-b-token";
const CONTROL_A: &str = "t19-control-a";
const CONTROL_B: &str = "t19-control-b";
const CONTROL_DUP: &str = "t19-control-dup";

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
    panic!("{name} is missing; build workspace binaries first");
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::new()
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
    listener.local_addr().expect("ephemeral address").port()
}

struct Worker {
    child: Child,
    base: String,
    instance: String,
    incarnation: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn worker_config(data: &Path, token: &str) -> PathBuf {
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
    std::fs::write(
        agents.join("general.toml"),
        "name = \"general\"\ndescription = \"General Worker\"\nmodel = \"deterministic\"\nprompt = \"general\"\ntools = [\"exec_command\"]\n",
    )
    .expect("general agent");
    std::fs::write(
        agents.join("echo.toml"),
        "name = \"echo\"\ndescription = \"Echo Worker\"\nmodel = \"deterministic\"\nprompt = \"echo\"\n",
    )
    .expect("echo agent");
    let config = format!(
        "instance = \"instance-{token}\"\nnamespace = \"local\"\ndefault_agent = \"general\"\ntoken = \"{token}\"\ntoken_scopes = [\"*\"]\n\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n",
        data.display()
    );
    let path = data.join("worker.toml");
    std::fs::write(&path, config).expect("worker config");
    path
}

impl Worker {
    fn spawn(port: Option<u16>, data: &Path, token: &str) -> Self {
        let address = match port {
            Some(port) => format!("127.0.0.1:{port}"),
            None => "127.0.0.1:0".to_string(),
        };
        let config = worker_config(data, token);
        let mut child = Command::new(binary("general"))
            .args([
                "serve",
                "--config",
                config.to_str().expect("config path"),
                "--addr",
                &address,
            ])
            .env("GOLDENBEE_DELEGATION_SECRET", DELEGATION_SECRET)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("general worker starts");
        let stdout = child.stdout.take().expect("worker stdout is piped");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("worker announces its listener");
        let base = line
            .split("listening on ")
            .nth(1)
            .unwrap_or_else(|| panic!("worker address announcement: {line}"))
            .trim()
            .to_string();
        let health = wait_health(&base);
        Self {
            child,
            base,
            instance: health["instance"].as_str().expect("instance").to_string(),
            incarnation: health["incarnation"]
                .as_str()
                .expect("incarnation")
                .to_string(),
        }
    }

    fn health(&self) -> Value {
        wait_health(&self.base)
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_health(base: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(response) = client().get(format!("{base}/v1/health")).send() {
            if let Ok(value) = response.json::<Value>() {
                return value;
            }
        }
        assert!(
            Instant::now() < deadline,
            "worker health at {base} did not respond"
        );
        sleep(Duration::from_millis(20));
    }
}

fn spawn_gateway(config: &Path) -> (Child, BufReader<ChildStdout>, String) {
    let mut child = Command::new(binary("gb-gateway"))
        .args([
            "serve",
            "--addr",
            "127.0.0.1:0",
            "--config",
            config.to_str().expect("configuration path"),
        ])
        .env("GOLDENBEE_DELEGATION_SECRET", DELEGATION_SECRET)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("gateway starts");
    let stdout = child.stdout.take().expect("gateway stdout is piped");
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
    (child, reader, base)
}

struct Gateway {
    child: Child,
    _stdout: BufReader<ChildStdout>,
    base: String,
    config: PathBuf,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Gateway {
    fn start(config: &Path) -> Self {
        let (child, stdout, base) = spawn_gateway(config);
        Self {
            child,
            _stdout: stdout,
            base,
            config: config.to_path_buf(),
        }
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (child, stdout, base) = spawn_gateway(&self.config);
        self.child = child;
        self._stdout = stdout;
        self.base = base;
    }

    fn register(
        &self,
        control: Option<&str>,
        endpoint: &str,
        incarnation: &str,
    ) -> reqwest::blocking::Response {
        let request = client()
            .post(format!("{}/v1/workers/register", self.base))
            .json(&json!({
                "endpoint": endpoint,
                "incarnation": incarnation,
                "capacity": {"active": 0, "limit": 4, "queued": 0, "queue_limit": 16}
            }));
        match control {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
        .send()
        .expect("registration responds")
    }

    fn renew(
        &self,
        control: &str,
        worker: &str,
        incarnation: &str,
        capacity: Value,
    ) -> reqwest::blocking::Response {
        client()
            .post(format!("{}/v1/workers/{worker}/lease", self.base))
            .bearer_auth(control)
            .json(&json!({"incarnation": incarnation, "capacity": capacity}))
            .send()
            .expect("renewal responds")
    }

    fn discover(&self, token: &str, query: &str) -> Vec<Value> {
        let response = client()
            .get(format!("{}/v1/workers{query}", self.base))
            .bearer_auth(token)
            .send()
            .expect("discovery responds");
        assert!(
            response.status().is_success(),
            "discovery failed: {}",
            response.status()
        );
        response.json::<Vec<Value>>().expect("discovery is JSON")
    }

    fn audit(&self, token: &str) -> Vec<Value> {
        let response = client()
            .get(format!("{}/v1/workers/audit", self.base))
            .bearer_auth(token)
            .send()
            .expect("audit responds");
        assert!(
            response.status().is_success(),
            "audit failed: {}",
            response.status()
        );
        response.json::<Vec<Value>>().expect("audit is JSON")
    }

    fn submit(&self, token: &str, worker: &str, key: &str) -> Value {
        let response = client()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(token)
            .json(&json!({
                "worker": worker,
                "agent": "general",
                "session_id": format!("t19-{key}"),
                "input": "lease and registration evidence",
                "idempotency_key": key
            }))
            .send()
            .expect("submit responds");
        assert!(
            response.status().is_success(),
            "submit failed: {}",
            response.status()
        );
        response.json().expect("submit is JSON")
    }

    fn wait_task(&self, token: &str, id: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let response = client()
                .get(format!("{}/v1/tasks/{id}", self.base))
                .bearer_auth(token)
                .send()
                .expect("task responds");
            assert!(
                response.status().is_success(),
                "task failed: {}",
                response.status()
            );
            let projection: Value = response.json().expect("task is JSON");
            if matches!(
                projection["task"]["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled" | "unknown")
            ) {
                return projection;
            }
            assert!(Instant::now() < deadline, "task {id} did not finish");
            sleep(Duration::from_millis(20));
        }
    }

    fn workers(&self, token: &str) -> reqwest::blocking::Response {
        client()
            .get(format!("{}/v1/workers", self.base))
            .bearer_auth(token)
            .send()
            .expect("worker discovery responds")
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    config: PathBuf,
    registry: PathBuf,
}

fn fixture(workers: &[(&str, &str, &str, &str)], lease_ttl_ms: Option<i64>) -> Fixture {
    let directory = tempfile::tempdir().expect("fixture directory");
    let config = directory.path().join("gateway.json");
    let registry = directory.path().join("registry.json");
    let mut documents = Vec::new();
    for (id, endpoint, instance, control) in workers {
        documents.push(json!({
            "id": id,
            "endpoint": endpoint,
            "instance": instance,
            "package": "general",
            "capabilities": ["general", "echo"],
            "control_token": control,
            "preflight": *id != "inst-dup"
        }));
    }
    let mut document = json!({
        "listen": "127.0.0.1:0",
        "registry": registry.display().to_string(),
        "callers": [
            {
                "token": ADMIN_TOKEN,
                "id": "admin-1",
                "namespace": "local",
                "scopes": ["admin", "task:submit", "task:read", "task:cancel"]
            },
            {
                "token": CALLER_A_TOKEN,
                "id": "caller-a",
                "namespace": "local",
                "scopes": ["task:submit", "task:read"],
                "workers": ["inst-a"]
            },
            {
                "token": CALLER_B_TOKEN,
                "id": "caller-b",
                "namespace": "local",
                "scopes": ["task:read"],
                "workers": ["inst-b"]
            }
        ],
        "workers": documents
    });
    if let Some(ttl) = lease_ttl_ms {
        document["lease_ttl_ms"] = json!(ttl);
    }
    std::fs::write(
        &config,
        serde_json::to_vec(&document).expect("configuration JSON"),
    )
    .expect("configuration is written");
    Fixture {
        _directory: directory,
        config,
        registry,
    }
}

fn ids(workers: &[Value]) -> Vec<String> {
    workers
        .iter()
        .filter_map(|worker| worker["id"].as_str().map(str::to_string))
        .collect()
}

fn status(response: &reqwest::blocking::Response) -> u16 {
    response.status().as_u16()
}

fn json(response: reqwest::blocking::Response) -> Value {
    response.json().expect("response is JSON")
}

#[test]
fn authenticated_registration_renewal_and_filtered_discovery() {
    let data_a = tempfile::tempdir().expect("worker a data");
    let data_b = tempfile::tempdir().expect("worker b data");
    let worker_a = Worker::spawn(None, data_a.path(), "t19-worker-a-token");
    let worker_b = Worker::spawn(None, data_b.path(), "t19-worker-b-token");
    let fixture = fixture(
        &[
            ("inst-a", &worker_a.base, &worker_a.instance, CONTROL_A),
            ("inst-b", &worker_b.base, &worker_b.instance, CONTROL_B),
        ],
        Some(60_000),
    );
    let gateway = Gateway::start(&fixture.config);

    assert_eq!(
        status(&gateway.register(None, &worker_a.base, &worker_a.incarnation)),
        401,
        "unauthenticated registration is refused"
    );

    let registered = gateway.register(Some(CONTROL_A), &worker_a.base, &worker_a.incarnation);
    assert_eq!(
        status(&registered),
        200,
        "authenticated registration accepted"
    );
    let lease = json(registered);
    assert_eq!(lease["worker"], "inst-a");
    assert_eq!(lease["instance"], worker_a.instance);
    assert_eq!(lease["incarnation"], worker_a.incarnation);
    assert_eq!(lease["package"], "general");
    assert_eq!(lease["protocol"], "1.0");
    assert_eq!(lease["status"], "registered");
    assert_eq!(lease["approved_capabilities"], json!(["general", "echo"]));
    assert_eq!(
        lease["declared_capabilities"].as_array().map(Vec::len),
        Some(3),
        "the verified package card is retained separately"
    );
    let capabilities: Vec<String> = lease["capabilities"]
        .as_array()
        .expect("capabilities")
        .iter()
        .filter_map(|capability| capability["id"].as_str().map(str::to_string))
        .collect();
    assert_eq!(capabilities, vec!["echo", "general"]);

    let registered = gateway.register(Some(CONTROL_B), &worker_b.base, &worker_b.incarnation);
    assert_eq!(status(&registered), 200);

    let visible = gateway.discover(
        CALLER_A_TOKEN,
        "?capability=echo&protocol=1.0&locality=local&health=healthy",
    );
    assert_eq!(ids(&visible), vec!["inst-a"]);
    assert_eq!(visible[0]["locality"], "local");
    assert_eq!(visible[0]["instance"], worker_a.instance);
    assert_eq!(visible[0]["package"], "general");
    assert_eq!(visible[0]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        ids(&gateway.discover(CALLER_B_TOKEN, "?locality=local&health=healthy")),
        vec!["inst-b"]
    );
    assert!(
        gateway
            .discover(CALLER_A_TOKEN, "?capability=missing")
            .is_empty(),
        "capability filter excludes non-matching Workers"
    );
    assert!(
        gateway.discover(CALLER_A_TOKEN, "?protocol=9.9").is_empty(),
        "protocol filter excludes non-matching Workers"
    );

    let renewed = gateway.renew(
        CONTROL_A,
        "inst-a",
        &worker_a.incarnation,
        json!({"active": 2, "limit": 4, "queued": 1, "queue_limit": 8}),
    );
    assert_eq!(status(&renewed), 200);
    let lease = json(renewed);
    assert_eq!(lease["capacity"]["active"], 2);
    assert_eq!(lease["capacity"]["limit"], 4);

    let visible = gateway.discover(CALLER_A_TOKEN, "?capability=echo&health=healthy");
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0]["capacity"]["active"], 2);
    assert_eq!(visible[0]["capacity"]["queued"], 1);
    assert_eq!(
        visible[0]["capabilities"].as_array().map(Vec::len),
        Some(2),
        "live capacity does not rewrite the static card"
    );

    let cross_identity = gateway.renew(
        CONTROL_B,
        "inst-a",
        &worker_a.incarnation,
        json!({"active": 0, "limit": 1, "queued": 0, "queue_limit": 1}),
    );
    assert_eq!(
        status(&cross_identity),
        403,
        "a control token cannot renew another Worker identity"
    );

    let cli = Command::new(binary("gb-client"))
        .args([
            "--gateway",
            &gateway.base,
            "--token",
            CALLER_A_TOKEN,
            "workers",
            "--capability",
            "echo",
            "--protocol",
            "1.0",
            "--locality",
            "local",
            "--health",
            "healthy",
            "--json",
        ])
        .output()
        .expect("cli runs");
    assert!(
        cli.status.success(),
        "cli discovery failed: {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let discovered: Value = serde_json::from_slice(&cli.stdout).expect("cli discovery is JSON");
    let discovered = discovered.as_array().expect("cli discovery list");
    assert_eq!(ids(discovered), vec!["inst-a"]);
    assert_eq!(discovered[0]["capacity"]["active"], 2);
    assert_eq!(discovered[0]["instance"], worker_a.instance);
    assert_eq!(discovered[0]["package"], "general");
}

#[test]
fn duplicate_spoofed_and_forbidden_registrations_are_rejected_and_audited() {
    let data_a = tempfile::tempdir().expect("worker a data");
    let data_b = tempfile::tempdir().expect("worker b data");
    let worker_a = Worker::spawn(None, data_a.path(), "t19-worker-a-token");
    let worker_b = Worker::spawn(None, data_b.path(), "t19-worker-b-token");
    let fixture = fixture(
        &[
            ("inst-a", &worker_a.base, &worker_a.instance, CONTROL_A),
            ("inst-b", &worker_b.base, &worker_b.instance, CONTROL_B),
            ("inst-dup", &worker_a.base, &worker_a.instance, CONTROL_DUP),
        ],
        Some(60_000),
    );
    let gateway = Gateway::start(&fixture.config);

    assert_eq!(
        status(&gateway.register(Some(CONTROL_A), &worker_a.base, &worker_a.incarnation)),
        200
    );

    let changed = gateway.register(Some(CONTROL_A), &worker_b.base, &worker_b.incarnation);
    assert_eq!(status(&changed), 403, "endpoint change is forbidden");
    assert_eq!(json(changed)["code"], "worker.endpoint_forbidden");

    let spoofed = gateway.register(Some(CONTROL_A), &worker_a.base, &worker_b.incarnation);
    assert_eq!(status(&spoofed), 403, "incarnation spoofing is rejected");
    assert_eq!(json(spoofed)["code"], "worker.incarnation_mismatch");

    let duplicate = gateway.register(Some(CONTROL_DUP), &worker_a.base, &worker_a.incarnation);
    assert_eq!(status(&duplicate), 409, "duplicate identity is rejected");
    assert_eq!(json(duplicate)["code"], "worker.duplicate_identity");

    let audit = gateway.audit(ADMIN_TOKEN);
    let denied: Vec<&Value> = audit
        .iter()
        .filter(|entry| entry["outcome"] == "denied")
        .collect();
    assert!(
        denied.iter().any(|entry| entry["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("endpoint"))),
        "forbidden endpoint change is audited: {audit:?}"
    );
    assert!(
        denied.iter().any(|entry| entry["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("incarnation"))),
        "spoofed incarnation is audited: {audit:?}"
    );
    assert!(
        audit
            .iter()
            .any(|entry| entry["action"] == "worker.register"
                && entry["worker"] == "inst-a"
                && entry["outcome"] == "accepted"),
        "accepted registration is audited"
    );
    assert!(
        audit
            .iter()
            .all(|entry| !entry.to_string().contains(CONTROL_A)),
        "audit never exposes control tokens"
    );

    assert_eq!(
        status(&gateway.workers(CALLER_A_TOKEN)),
        200,
        "caller discovery remains authenticated"
    );
}

#[test]
fn decommission_requires_admin_and_is_audited() {
    let data_a = tempfile::tempdir().expect("worker a data");
    let worker_a = Worker::spawn(None, data_a.path(), "t19-worker-a-token");
    let fixture = fixture(
        &[("inst-a", &worker_a.base, &worker_a.instance, CONTROL_A)],
        Some(60_000),
    );
    let gateway = Gateway::start(&fixture.config);
    assert_eq!(
        status(&gateway.register(Some(CONTROL_A), &worker_a.base, &worker_a.incarnation)),
        200
    );

    let denied = client()
        .delete(format!("{}/v1/workers/inst-a", gateway.base))
        .bearer_auth(CALLER_A_TOKEN)
        .send()
        .expect("decommission responds");
    assert_eq!(status(&denied), 403, "unauthorized decommission is refused");
    assert_eq!(json(denied)["code"], "scope.denied");

    let accepted = client()
        .delete(format!("{}/v1/workers/inst-a", gateway.base))
        .bearer_auth(ADMIN_TOKEN)
        .send()
        .expect("decommission responds");
    assert_eq!(status(&accepted), 200);
    assert_eq!(json(accepted)["status"], "decommissioned");

    assert!(
        gateway.discover(ADMIN_TOKEN, "").is_empty(),
        "decommissioned identities are not discoverable"
    );
    assert_eq!(
        status(&gateway.register(Some(CONTROL_A), &worker_a.base, &worker_a.incarnation)),
        409,
        "a decommissioned identity cannot silently re-register"
    );

    let audit = gateway.audit(ADMIN_TOKEN);
    assert!(audit.iter().any(|entry| {
        entry["action"] == "worker.decommission"
            && entry["outcome"] == "denied"
            && entry["principal"] == "caller-a"
    }));
    assert!(audit.iter().any(|entry| {
        entry["action"] == "worker.decommission"
            && entry["outcome"] == "accepted"
            && entry["principal"] == "admin-1"
    }));
}

#[test]
fn lease_expiry_marks_unreachable_without_rewriting_task_outcomes() {
    let data_a = tempfile::tempdir().expect("worker a data");
    let worker_a = Worker::spawn(None, data_a.path(), "t19-worker-a-token");
    let fixture = fixture(
        &[("inst-a", &worker_a.base, &worker_a.instance, CONTROL_A)],
        Some(1_500),
    );
    let gateway = Gateway::start(&fixture.config);
    assert_eq!(
        status(&gateway.register(Some(CONTROL_A), &worker_a.base, &worker_a.incarnation)),
        200
    );

    let submitted = gateway.submit(CALLER_A_TOKEN, "inst-a", "expiry-outcome");
    let task_id = submitted["task_id"].as_str().expect("task id").to_string();
    let completed = gateway.wait_task(CALLER_A_TOKEN, &task_id);
    assert_eq!(completed["task"]["state"], "succeeded");
    let output = completed["task"]["output"].clone();

    let deadline = Instant::now() + Duration::from_secs(10);
    let unreachable = loop {
        let unreachable = gateway.discover(CALLER_A_TOKEN, "?health=unreachable");
        let healthy = gateway.discover(CALLER_A_TOKEN, "?health=healthy");
        if ids(&unreachable) == vec!["inst-a"] && healthy.is_empty() {
            break unreachable;
        }
        assert!(
            Instant::now() < deadline,
            "lease did not expire; healthy={healthy:?} unreachable={unreachable:?}"
        );
        sleep(Duration::from_millis(25));
    };
    assert_eq!(unreachable[0]["status"], "unreachable");
    assert_eq!(unreachable[0]["reachable"], false);

    let after = gateway.wait_task(CALLER_A_TOKEN, &task_id);
    assert_eq!(after["task"]["state"], "succeeded");
    assert_eq!(after["task"]["output"], output);
    assert_eq!(after["worker_incarnation"], worker_a.incarnation);
    assert!(
        gateway.audit(ADMIN_TOKEN).iter().any(|entry| {
            entry["action"] == "worker.lease_expired" && entry["worker"] == "inst-a"
        }),
        "expiry is audited"
    );

    let renewed = gateway.renew(
        CONTROL_A,
        "inst-a",
        &worker_a.incarnation,
        json!({"active": 0, "limit": 2, "queued": 0, "queue_limit": 4}),
    );
    assert_eq!(status(&renewed), 200, "renewal restores reachability");
    assert_eq!(
        ids(&gateway.discover(CALLER_A_TOKEN, "?health=healthy")),
        vec!["inst-a"]
    );
}

#[test]
fn restart_preserves_registry_and_new_incarnation_keeps_historical_references() {
    let data = tempfile::tempdir().expect("worker data");
    let port = free_port();
    let mut worker = Worker::spawn(Some(port), data.path(), "t19-worker-a-token");
    let fixture = fixture(
        &[("inst-a", &worker.base, &worker.instance, CONTROL_A)],
        Some(60_000),
    );
    let mut gateway = Gateway::start(&fixture.config);
    assert_eq!(
        status(&gateway.register(Some(CONTROL_A), &worker.base, &worker.incarnation)),
        200
    );

    let submitted = gateway.submit(CALLER_A_TOKEN, "inst-a", "restart-history");
    let task_id = submitted["task_id"].as_str().expect("task id").to_string();
    let completed = gateway.wait_task(CALLER_A_TOKEN, &task_id);
    assert_eq!(completed["task"]["state"], "succeeded");
    let old_incarnation = worker.incarnation.clone();
    assert_eq!(completed["worker_incarnation"], old_incarnation);
    let output = completed["task"]["output"].clone();

    worker.stop();
    sleep(Duration::from_millis(100));
    worker = Worker::spawn(Some(port), data.path(), "t19-worker-a-token");
    assert_ne!(
        worker.incarnation, old_incarnation,
        "restart changes incarnation"
    );
    let health = worker.health();
    assert_eq!(
        health["instance"], worker.instance,
        "instance identity is stable"
    );

    let renewed = gateway.renew(
        CONTROL_A,
        "inst-a",
        &worker.incarnation,
        json!({"active": 1, "limit": 4, "queued": 0, "queue_limit": 8}),
    );
    let renewed_status = status(&renewed);
    let lease = json(renewed);
    assert_eq!(renewed_status, 200, "new incarnation reconnects: {lease:?}");
    assert_eq!(lease["incarnation"], worker.incarnation);
    let history = lease["incarnations"]
        .as_array()
        .expect("incarnation history");
    assert!(
        history
            .iter()
            .any(|entry| entry["incarnation"] == old_incarnation
                && entry["superseded_at"].is_number())
    );
    assert!(history
        .iter()
        .any(|entry| entry["incarnation"] == worker.incarnation));

    let preserved = gateway.wait_task(CALLER_A_TOKEN, &task_id);
    assert_eq!(preserved["task"]["state"], "succeeded");
    assert_eq!(preserved["task"]["output"], output);
    assert_eq!(
        preserved["worker_incarnation"], old_incarnation,
        "routed historical reference is not rewritten by reconciliation"
    );

    let visible = gateway.discover(ADMIN_TOKEN, "?health=healthy");
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0]["incarnation"], worker.incarnation);
    assert_eq!(visible[0]["instance"], worker.instance);

    gateway.restart();
    let after_restart = gateway.discover(ADMIN_TOKEN, "");
    assert_eq!(
        ids(&after_restart),
        vec!["inst-a"],
        "registry state survives a Gateway restart"
    );
    assert_eq!(after_restart[0]["incarnation"], worker.incarnation);
    assert_eq!(after_restart[0]["capacity"]["active"], 1);
    assert_eq!(after_restart[0]["package"], "general");
    assert!(fixture.registry.exists(), "registry state file is durable");

    let audit = gateway.audit(ADMIN_TOKEN);
    assert!(audit
        .iter()
        .any(|entry| entry["action"] == "worker.incarnation_reconciled"));
}
