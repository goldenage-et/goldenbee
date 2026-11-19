//! Opaque identifiers, keys, and small time helpers.

use std::time::Duration;

/// Generate a short, opaque identifier.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// Profile key used by the canonical store.
pub(crate) fn profile_key(namespace: &str, name: &str) -> String {
    format!("{namespace}\u{1f}{name}")
}

/// Task idempotency key.
pub(crate) fn task_key(namespace: &str, caller: &str, key: &str) -> String {
    format!("{namespace}\u{1f}{caller}\u{1f}{key}")
}

/// Current time.
pub(crate) fn now() -> i64 {
    common::now_ms()
}

/// Convert a duration in milliseconds into a Tokio duration.
pub(crate) fn millis(value: u64) -> Duration {
    Duration::from_millis(value)
}
