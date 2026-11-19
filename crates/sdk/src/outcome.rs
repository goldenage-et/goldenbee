//! Typed client-facing error and outcome classification.

use common::{ApiError, GatewayTask, TaskState};
use std::time::Duration;

/// Stable client-facing category for a Gateway or transport failure.
///
/// The underlying [`ApiError`] remains the authoritative wire error. This
/// category makes recovery policy explicit without inventing a second error
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Missing, invalid, or expired authentication.
    Authentication,
    /// The authenticated principal lacks the required scope.
    Authorization,
    /// Bounded capacity or an advertised rate limit was reached.
    RateLimited,
    /// The retained cursor is older than the Worker retention window.
    CursorExpired,
    /// The cursor is malformed, repeated, or from another stream.
    CursorInvalid,
    /// The requested resource is absent or not visible.
    NotFound,
    /// Idempotency identity conflicts with a different canonical request.
    IdempotencyConflict,
    /// The acceptance or effect outcome remains unknown.
    Uncertain,
    /// A non-idempotency invariant conflict.
    Conflict,
    /// A bounded transport failure that may be retried safely.
    Transport,
    /// The peer payload does not match the published protocol.
    Protocol,
    /// The task stream itself was malformed or changed identity.
    Stream,
    /// The request is invalid and must be corrected.
    InvalidRequest,
    /// No more specific category applies.
    Other,
}

/// Classify one stable wire error for client recovery policy.
pub fn classify(error: &ApiError) -> ErrorKind {
    match error.code.as_str() {
        "auth.unauthenticated" | "auth.expired" | "delegation.expired" | "delegation.invalid" => {
            ErrorKind::Authentication
        }
        "delegation.wrong_audience" | "delegation.depth_exceeded" => ErrorKind::Authorization,
        "stream.cursor_expired" => ErrorKind::CursorExpired,
        "stream.cursor_invalid" => ErrorKind::CursorInvalid,
        "task.unknown_submission" => ErrorKind::Uncertain,
        "task.idempotency_conflict" | "worker.idempotency_conflict" | "worker.control_conflict" => {
            ErrorKind::IdempotencyConflict
        }
        "sdk.transport"
        | "route.worker_unavailable"
        | "storage.unavailable"
        | "artifact.transfer_interrupted" => ErrorKind::Transport,
        "sdk.invalid_response" | "sdk.stream_invalid" | "route.invalid_worker_response" => {
            ErrorKind::Protocol
        }
        "sdk.stream" | "sdk.stream_foreign" | "sdk.stream_overflow" => ErrorKind::Stream,
        code if code.starts_with("scope.")
            || code.starts_with("worker.")
                && matches!(
                    code,
                    "worker.endpoint_forbidden"
                        | "worker.instance_mismatch"
                        | "worker.package_mismatch"
                        | "worker.capability_mismatch"
                        | "worker.descriptor_mismatch"
                        | "worker.incarnation_mismatch"
                ) =>
        {
            ErrorKind::Authorization
        }
        code if code.starts_with("capacity.")
            || code.contains("rate_limited")
            || matches!(code, "stream.subscriber_limit" | "artifact.transfer_limit") =>
        {
            ErrorKind::RateLimited
        }
        code if code.ends_with(".not_found") => ErrorKind::NotFound,
        code if code.ends_with(".conflict") => ErrorKind::Conflict,
        code if code.ends_with(".invalid") || code.starts_with("validation.") => {
            ErrorKind::InvalidRequest
        }
        _ => ErrorKind::Other,
    }
}

/// Advertised retry delay in typed error details, clamped to five minutes.
pub fn retry_after(error: &ApiError) -> Option<Duration> {
    let millis = error.details.get("retry_after_ms")?.parse::<u64>().ok()?;
    Some(Duration::from_millis(millis.min(300_000)))
}

/// Typed interpretation of a returned task after an explicit cancel command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancellationOutcome {
    /// Cancellation completed and the task is terminal.
    Confirmed,
    /// Cancellation was accepted but the native attempt has not returned yet.
    Pending,
    /// The task had already succeeded or failed; no cancellation was needed.
    AlreadyTerminal,
    /// The task outcome is uncertain and requires reconciliation.
    Uncertain,
}

/// Interpret one Gateway task projection after cancellation.
pub fn cancellation_outcome(task: &GatewayTask) -> CancellationOutcome {
    match task.task.state {
        TaskState::Cancelled => CancellationOutcome::Confirmed,
        TaskState::Succeeded | TaskState::Failed => CancellationOutcome::AlreadyTerminal,
        TaskState::Recovering | TaskState::Unknown => CancellationOutcome::Uncertain,
        TaskState::Accepted
        | TaskState::Queued
        | TaskState::Running
        | TaskState::WaitingApproval
        | TaskState::Cancelling => CancellationOutcome::Pending,
    }
}
