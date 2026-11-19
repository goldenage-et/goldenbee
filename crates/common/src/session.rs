//! Canonical session views and conversation turns.

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
