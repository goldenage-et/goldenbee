//! Canonical session views, conversation turns, and transcript export.

use serde::{Deserialize, Serialize};

/// Session view returned by native Worker management access.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    /// Namespace.
    pub namespace: String,
    /// Principal.
    pub principal: String,
    /// Session ID.
    pub session_id: String,
    /// Operator-visible session title, when one was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Source session when this session was forked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
    /// Number of canonical events.
    pub event_count: usize,
    /// Ordered conversation turns.
    pub turns: Vec<SessionTurn>,
    /// Creation time.
    pub created_at: i64,
    /// Update time.
    pub updated_at: i64,
}

/// One canonical conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTurn {
    /// Author role.
    pub role: String,
    /// Text, redacted before exposure if it contains secrets.
    pub text: String,
    /// Time.
    pub at: i64,
}

/// Request to fork a session into an independent branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionForkRequest {
    /// Identity of the new session.
    pub session_id: String,
    /// Optional title for the fork. The source title is copied when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Request to rename a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRenameRequest {
    /// New operator-visible title.
    pub title: String,
}

/// Durable transcript export of one canonical session.
///
/// The export is derived from the native session events; it never replaces
/// them. `digest` covers the redacted turns so two exports of the same history
/// are byte-identical for comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTranscript {
    /// Session ID.
    pub session_id: String,
    /// Namespace.
    pub namespace: String,
    /// Principal.
    pub principal: String,
    /// Canonical digest of the exported turns.
    pub digest: String,
    /// Serialized transcript size in bytes.
    pub bytes: usize,
    /// Number of exported turns.
    pub turn_count: usize,
    /// Export time.
    pub exported_at: i64,
    /// Ordered conversation turns.
    pub turns: Vec<SessionTurn>,
}
