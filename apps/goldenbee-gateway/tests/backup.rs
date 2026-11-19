//! Restored Gateway projections are reachable through the SDK boundary.
//!
//! The Gateway store is restored into its own directory, then a real Gateway
//! process starts and a real SDK client reads health and Workers. The test does
//! not merge the projection into Worker-native state.

use gateway_core::backup::{self, BackupRequest, RestoreRequest};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TOKEN: &str = "sdk-backup-token";

fn fixture(root: &Path) {
    std::fs::create_dir_all(root.join("state")).expect("state");
    std::fs::write(
        root.join("state/dispatch.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": 1,
            "submissions": {},
            "keys": {}
        }))
        .unwrap(),
    )
    .expect("dispatch");
    std::fs::write(
        root.join("state/registry.json"),
        serde_json::to_vec_pretty(&json!({
            "version": 2,
            "workers": {},
            "approvals": {},
            "teams": {},
            "audit": []
        }))
        .unwrap(),
    )
    .expect("registry");
}

fn config(root: &Path, data_dir: &Path, registry: &Path) -> std::path::PathBuf {
    let path = root.join(format!("gateway-{}.json", std::process::id()));
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "listen": "127.0.0.1:0",
            "data_dir": data_dir,
            "registry": registry,
            "callers": [
                {"token": TOKEN, "id": "alice", "namespace": "local", "scopes": ["*"]}
            ]
        }))
        .unwrap(),
    )
    .expect("config");
    path
}

fn spawn(config: &Path) -> (Child, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_gb-gateway"))
        .args(["serve", "--config", config.to_str().unwrap()])
        .env("GOLDENBEE_DELEGATION_SECRET", "sdk-backup-secret")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("gateway starts");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        line.clear();
        if reader.read_line(&mut line).expect("line") == 0 {
            break;
        }
        if let Some(base) = line.split("listening on ").nth(1) {
            return (child, base.trim().to_string());
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("gateway did not announce a listener: {line}");
}

#[test]
fn restored_gateway_is_reachable_through_the_sdk_boundary() {
    let source = tempfile::tempdir().expect("source");
    fixture(source.path());
    let source_config = config(
        source.path(),
        &source.path().join("state"),
        &source.path().join("state/registry.json"),
    );
    let backups = tempfile::tempdir().expect("backups");
    let backup = backup::create(&BackupRequest {
        data_dir: source.path().join("state"),
        registry: Some(source.path().join("state/registry.json")),
        output: backups.path().join("gateway"),
    })
    .expect("backup");

    let target = tempfile::tempdir().expect("target");
    let restored = backup::restore(&RestoreRequest {
        backup: backup.path.clone(),
        data_dir: target.path().join("state"),
        registry: Some(target.path().join("state/registry.json")),
        not_within: Some(target.path().join("workers")),
        caller_config: Some(source_config),
        revoked_workers: Vec::new(),
        revoked_approvals: Vec::new(),
    })
    .expect("restore");
    assert!(restored.ready, "{:?}", restored.checks);

    let target_config = config(
        target.path(),
        &target.path().join("state"),
        &target.path().join("state/registry.json"),
    );
    let (mut gateway, base) = spawn(&target_config);
    let base = if base.starts_with("http://") || base.starts_with("https://") {
        base
    } else {
        format!("http://{base}")
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let client = sdk::Client::new(base, TOKEN);
    let health: Value = serde_json::to_value(
        runtime
            .block_on(client.health())
            .expect("SDK health after restore"),
    )
    .expect("health JSON");
    assert_eq!(health["status"], "live");
    let workers = runtime.block_on(client.workers()).expect("SDK workers");
    assert!(workers.is_empty(), "restored projection started empty");
    let _ = gateway.kill();
    let _ = gateway.wait();
}
