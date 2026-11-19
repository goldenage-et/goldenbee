//! Cross-process artifact evidence: native finalization, authenticated
//! download, scoped grants, isolation, restart, tampering, and interruption.

use common::{Limits, SubmitTaskRequest};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

mod fixture;

struct Worker {
    child: Child,
    data: tempfile::TempDir,
    base: String,
    instance: String,
    incarnation: String,
    token: String,
    principals: Vec<(String, String, String)>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_worker(
    data: &Path,
    token: &str,
    principals: &[(String, String, String)],
) -> (Child, String, String, String) {
    let fixture = fixture::write(
        data,
        fixture::Options {
            instance: "general-artifact".into(),
            token: Some(token.to_string()),
            model: "deterministic".into(),
            principals: principals.to_vec(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_general"));
    command
        .args([
            "serve",
            "--config",
            fixture.config.to_str().expect("fixture config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
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
    let health = reqwest::blocking::Client::new()
        .get(format!("{base}/v1/health"))
        .send()
        .expect("health responds")
        .json::<Value>()
        .expect("health is JSON");
    let instance = health["instance"]
        .as_str()
        .expect("instance identity")
        .to_string();
    let incarnation = health["incarnation"]
        .as_str()
        .expect("incarnation identity")
        .to_string();
    (child, base, instance, incarnation)
}

fn request(session: &str, input: &str, key: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: None,
        agent: "general".into(),
        session_id: session.into(),
        input: input.into(),
        idempotency_key: key.into(),
        profile: None,
        team: None,
        worker: None,
        limits: Limits::default(),
    }
}

impl Worker {
    fn start(token: &str) -> Self {
        Self::start_with(token, &[])
    }

    fn start_with(token: &str, principals: &[(&str, &str, &str)]) -> Self {
        let data = tempfile::tempdir().expect("temporary data directory");
        let principals: Vec<(String, String, String)> = principals
            .iter()
            .map(|(token, namespace, principal)| {
                (
                    (*token).to_string(),
                    (*namespace).to_string(),
                    (*principal).to_string(),
                )
            })
            .collect();
        let (child, base, instance, incarnation) = spawn_worker(data.path(), token, &principals);
        Self {
            child,
            data,
            base,
            instance,
            incarnation,
            token: token.to_string(),
            principals,
        }
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (child, base, instance, incarnation) =
            spawn_worker(self.data.path(), &self.token, &self.principals);
        self.child = child;
        self.base = base;
        self.instance = instance;
        self.incarnation = incarnation;
    }

    fn client(&self) -> reqwest::blocking::Client {
        reqwest::blocking::Client::new()
    }

    fn submit(&self, session: &str, input: &str, key: &str) -> Value {
        let response = self
            .client()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(&self.token)
            .json(&request(session, input, key))
            .send()
            .expect("submit responds");
        let status = response.status();
        let body = response.text().expect("submit body");
        assert!(status.is_success(), "submit failed: {status} {body}");
        serde_json::from_str(&body).expect("submit is JSON")
    }

    fn wait(&self, id: &str) -> Value {
        for _ in 0..500 {
            let task = self
                .client()
                .get(format!("{}/v1/tasks/{id}", self.base))
                .bearer_auth(&self.token)
                .send()
                .expect("task responds")
                .json::<Value>()
                .expect("task is JSON");
            if matches!(
                task["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled" | "unknown")
            ) {
                return task;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("task did not reach a terminal state");
    }

    fn run(&self, session: &str, input: &str, key: &str) -> Value {
        let submitted = self.submit(session, input, key);
        self.wait(submitted["task_id"].as_str().expect("task id"))
    }

    fn artifacts(&self, token: &str, task_id: &str) -> Value {
        let response = self
            .client()
            .get(format!("{}/v1/tasks/{task_id}/artifacts", self.base))
            .bearer_auth(token)
            .send()
            .expect("artifact list responds");
        let status = response.status();
        let body = response.text().expect("artifact list body");
        assert!(status.is_success(), "artifact list failed: {status} {body}");
        serde_json::from_str(&body).expect("artifact list is JSON")
    }

    fn download(&self, token: Option<&str>, artifact_id: &str) -> reqwest::blocking::Response {
        let mut request = self
            .client()
            .get(format!("{}/v1/artifacts/{artifact_id}/content", self.base));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().expect("artifact download responds")
    }

    fn grant(&self, artifact_id: &str, ttl_ms: i64) -> Value {
        self.client()
            .post(format!("{}/v1/artifacts/{artifact_id}/grant", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({"ttl_ms": ttl_ms}))
            .send()
            .expect("grant responds")
            .json()
            .expect("grant is JSON")
    }

    fn grant_download(&self, grant: &str) -> reqwest::blocking::Response {
        self.client()
            .get(format!("{}/v1/artifact-grants/{grant}", self.base))
            .send()
            .expect("grant download responds")
    }
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn artifact_versions_are_immutable_and_keep_their_own_provenance() {
    let worker = Worker::start("artifact-token");
    let first = worker.run("version-session", "artifact:first version", "version-key-1");
    let second = worker.run(
        "version-session",
        "artifact:second version",
        "version-key-2",
    );
    let first_id = first["id"].as_str().expect("first task");
    let second_id = second["id"].as_str().expect("second task");
    let first_artifact = worker
        .artifacts("artifact-token", first_id)
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .expect("first artifact");
    let second_artifact = worker
        .artifacts("artifact-token", second_id)
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .expect("second artifact");
    assert_eq!(first_artifact["version"], 1);
    assert_eq!(second_artifact["version"], 2);
    assert_ne!(first_artifact["id"], second_artifact["id"]);
    assert_ne!(first_artifact["digest"], second_artifact["digest"]);
    assert_eq!(first_artifact["task_id"], first_id);
    assert_eq!(second_artifact["task_id"], second_id);
    assert_ne!(
        first_artifact["native_invocation_id"],
        second_artifact["native_invocation_id"]
    );
    assert_eq!(
        worker
            .download(
                Some("artifact-token"),
                first_artifact["id"].as_str().expect("first id")
            )
            .text()
            .expect("first content"),
        "first version"
    );
    assert_eq!(
        worker
            .download(
                Some("artifact-token"),
                second_artifact["id"].as_str().expect("second id")
            )
            .text()
            .expect("second content"),
        "second version"
    );
}

#[test]
fn artifact_versions_survive_restart_and_do_not_cross_instances() {
    let mut worker = Worker::start("artifact-token");
    let task = worker.run(
        "restart-session",
        "artifact:durable evidence",
        "restart-key",
    );
    let task_id = task["id"].as_str().expect("task id");
    let artifact = worker
        .artifacts("artifact-token", task_id)
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .expect("artifact metadata");
    let artifact_id = artifact["id"].as_str().expect("artifact id").to_string();
    let digest = artifact["digest"].as_str().expect("digest").to_string();
    let grant = worker.grant(&artifact_id, 300_000)["grant"]
        .as_str()
        .expect("grant")
        .to_string();

    let first_incarnation = worker.incarnation.clone();
    worker.restart();
    assert_ne!(
        first_incarnation, worker.incarnation,
        "restart is a new incarnation"
    );
    let after = worker
        .artifacts("artifact-token", task_id)
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .expect("artifact metadata survives restart");
    assert_eq!(after["id"], artifact_id.as_str());
    assert_eq!(after["digest"], digest.as_str());
    let response = worker.download(Some("artifact-token"), &artifact_id);
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-goldenbee-artifact-digest")
            .expect("digest header")
            .to_str()
            .expect("digest header is text"),
        digest
    );
    assert_eq!(response.text().expect("content"), "durable evidence");
    assert_eq!(
        worker
            .grant_download(&grant)
            .text()
            .expect("grant content survives restart"),
        "durable evidence"
    );

    // Another independently stored Worker Instance cannot see the artifact or
    // reuse its grant, even though it runs the same package and token.
    let other = Worker::start("artifact-token");
    assert_eq!(
        other
            .client()
            .get(format!("{}/v1/artifacts/{artifact_id}", other.base))
            .bearer_auth("artifact-token")
            .send()
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        other
            .download(Some("artifact-token"), &artifact_id)
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        other.grant_download(&grant).status(),
        reqwest::StatusCode::BAD_REQUEST
    );
}

#[test]
fn large_artifact_streams_outside_events_and_survives_interruption() {
    let worker = Worker::start("artifact-token");
    let task = worker.run("large-session", "artifact-large:4194304", "large-key");
    let task_id = task["id"].as_str().expect("task id");
    let artifact = worker
        .artifacts("artifact-token", task_id)
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .expect("large artifact metadata");
    let artifact_id = artifact["id"].as_str().expect("artifact id");
    assert_eq!(artifact["size"], 4 * 1024 * 1024);
    assert_eq!(artifact["content_type"], "application/octet-stream");
    let digest = artifact["digest"].as_str().expect("digest").to_string();

    let page: Value = worker
        .client()
        .get(format!("{}/v1/tasks/{task_id}/events", worker.base))
        .bearer_auth("artifact-token")
        .send()
        .expect("events respond")
        .json()
        .expect("events page is JSON");
    let serialized = page.to_string();
    assert!(
        serialized.len() < 100_000,
        "events must not embed artifact bytes: {} bytes",
        serialized.len()
    );
    assert!(!serialized.contains(&"x".repeat(4096)));
    assert!(serialized.contains("artifact.finalized"));

    // Read only a prefix, then drop the response. The next transfer must still
    // be complete and digest-verified.
    let mut interrupted = worker.download(Some("artifact-token"), artifact_id);
    assert_eq!(interrupted.status(), reqwest::StatusCode::OK);
    assert_eq!(
        interrupted
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .expect("length header")
            .to_str()
            .expect("length header is text"),
        (4 * 1024 * 1024).to_string()
    );
    let mut prefix = vec![0u8; 4096];
    interrupted
        .read_exact(&mut prefix)
        .expect("partial transfer");
    assert!(prefix.iter().all(|byte| *byte == b'x'));
    drop(interrupted);

    let mut complete = worker.download(Some("artifact-token"), artifact_id);
    assert_eq!(complete.status(), reqwest::StatusCode::OK);
    assert_eq!(
        complete
            .headers()
            .get("x-goldenbee-artifact-digest")
            .expect("digest header")
            .to_str()
            .expect("digest header is text"),
        digest
    );
    let mut bytes = Vec::new();
    complete.read_to_end(&mut bytes).expect("complete transfer");
    assert_eq!(bytes.len(), 4 * 1024 * 1024);
    assert!(bytes.iter().all(|byte| *byte == b'x'));
}

#[test]
fn artifact_access_is_scoped_expiring_and_tamper_evident() {
    let worker = Worker::start_with("artifact-token", &[("mallory", "local", "mallory")]);
    let task = worker.run("scoped-session", "artifact:secret evidence", "scoped-key-1");
    let task_id = task["id"].as_str().expect("task id");
    let artifact = worker
        .artifacts("artifact-token", task_id)
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .expect("artifact metadata");
    let artifact_id = artifact["id"].as_str().expect("artifact id");

    // Missing or foreign credentials cannot inspect or transfer the artifact.
    assert_eq!(
        worker
            .client()
            .get(format!(
                "{}/v1/artifacts/{artifact_id}/content",
                worker.base
            ))
            .send()
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        worker
            .client()
            .get(format!("{}/v1/artifacts/{artifact_id}", worker.base))
            .bearer_auth("mallory")
            .send()
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert_eq!(
        worker.download(Some("mallory"), artifact_id).status(),
        reqwest::StatusCode::FORBIDDEN
    );

    // A scoped grant is sufficient on its own, and the body carries the digest.
    let grant = worker.grant(artifact_id, 60_000);
    let grant_value = grant["grant"].as_str().expect("grant value");
    let response = worker.grant_download(grant_value);
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-goldenbee-artifact-digest")
            .expect("digest header")
            .to_str()
            .expect("digest header is text"),
        artifact["digest"].as_str().expect("digest")
    );
    assert_eq!(response.text().expect("content"), "secret evidence");

    // Altered grant and altered reference fail without a transfer.
    assert_eq!(
        worker.grant_download(&format!("{grant_value}x")).status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    assert_eq!(
        worker
            .download(Some("artifact-token"), &format!("{artifact_id}x"))
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );

    // Expiry is enforced by the Worker, not by the client.
    let expiring = worker.grant(artifact_id, 1);
    std::thread::sleep(Duration::from_millis(20));
    let expired = worker.grant_download(expiring["grant"].as_str().expect("grant"));
    assert_eq!(expired.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        expired.json::<Value>().expect("error is JSON")["code"],
        "artifact.grant_expired"
    );

    // A fresh grant survives until retention removes the version, but tampering
    // with stored bytes is detected before any successful transfer.
    let tamper_grant = worker.grant(artifact_id, 60_000);
    let file = walk_files(worker.data.path())
        .into_iter()
        .find(|path| {
            path.to_string_lossy().contains("native-artifacts")
                && std::fs::read_to_string(path)
                    .map(|text| text.contains("secret evidence"))
                    .unwrap_or(false)
        })
        .expect("native artifact file");
    let mut stored: Value = serde_json::from_slice(&std::fs::read(&file).expect("read artifact"))
        .expect("artifact envelope is JSON");
    stored["text"] = serde_json::json!("tampered");
    std::fs::write(&file, serde_json::to_vec(&stored).expect("write artifact"))
        .expect("tamper artifact");

    let tampered = worker.download(Some("artifact-token"), artifact_id);
    assert_eq!(tampered.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        tampered.json::<Value>().expect("error is JSON")["code"],
        "artifact.digest_mismatch"
    );
    let tampered_grant = worker.grant_download(tamper_grant["grant"].as_str().expect("grant"));
    assert_eq!(tampered_grant.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        tampered_grant.json::<Value>().expect("error is JSON")["code"],
        "artifact.digest_mismatch"
    );

    // References are opaque. Shared paths, local-file schemes, and server-side
    // URLs are not addressable through the artifact routes.
    for reference in [
        "..%2Fstate.json",
        "file%3A%2F%2F%2Fetc%2Fpasswd",
        "https%3A%2F%2Fexample.com%2Fevidence",
    ] {
        assert_eq!(
            worker
                .client()
                .get(format!("{}/v1/artifacts/{reference}", worker.base))
                .bearer_auth("artifact-token")
                .send()
                .unwrap()
                .status(),
            reqwest::StatusCode::NOT_FOUND,
            "{reference}"
        );
        assert_eq!(
            worker
                .client()
                .get(format!("{}/v1/artifact-grants/{reference}", worker.base))
                .send()
                .unwrap()
                .status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{reference}"
        );
    }
}

#[test]
fn native_invocation_finalizes_artifact_and_authenticated_download_verifies_digest() {
    let worker = Worker::start("artifact-token");
    let task = worker.run(
        "artifact-session",
        "artifact:hello evidence",
        "artifact-key-1",
    );
    assert_eq!(task["state"], "succeeded");
    let task_id = task["id"].as_str().expect("task id");
    let invocation = task["native_invocation_id"]
        .as_str()
        .expect("native invocation id");

    let artifacts = worker.artifacts("artifact-token", task_id);
    let artifact = artifacts
        .as_array()
        .and_then(|items| items.first())
        .expect("native invocation produced one artifact");
    assert_eq!(artifact["task_id"], task_id);
    assert_eq!(artifact["namespace"], "local");
    assert_eq!(artifact["owner"], "local");
    assert_eq!(artifact["name"], "evidence.txt");
    assert_eq!(artifact["content_type"], "text/plain; charset=utf-8");
    assert_eq!(artifact["size"], "hello evidence".len());
    assert_eq!(artifact["finalized"], true);
    assert_eq!(artifact["version"], 1);
    assert_eq!(artifact["attempt"], 1);
    assert_eq!(artifact["native_invocation_id"], invocation);
    let digest = artifact["digest"].as_str().expect("digest");
    assert!(
        digest.starts_with("sha256:") && digest.len() == 71,
        "{digest}"
    );

    let artifact_id = artifact["id"].as_str().expect("artifact id");
    let response = worker.download(Some("artifact-token"), artifact_id);
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-goldenbee-artifact-digest")
            .expect("digest header")
            .to_str()
            .expect("digest header is text"),
        digest
    );
    assert_eq!(response.text().expect("content"), "hello evidence");
}
