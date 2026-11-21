//! Read-only workspace inspection through the real General Worker HTTP surface.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

mod fixture;

struct Worker {
    child: Child,
    base: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn(config: &std::path::Path, log: &std::path::Path) -> Worker {
    let stderr = File::create(log).expect("stderr log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
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
    Worker { child, base }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_stream_reports_a_changed_file() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "general-workspace-live".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let workspace = data.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let mut config = std::fs::read_to_string(&fixture.config).expect("config");
    config.push_str("\n[workspace]\nbrowse = true\n");
    std::fs::write(&fixture.config, config).expect("config update");
    let worker = spawn(&fixture.config, &data.path().join("stderr.log"));
    let client = reqwest::Client::new();
    let mut response = client
        .get(format!("{}/v1/workspace/events/stream", worker.base))
        .bearer_auth("dev-token")
        .send()
        .await
        .expect("stream responds");
    assert!(response.status().is_success());
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    std::fs::write(workspace.join("live.txt"), "changed").expect("changed file");
    let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), response.chunk())
        .await
        .expect("stream emits within deadline")
        .expect("stream chunk")
        .expect("stream bytes");
    let text = String::from_utf8_lossy(&chunk);
    assert!(text.contains("workspace.changed"), "{text}");
    assert!(text.contains("live.txt"), "{text}");
}

#[test]
fn workspace_tree_and_preview_are_bounded_and_relative() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "general-workspace".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let workspace = data.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::write(workspace.join("index.html"), "<h1>todo</h1>").expect("file");
    let mut config = std::fs::read_to_string(&fixture.config).expect("config");
    config.push_str("\n[workspace]\nbrowse = true\n");
    std::fs::write(&fixture.config, config).expect("config update");
    let worker = spawn(&fixture.config, &data.path().join("stderr.log"));
    let client = reqwest::blocking::Client::new();

    let tree: serde_json::Value = client
        .get(format!("{}/v1/workspace/tree", worker.base))
        .bearer_auth("dev-token")
        .send()
        .expect("tree responds")
        .json()
        .expect("tree JSON");
    assert_eq!(tree["path"], "");
    assert!(tree["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .any(|entry| entry["path"] == "index.html"));
    assert!(!serde_json::to_string(&tree)
        .expect("tree string")
        .contains(data.path().to_string_lossy().as_ref()));

    let file: serde_json::Value = client
        .get(format!("{}/v1/workspace/file", worker.base))
        .query(&[("path", "index.html")])
        .bearer_auth("dev-token")
        .send()
        .expect("file responds")
        .json()
        .expect("file JSON");
    assert_eq!(file["text"], "<h1>todo</h1>");
    assert_eq!(file["binary"], false);
    assert_eq!(file["path"], "index.html");

    let denied = client
        .get(format!("{}/v1/workspace/file", worker.base))
        .query(&[("path", "../worker.toml")])
        .bearer_auth("dev-token")
        .send()
        .expect("denied responds");
    assert_eq!(denied.status(), reqwest::StatusCode::BAD_REQUEST);
    let error: serde_json::Value = denied.json().expect("error JSON");
    assert_eq!(error["code"], "workspace.path_denied");
}
