//! Deterministic fault injection for acceptance and dispatch boundaries.
//!
//! Tests create one marker file per fault point below the directory named by
//! `GOLDENBEE_FAULT_DIR`. A marker is consumed on first use, so a restarted
//! process runs normally and the retry path is observable. When the variable is
//! unset this module does nothing.
//!
//! The injected boundaries are the ones the architecture decision requires:
//! durable reservation, native start, and acceptance acknowledgment. A
//! persistence marker fails the next store write before any in-memory state is
//! committed, which is the same path a full disk takes.

use common::ApiError;
use std::path::PathBuf;

/// Environment variable selecting the fault marker directory.
pub const FAULT_DIR: &str = "GOLDENBEE_FAULT_DIR";

/// Exit code used when an injected crash terminates the process.
pub const EXIT_CODE: i32 = 75;

/// Boundaries a test can terminate or fail deterministically.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Point {
    /// A task is durably marked running, before native execution is requested.
    AfterReservation,
    /// Native execution is about to be requested.
    BeforeNativeStart,
    /// The native Runner returned an invocation stream; the outcome is unknown.
    AfterNativeStart,
    /// The first native event exposed the invocation identity, which is now
    /// durable, but the outcome is still unknown.
    AfterNativeEvent,
    /// Acceptance metadata is durable but the caller has not been acknowledged.
    BeforeAcknowledge,
    /// A peer accepted the delegated objective, but the parent has not
    /// persisted the remote task identity yet.
    AfterPeerSend,
    /// A protected operation identity is durable, before the external dispatch.
    AfterEffectReservation,
    /// The external system returned a confirmed effect, before its receipt is
    /// persisted.
    AfterEffectDispatch,
    /// The receipt is durable, before the native tool response is returned.
    AfterEffectReceipt,
    /// An approval decision is durable, before the native workflow resumes.
    AfterApprovalDecision,
    /// The next harness metadata write fails before committing.
    Persist,
}

impl Point {
    fn marker(self) -> &'static str {
        match self {
            Self::AfterReservation => "after-reservation",
            Self::BeforeNativeStart => "before-native-start",
            Self::AfterNativeStart => "after-native-start",
            Self::AfterNativeEvent => "after-native-event",
            Self::BeforeAcknowledge => "before-acknowledge",
            Self::AfterPeerSend => "after-peer-send",
            Self::AfterEffectReservation => "after-effect-reservation",
            Self::AfterEffectDispatch => "after-effect-dispatch",
            Self::AfterEffectReceipt => "after-effect-receipt",
            Self::AfterApprovalDecision => "after-approval-decision",
            Self::Persist => "persist-failure",
        }
    }
}

fn marker_path(point: Point) -> Option<PathBuf> {
    let dir = std::env::var_os(FAULT_DIR)?;
    Some(PathBuf::from(dir).join(point.marker()))
}

/// Consume the marker for `point`, returning true exactly once.
pub(crate) fn trigger(point: Point) -> bool {
    let Some(path) = marker_path(point) else {
        return false;
    };
    if !path.exists() {
        return false;
    }
    let _ = std::fs::remove_file(&path);
    true
}

/// Terminate the process at `point` when its marker is present.
pub(crate) fn exit_if(point: Point) {
    if trigger(point) {
        std::process::exit(EXIT_CODE);
    }
}

/// Fail a store write before it commits when the persistence marker is present.
pub(crate) fn persistence_failure() -> Option<ApiError> {
    trigger(Point::Persist).then(|| {
        ApiError::retryable(
            "storage.unavailable",
            "injected persistence failure before commit",
        )
    })
}
