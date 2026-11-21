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

#[test]
fn exit_classes_separate_usage_authorization_approval_and_uncertainty() {
    assert_eq!(exit_code(None, TaskState::Succeeded), exit::SUCCESS);
    assert_eq!(
        exit_code(
            Some(&ApiError::new("validation.input", "")),
            TaskState::Failed
        ),
        exit::USAGE
    );
    assert_eq!(
        exit_code(
            Some(&ApiError::new("cli.value_missing", "")),
            TaskState::Failed
        ),
        exit::USAGE
    );
    assert_eq!(
        exit_code(
            Some(&ApiError::new("scope.task_submit", "")),
            TaskState::Failed
        ),
        exit::AUTHORIZATION
    );
    assert_eq!(
        exit_code(
            Some(&ApiError::new("permission.denied", "")),
            TaskState::Failed
        ),
        exit::AUTHORIZATION
    );
    assert_eq!(
        exit_code(
            Some(&ApiError::new("approval.pending", "")),
            TaskState::WaitingApproval
        ),
        exit::APPROVAL
    );
    assert_eq!(exit_code(None, TaskState::Unknown), exit::UNCERTAIN);
    assert_eq!(exit_code(None, TaskState::Recovering), exit::UNCERTAIN);
    assert_eq!(exit_code(None, TaskState::Running), exit::UNCERTAIN);
    assert_eq!(exit_code(None, TaskState::Cancelled), exit::CANCELLED);
    assert_eq!(
        exit_code(
            Some(&ApiError::new("execution.failed", "")),
            TaskState::Failed
        ),
        exit::FAILURE
    );
}

#[test]
fn run_input_rejects_oversized_and_malformed_images_without_echoing_them() {
    let oversized = RunImage::from_bytes(
        "image/png",
        vec![0u8; MAX_RUN_IMAGE_BYTES + 1],
        Some("secret-name".into()),
    );
    let error = RunInput {
        text: "look".into(),
        images: vec![oversized],
        ..RunInput::default()
    }
    .validate()
    .unwrap_err();
    assert_eq!(error.code, "validation.run_image");
    assert!(!error.message.contains("secret-name"));

    let malformed = RunImage {
        media_type: "image/png".into(),
        data: "not base64".into(),
        name: None,
    };
    assert_eq!(
        RunInput {
            text: "look".into(),
            images: vec![malformed],
            ..RunInput::default()
        }
        .validate()
        .unwrap_err()
        .code,
        "validation.run_image"
    );

    assert_eq!(
        RunInput {
            text: "look".into(),
            images: vec![RunImage::from_bytes("image/bmp", b"x", None)],
            ..RunInput::default()
        }
        .validate()
        .unwrap_err()
        .code,
        "validation.run_image"
    );
}

#[test]
fn run_request_rejects_conflicting_session_selectors_before_admission() {
    let mut request = RunRequest {
        input: RunInput::text("hello"),
        ..RunRequest::default()
    };
    request.continue_session = true;
    request.resume_session = Some("session-a".into());
    assert_eq!(
        request.validate().unwrap_err().code,
        "validation.run_session"
    );

    let request = RunRequest {
        input: RunInput::text("hello"),
        session_id: Some("session-a".into()),
        resume_session: Some("session-b".into()),
        ..RunRequest::default()
    };
    assert_eq!(
        request.validate().unwrap_err().code,
        "validation.run_session"
    );

    let request = RunRequest {
        input: RunInput::text("hello"),
        wait_ms: Some(MAX_RUN_WAIT_MS + 1),
        ..RunRequest::default()
    };
    assert_eq!(request.validate().unwrap_err().code, "validation.run_wait");
}

#[test]
fn jsonl_frames_use_tagged_goldenbee_task_and_item_vocabulary() {
    let frame = JsonlFrame::ItemCompleted {
        task_id: "task-1".into(),
        seq: 3,
        at: 1,
        item: JsonlItem::ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            server: None,
            agent: Some("general".into()),
            ok: Some(true),
            error_code: None,
        },
    };
    let value = serde_json::to_value(&frame).unwrap();
    assert_eq!(value["type"], "item_completed");
    assert_eq!(value["item"]["item_type"], "tool_call");
    assert_eq!(value["item"]["id"], "call-1");
    assert!(value["item"].get("server").is_none());
}
