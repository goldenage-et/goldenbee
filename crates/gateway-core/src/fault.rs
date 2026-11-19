//! Deterministic Gateway dispatch fault injection for acceptance tests.
//!
//! Tests create one marker file per point below the directory named by
//! `GOLDENBEE_GATEWAY_FAULT_DIR`. A marker is consumed on first use, so the
//! restarted Gateway follows the normal reconciliation path.
//!
//! When the variable is unset this module does nothing. It is a test-only
//! switch, not operator configuration.

use common::ApiError;
use std::path::PathBuf;

/// Environment variable selecting the Gateway fault marker directory.
pub const FAULT_DIR: &str = "GOLDENBEE_GATEWAY_FAULT_DIR";

/// Exit code used when an injected crash terminates the process.
pub const EXIT_CODE: i32 = 75;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Point {
    /// Dispatch intent is durable, but no remote request has been sent.
    AfterIntent,
    /// The Worker confirmed acceptance, but the Gateway mapping is not durable.
    AfterAcceptance,
    /// A Worker committed a decision, but the Gateway has not acknowledged it.
    AfterApprovalForward,
    /// The next dispatch-state write fails before committing.
    Persist,
}

impl Point {
    fn marker(self) -> &'static str {
        match self {
            Self::AfterIntent => "after-intent",
            Self::AfterAcceptance => "after-acceptance",
            Self::AfterApprovalForward => "after-approval-forward",
            Self::Persist => "persist-failure",
        }
    }
}

fn marker_path(point: Point) -> Option<PathBuf> {
    let dir = std::env::var_os(FAULT_DIR)?;
    Some(PathBuf::from(dir).join(point.marker()))
}

fn trigger(point: Point) -> bool {
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

/// Fail a dispatch write before it commits when the marker is present.
pub(crate) fn persistence_failure() -> Option<ApiError> {
    trigger(Point::Persist).then(|| {
        ApiError::retryable(
            "storage.unavailable",
            "injected Gateway dispatch persistence failure before commit",
        )
    })
}
