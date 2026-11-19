//! Durable event envelopes, retained stream pages, and cursor parsing.

use crate::ApiError;
use serde::{Deserialize, Serialize};

/// Durable event envelope schema version.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Maximum number of retained events one page may request.
pub const MAX_EVENT_PAGE: usize = 1_000;

/// Read one raw query parameter without accepting a second definition.
///
/// Stream cursors are decimal integers, so percent-encoded or duplicate values
/// are rejected by the typed parser rather than guessed at.
fn query_value<'a>(query: Option<&'a str>, name: &str) -> Result<Option<&'a str>, ()> {
    let Some(query) = query else {
        return Ok(None);
    };
    let mut found = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key != name {
            continue;
        }
        if found.is_some() {
            return Err(());
        }
        found = Some(value);
    }
    Ok(found)
}

/// Parse the `after` stream cursor.
///
/// Absent means a fresh subscription from the earliest retained event. Any
/// present value must be a base-10 `u64`; malformed, negative, empty, or
/// duplicated cursors are explicit `stream.cursor_invalid` errors.
pub fn parse_cursor(query: Option<&str>) -> Result<u64, ApiError> {
    let value = query_value(query, "after").map_err(|_| {
        ApiError::new(
            "stream.cursor_invalid",
            "the stream cursor must appear at most once",
        )
        .with("parameter", "after")
    })?;
    let Some(value) = value else {
        return Ok(0);
    };
    value.parse::<u64>().map_err(|_| {
        ApiError::new(
            "stream.cursor_invalid",
            "the stream cursor must be a non-negative decimal sequence number",
        )
        .with("parameter", "after")
    })
}

/// Parse the `limit` page size.
///
/// Absent means no explicit limit. Any present value must be a positive base-10
/// integer; malformed values are explicit `stream.limit_invalid` errors.
pub fn parse_limit(query: Option<&str>) -> Result<Option<usize>, ApiError> {
    let value = query_value(query, "limit").map_err(|_| {
        ApiError::new(
            "stream.limit_invalid",
            "the stream limit must appear at most once",
        )
        .with("parameter", "limit")
    })?;
    let Some(value) = value else {
        return Ok(None);
    };
    match value.parse::<usize>() {
        Ok(limit) if limit > 0 => Ok(Some(limit)),
        _ => Err(ApiError::new(
            "stream.limit_invalid",
            "the stream limit must be a positive decimal integer",
        )
        .with("parameter", "limit")),
    }
}

/// A durable event envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    /// Globally unique event identity.
    pub event_id: String,
    /// Source process identity.
    pub source: String,
    /// Source process incarnation.
    pub incarnation: String,
    /// Stream identity, for example `task:<id>`.
    pub stream_id: String,
    /// Monotonic sequence within the stream.
    pub seq: u64,
    /// Envelope schema version.
    pub schema_version: u32,
    /// Durable domain event kind.
    pub kind: String,
    /// Whether replay is guaranteed for the retention window.
    pub durable: bool,
    /// Event payload.
    pub payload: serde_json::Value,
    /// Trace correlation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Event time.
    pub at: i64,
}

/// A page of durable stream events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamPage {
    /// Stream identity.
    pub stream_id: String,
    /// Retained events after the requested cursor.
    pub events: Vec<StreamEvent>,
    /// Cursor to use for the next request.
    pub next_seq: u64,
    /// Oldest retained sequence.
    pub oldest_seq: u64,
    /// True when the requested cursor fell outside retention.
    pub cursor_expired: bool,
}
