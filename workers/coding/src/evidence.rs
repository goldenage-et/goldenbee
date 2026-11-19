//! Objective evidence for one coding assignment.
//!
//! A successful model turn is not acceptance. After the native outcome is
//! committed this module captures the worktree diff, re-runs configured checks
//! in the sandbox, records out-of-worktree changes, stores digest-verified
//! native artifacts, and attaches the summary to the task projection.

use crate::package::CodingPackage;
use crate::repo;
use crate::services::CodingServices;
use adk_artifact::{ArtifactService, SaveRequest};
use adk_rust::Part;
use common::{ApiError, TaskState};
use runtime::worker::TaskHookContext;
use runtime::ExecutionOutcome;
use serde_json::{json, Value};
use std::sync::Arc;

const EVIDENCE_SCHEMA: &str = "coding.evidence@1";

/// Collect, persist, and attach evidence for a committed task outcome.
pub async fn collect(
    package: &CodingPackage,
    ctx: &TaskHookContext,
    outcome: &ExecutionOutcome,
) -> Result<Value, ApiError> {
    let services = package.current()?;
    let runtime = package.runtime()?;
    let state = outcome.state.unwrap_or(if outcome.error.is_some() {
        TaskState::Failed
    } else {
        TaskState::Succeeded
    });
    let app_name = "coding";
    let user_id = runtime::native::native_user_id(&ctx.namespace, &ctx.caller)?;
    let session_id = ctx.session_id.clone();
    let store = runtime.artifact_store();

    let mut unverified = Vec::new();
    let diff = services.diff().unwrap_or_else(|error| {
        unverified.push(format!("diff unavailable: {}", error.code));
        String::new()
    });
    let status = services.status().unwrap_or_else(|error| {
        unverified.push(format!("status unavailable: {}", error.code));
        String::new()
    });
    let diff_empty = !services.task_changed().unwrap_or_else(|error| {
        unverified.push(format!("task baseline unavailable: {}", error.code));
        false
    });
    if diff_empty {
        unverified.push("task made no worktree change".into());
    }
    let untracked: Vec<&str> = status
        .lines()
        .filter(|line| line.starts_with("??"))
        .collect();
    if !untracked.is_empty() {
        unverified.push(format!(
            "untracked files are not included in the binary diff ({})",
            untracked.len()
        ));
    }

    let mut checks = Vec::new();
    if state == TaskState::Succeeded {
        if services.checks().is_empty() {
            unverified.push("no checks configured".into());
        }
        for command in services.checks() {
            if ctx.cancel.is_cancelled() {
                unverified.push(format!("check skipped after cancellation: {command}"));
                break;
            }
            let outcome = services.build().check(command, &ctx.cancel).await;
            if outcome.get("status").and_then(Value::as_str) != Some("passed") {
                unverified.push(format!("check did not pass: {command}"));
            }
            checks.push(outcome);
        }
    } else {
        unverified.push(format!(
            "task state {state:?}; configured checks were not accepted as evidence"
        ));
    }

    let after_fingerprint = repo::fingerprint(&services.config().repo)?;
    let changed_outside = after_fingerprint != services.repo_fingerprint();
    if changed_outside {
        unverified.push("repository changed outside the owned worktree".into());
    }

    let diff_ref = save_text(
        &store,
        app_name,
        &user_id,
        &session_id,
        "coding.diff.patch",
        &diff,
    )
    .await?;
    let status_ref = save_text(
        &store,
        app_name,
        &user_id,
        &session_id,
        "coding.status.txt",
        &status,
    )
    .await?;

    let mut verified = true;
    for reference in [&diff_ref, &status_ref] {
        let name = reference["name"].as_str().unwrap_or_default();
        let version = reference["version"].as_i64().unwrap_or(1);
        let digest = reference["digest"].as_str().unwrap_or_default();
        let payload = store
            .payload(app_name, &user_id, &session_id, name, version)
            .await
            .map_err(|error| {
                ApiError::new(
                    "artifact.digest_mismatch",
                    format!(
                        "saved evidence artifact is not readable: {}: {}",
                        error.code, error.message
                    ),
                )
            })?;
        if common::bytes_digest(&payload.bytes) != digest {
            verified = false;
            unverified.push(format!("artifact digest mismatch: {name}"));
        }
    }

    let all_checks_passed = !checks.is_empty()
        && checks
            .iter()
            .all(|check| check.get("status").and_then(Value::as_str) == Some("passed"));
    let accepted = state == TaskState::Succeeded
        && all_checks_passed
        && !diff_empty
        && !changed_outside
        && verified;

    let evidence = json!({
        "schema": EVIDENCE_SCHEMA,
        "task_id": ctx.task_id,
        "instance": services.config().instance,
        "state": state,
        "base_commit": services.base_commit(),
        "diff": diff_ref,
        "status": status_ref,
        "checks": checks,
        "changed_outside_worktree": changed_outside,
        "task_changed": !diff_empty,
        "plugin": {
            "tool_calls": package.counters().calls(),
            "tool_names": package.counters().names(),
        },
        "unverified": unverified,
        "accepted": accepted,
        "verified": verified,
    });

    let evidence_text = serde_json::to_string_pretty(&evidence)
        .map_err(|error| ApiError::new("coding.evidence_failed", error.to_string()))?;
    let evidence_ref = save_text(
        &store,
        app_name,
        &user_id,
        &session_id,
        "coding.evidence.json",
        &evidence_text,
    )
    .await?;
    let summary = json!({
        "schema": EVIDENCE_SCHEMA,
        "task_id": ctx.task_id,
        "instance": services.config().instance,
        "accepted": accepted,
        "state": state,
        "verified": verified,
        "diff": evidence["diff"],
        "status": evidence["status"],
        "evidence": evidence_ref,
        "checks": evidence["checks"],
        "changed_outside_worktree": changed_outside,
        "task_changed": !diff_empty,
        "unverified": evidence["unverified"],
        "plugin": evidence["plugin"],
    });
    runtime
        .set_task_evidence(&ctx.task_id, summary.clone())
        .await?;
    runtime
        .append_event(&ctx.task_id, "coding.evidence", summary.clone(), true)
        .await?;
    Ok(summary)
}

/// Remove a cancelled checkout after evidence was captured. Identity remains
/// owned by this instance, so the next task re-provisions the same path.
pub fn cleanup_cancelled(services: &Arc<CodingServices>) -> Option<String> {
    services
        .remove_checkout()
        .err()
        .map(|error| format!("{}: {}", error.code, common::redact(&error.message)))
}

async fn save_text(
    store: &runtime::native::NativeArtifactStore,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    name: &str,
    text: &str,
) -> Result<Value, ApiError> {
    let response = store
        .save(SaveRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            file_name: name.to_string(),
            part: Part::Text {
                text: text.to_string(),
            },
            version: None,
        })
        .await
        .map_err(|error| {
            ApiError::new(
                "coding.evidence_failed",
                format!("native evidence artifact could not be saved: {error}"),
            )
        })?;
    Ok(json!({
        "name": name,
        "version": response.version,
        "digest": common::bytes_digest(text.as_bytes()),
        "bytes": text.len(),
    }))
}
