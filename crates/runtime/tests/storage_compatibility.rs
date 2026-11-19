//! Removing superseded fleet APIs must not discard existing Worker metadata.

use runtime::{State, Store};
use serde_json::{json, Value};

#[tokio::test]
async fn legacy_fleet_metadata_survives_worker_writes_and_reopen() {
    let directory = tempfile::tempdir().expect("temporary data directory");
    let path = directory.path().join("state.json");
    let mut original = serde_json::to_value(State::default()).expect("state JSON");
    original["workers"] = json!({
        "worker-a": {
            "id": "worker-a",
            "endpoint": "http://127.0.0.1:8080",
            "incarnation": "incarnation-a",
            "package": "general",
            "protocol": "1.0",
            "capabilities": [],
            "policy": "default",
            "lease_expires_at": 1000,
            "renewed_at": 500,
            "capacity": common::Capacity::default(),
            "status": "registered"
        }
    });
    original["teams"] = json!({
        "local\u{1f}review": {
            "namespace": "local",
            "name": "review",
            "version": 2,
            "master": "worker-a",
            "members": [],
            "limits": common::Limits::default(),
            "active": false,
            "created_at": 100,
            "updated_at": 200
        }
    });
    original["releases"] = json!({
        "general@0.1.0": {
            "release": {
                "package": "general",
                "version": "0.1.0",
                "digest": "sha256:release",
                "signer": "publisher",
                "target": "x86_64-unknown-linux-gnu",
                "manifest_digest": "sha256:manifest",
                "added_permissions": [],
                "revocation_generation": 1
            },
            "manifest": "e30=",
            "signature": "retained-signature",
            "published_at": 100
        }
    });
    original["revocations"] = json!(["sha256:revoked"]);
    original["instances"] = json!({
        "general-a": {
            "id": "general-a",
            "package": "general",
            "version": "0.1.0",
            "data_dir": "instances/general-a",
            "profiles": ["default"],
            "secret_refs": ["env:MODEL_TOKEN"],
            "limits": common::Limits::default(),
            "binary_version": "0.1.0",
            "data_schema": runtime::DATA_SCHEMA
        }
    });
    std::fs::write(&path, serde_json::to_vec(&original).expect("state bytes"))
        .expect("existing state file");

    let store = Store::open(&path, State::default())
        .await
        .expect("existing state opens");
    store
        .write(|state| {
            state.counters.audit += 1;
            Ok(())
        })
        .await
        .expect("unrelated Worker write commits");
    drop(store);

    let reopened = Store::open(&path, State::default())
        .await
        .expect("state reopens");
    assert_eq!(reopened.read(|state| state.counters.audit).await, 1);
    let persisted: Value = serde_json::from_slice(&std::fs::read(&path).expect("persisted bytes"))
        .expect("persisted state JSON");
    for field in [
        "schema",
        "workers",
        "teams",
        "releases",
        "revocations",
        "instances",
    ] {
        assert_eq!(persisted[field], original[field], "retained field: {field}");
    }
}
