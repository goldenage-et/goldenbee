//! Gateway backup and restore drills.
//!
//! These tests use the standalone file store contract directly. They prove
//! content validation, schema refusal, revoked-identity rejection, explicit
//! caller configuration, and separation from Worker-native state without
//! starting a shared database.

use gateway_core::backup::{self, BackupRequest, RestoreRequest, MANIFEST_FILE};
use serde_json::{json, Value};
use std::path::Path;

fn write_fixture(root: &Path, dispatch_schema: u32) {
    std::fs::create_dir_all(root.join("state")).expect("state dir");
    std::fs::write(
        root.join("state/dispatch.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": dispatch_schema,
            "submissions": {
                "exec-1": {
                    "execution_id": "exec-1",
                    "namespace": "local",
                    "caller": "alice",
                    "idempotency_key": "key-1",
                    "request_digest": "sha256:request",
                    "worker": "worker-a",
                    "worker_instance": "worker-a-instance",
                    "worker_incarnation": "inc-1",
                    "agent": "general",
                    "dispatch": "accepted",
                    "worker_task_id": "task-1",
                    "accepted": null,
                    "projection": null,
                    "connectivity": "online",
                    "last_error": null,
                    "revision": 1,
                    "claim_id": null,
                    "claimed_by": null,
                    "claim_expires_at": null,
                    "created_at": 1,
                    "updated_at": 2,
                    "transitions": []
                }
            },
            "keys": {"local\u{1f}alice\u{1f}key-1": "exec-1"}
        }))
        .unwrap(),
    )
    .expect("dispatch");
    std::fs::write(
        root.join("state/registry.json"),
        serde_json::to_vec_pretty(&json!({
            "version": 2,
            "workers": {
                "worker-a": {
                    "id": "worker-a",
                    "instance": "worker-a-instance",
                    "incarnation": "inc-1",
                    "endpoint": "http://127.0.0.1:1",
                    "capabilities": [],
                    "status": "online",
                    "lease_expires_at": 0,
                    "updated_at": 2
                }
            },
            "approvals": {
                "worker-a": {
                    "id": "worker-a",
                    "endpoint": "http://127.0.0.1:1",
                    "instance": "worker-a-instance",
                    "control_token_hash": "hash-not-a-token",
                    "enabled": true,
                    "revision": 1,
                    "source": "config",
                    "decommissioned": false,
                    "created_at": 1,
                    "updated_at": 1
                }
            },
            "teams": {
                "local/team-a": {
                    "namespace": "local",
                    "name": "team-a",
                    "version": 1,
                    "master": "worker-a",
                    "members": [
                        {"worker": "worker-a", "role": "master", "capabilities": [], "max_depth": 0}
                    ],
                    "limits": {},
                    "active": true,
                    "created_at": 1,
                    "updated_at": 1
                }
            },
            "audit": []
        }))
        .unwrap(),
    )
    .expect("registry");
}

fn write_callers(root: &Path) -> std::path::PathBuf {
    let path = root.join("gateway.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "listen": "127.0.0.1:0",
            "data_dir": "state",
            "registry": "state/registry.json",
            "callers": [
                {"token": "test-token", "id": "alice", "namespace": "local", "scopes": ["*"]}
            ]
        }))
        .unwrap(),
    )
    .expect("config");
    path
}

fn create_backup(root: &Path, output: &Path) -> backup::BackupOutcome {
    backup::create(&BackupRequest {
        data_dir: root.join("state"),
        registry: Some(root.join("state/registry.json")),
        output: output.to_path_buf(),
    })
    .expect("backup created")
}

fn restore_request(backup: &Path, target: &Path, config: &Path) -> RestoreRequest {
    RestoreRequest {
        backup: backup.to_path_buf(),
        data_dir: target.join("state"),
        registry: Some(target.join("state/registry.json")),
        not_within: Some(target.join("workers")),
        caller_config: Some(config.to_path_buf()),
        revoked_workers: Vec::new(),
        revoked_approvals: Vec::new(),
    }
}

#[test]
fn gateway_backup_restores_separate_store_and_mappings() {
    let source = tempfile::tempdir().expect("source");
    write_fixture(source.path(), 1);
    let config = write_callers(source.path());
    let backups = tempfile::tempdir().expect("backups");
    let outcome = create_backup(source.path(), &backups.path().join("gateway"));
    let report = backup::verify(&outcome.path).expect("verify");
    assert!(report.ready, "{:?}", report.checks);
    let gateway = report.manifest.gateway.clone().expect("gateway section");
    assert_eq!(gateway.store_kind, "file");
    assert_eq!(gateway.workers, vec!["worker-a".to_string()]);
    assert_eq!(gateway.teams, vec!["local/team-a@1".to_string()]);
    assert_eq!(gateway.executions.len(), 1);
    assert_eq!(
        gateway.executions[0].worker_task_id.as_deref(),
        Some("task-1")
    );
    let manifest_text =
        std::fs::read_to_string(outcome.path.join(MANIFEST_FILE)).expect("manifest");
    assert!(!manifest_text.contains("test-token"));
    assert!(!manifest_text.contains("hash-not-a-token"));

    let target = tempfile::tempdir().expect("target");
    let restored =
        backup::restore(&restore_request(&outcome.path, target.path(), &config)).expect("restore");
    assert!(restored.ready, "{:?}", restored.checks);
    assert_eq!(restored.gateway_id, gateway.gateway_id);
    assert_eq!(restored.access.status, "ok");
    assert!(target.path().join("state/dispatch.json").is_file());
    assert!(target.path().join("state/registry.json").is_file());
    assert!(!target.path().join("workers").exists());
    let dispatch: Value =
        serde_json::from_slice(&std::fs::read(target.path().join("state/dispatch.json")).unwrap())
            .expect("dispatch JSON");
    assert_eq!(
        dispatch["submissions"]["exec-1"]["worker_task_id"],
        "task-1"
    );
}

#[test]
fn corrupted_revoked_or_unconfigured_gateway_restore_is_blocked() {
    let source = tempfile::tempdir().expect("source");
    write_fixture(source.path(), 1);
    let config = write_callers(source.path());
    let backups = tempfile::tempdir().expect("backups");
    let outcome = create_backup(source.path(), &backups.path().join("gateway"));

    let corrupted = tempfile::tempdir().expect("corrupt");
    copy_tree(&outcome.path, corrupted.path());
    let mut bytes = std::fs::read(corrupted.path().join("stores/dispatch.json")).unwrap();
    bytes.extend_from_slice(b"corrupt");
    std::fs::write(corrupted.path().join("stores/dispatch.json"), bytes).unwrap();
    assert!(!backup::verify(corrupted.path()).expect("manifest").ready);
    let target = tempfile::tempdir().expect("target");
    let report = backup::restore(&restore_request(corrupted.path(), target.path(), &config))
        .expect("restore");
    assert!(!report.ready);
    assert!(!target.path().join("state/dispatch.json").exists());

    let target = tempfile::tempdir().expect("target");
    let mut request = restore_request(&outcome.path, target.path(), &config);
    request.revoked_workers = vec!["worker-a".into()];
    let report = backup::restore(&request).expect("restore");
    assert!(!report.ready);
    assert!(report
        .checks
        .iter()
        .any(|check| check.name == "revoked-credential"));
    assert!(!target.path().join("state/dispatch.json").exists());

    let target = tempfile::tempdir().expect("target");
    let mut request = restore_request(&outcome.path, target.path(), &config);
    request.caller_config = None;
    let report = backup::restore(&request).expect("restore");
    assert!(!report.ready);
    assert_eq!(report.access.status, "blocked");

    let target = tempfile::tempdir().expect("target");
    let mut request = restore_request(&outcome.path, target.path(), &config);
    request.not_within = Some(target.path().to_path_buf());
    let report = backup::restore(&request).expect("restore");
    assert!(!report.ready);
    assert!(report
        .checks
        .iter()
        .any(|check| check.name == "store-separation"));
}

#[test]
fn newer_gateway_state_is_refused_without_copying() {
    let source = tempfile::tempdir().expect("source");
    write_fixture(source.path(), 99);
    let backups = tempfile::tempdir().expect("backups");
    let error = backup::create(&BackupRequest {
        data_dir: source.path().join("state"),
        registry: Some(source.path().join("state/registry.json")),
        output: backups.path().join("gateway"),
    })
    .expect_err("newer schema");
    assert_eq!(error.code, "backup.schema_unsupported");
    assert!(!backups.path().join("gateway/manifest.json").exists());
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
