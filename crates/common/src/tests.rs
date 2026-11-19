//! Cross-module wire contract behavior.

use crate::*;

#[test]
fn scopes_and_namespaces_are_enforced() {
    let principal = Principal::new("u1", "app", &[scopes::TASK_SUBMIT]);
    assert!(principal.require(scopes::TASK_SUBMIT).is_ok());
    assert_eq!(
        principal.require(scopes::ADMIN).unwrap_err().code,
        "scope.denied"
    );
    assert!(principal.require_namespace("other").is_err());
}

#[test]
fn limits_only_narrow() {
    let parent = Limits {
        max_duration_ms: 100,
        max_cost_micros: 10,
        max_fanout: 4,
        ..Limits::default()
    };
    let child = Limits {
        max_duration_ms: 500,
        max_cost_micros: 20,
        max_fanout: 2,
        ..Limits::default()
    };
    let effective = parent.narrower(&child);
    assert_eq!(effective.max_duration_ms, 100);
    assert_eq!(effective.max_cost_micros, 10);
    assert_eq!(effective.max_fanout, 2);
}

#[test]
fn redaction_removes_canaries() {
    assert_eq!(redact("api_key=secret-123 ok"), "[REDACTED] ok");
}

#[test]
fn digests_are_stable() {
    assert_eq!(bytes_digest(b"hello"), bytes_digest(b"hello"));
    assert_ne!(bytes_digest(b"hello"), bytes_digest(b"world"));
}

#[test]
fn control_wire_types_are_transport_only_and_redactable() {
    let request = ControlCommandRequest {
        session_id: "s".into(),
        generation: 7,
        epoch: "ctl-1".into(),
        command: ControlCommand::Type {
            selector: "#password".into(),
            text: "secret-value".into(),
        },
    };
    let encoded = serde_json::to_value(&request).unwrap();
    assert_eq!(encoded["command"]["kind"], "type");
    assert_eq!(request.command.kind(), "type");

    let entry = ControlAuditEntry {
        at: 1,
        action: "command".into(),
        session_id: "s".into(),
        principal: "alice".into(),
        generation: 7,
        outcome: "accepted".into(),
        detail: request.command.kind().into(),
    };
    let audit = serde_json::to_string(&entry).unwrap();
    assert!(!audit.contains("secret-value"));
    assert!(!audit.contains("#password"));
}

fn legacy_metadata() -> serde_json::Value {
    serde_json::json!({
        "schema": RELEASE_METADATA_SCHEMA,
        "package": {
            "name": "general",
            "version": "0.1.0",
            "protocol": "1.0",
            "runtime": "0.1.0",
            "adk": "2.3.0",
            "capabilities": [{
                "id": "manual",
                "kind": "agent",
                "description": "manual agent",
                "permissions": ["task:submit"]
            }],
            "required_permissions": ["task:submit"],
            "features": ["agents"],
            "launcher_modes": ["dev", "serve"],
            "hooks": []
        },
        "source_digest": bytes_digest(b"source"),
        "binary_digest": bytes_digest(b"binary"),
        "target": "x86_64-unknown-linux-gnu",
        "signer": "publisher",
        "seed_assets": []
    })
}

#[test]
fn legacy_release_metadata_defaults_the_protocol_range() {
    let metadata: ReleaseMetadata = serde_json::from_value(legacy_metadata()).unwrap();
    assert_eq!(metadata.source_revision, "");
    assert_eq!(metadata.protocol_range(), ("1.0", "1.0"));
    assert_eq!(metadata.build, BuildEvidence::default());
    metadata.validate_protocol_range().unwrap();
}

#[test]
fn protocol_range_must_be_well_formed_and_include_the_package_version() {
    let mut metadata: ReleaseMetadata = serde_json::from_value(legacy_metadata()).unwrap();
    metadata.protocol_min = "1.0".into();
    metadata.protocol_max = "1.2".into();
    metadata.validate_protocol_range().unwrap();
    assert!(protocol_in_range("1.1", "1.0", "1.2"));
    assert!(!protocol_in_range("2.0", "1.0", "1.2"));

    metadata.package.protocol = "2.0".into();
    assert_eq!(
        metadata.validate_protocol_range().unwrap_err().code,
        "package.protocol_range_invalid"
    );

    metadata.protocol_min = "1.4".into();
    metadata.protocol_max = "1.1".into();
    assert_eq!(
        metadata.validate_protocol_range().unwrap_err().code,
        "package.protocol_range_invalid"
    );
}
