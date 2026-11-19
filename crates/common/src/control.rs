//! Fenced interactive-control wire types.
//!
//! These are transport representations only. The Worker runtime owns the lease,
//! generation allocation, expiry, and audit. Neither the Gateway, the SDK, nor
//! `common` may decide that a controller is current.

use serde::{Deserialize, Serialize};

/// Wire schema version for fenced control.
pub const CONTROL_SCHEMA_VERSION: &str = "1.0";

/// Who currently owns one interactive browser session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlOwner {
    /// The Worker's native agent execution owns the session.
    Agent,
    /// An authorized human controller owns the session.
    Human,
}

/// Request to take exclusive control of one exact authenticated session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlLeaseRequest {
    /// Session identity within the authenticated principal scope.
    pub session_id: String,
    /// Requested lease duration. The Worker clamps it to its own ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
}

/// The Worker's authoritative current lease.
///
/// `epoch` identifies one Worker process incarnation. Commands that present a
/// different epoch are stale even when their numeric generation happens to
/// match after a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlLease {
    /// Session identity.
    pub session_id: String,
    /// Fencing generation allocated by the Worker.
    pub generation: u64,
    /// Current owner.
    pub owner: ControlOwner,
    /// Human principal when `owner` is human.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    /// Worker process epoch that allocated this lease.
    pub epoch: String,
    /// Expiry time in Unix milliseconds. Zero when the agent owns the session.
    #[serde(default)]
    pub expires_at_ms: i64,
    /// Control protocol schema.
    #[serde(default = "control_schema")]
    pub schema_version: String,
}

fn control_schema() -> String {
    CONTROL_SCHEMA_VERSION.to_string()
}

/// Current ownership without changing it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlStatus {
    /// Session identity.
    pub session_id: String,
    /// Current fencing generation.
    pub generation: u64,
    /// Current owner.
    pub owner: ControlOwner,
    /// Human principal when `owner` is human.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    /// Worker process epoch.
    pub epoch: String,
    /// Expiry time in Unix milliseconds. Zero for the agent.
    #[serde(default)]
    pub expires_at_ms: i64,
}

/// A human control operation. No command enables cookies, arbitrary host
/// access, model-written JavaScript, uploads, or another transport.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ControlCommand {
    /// Navigate the owned page.
    Navigate {
        /// Absolute http or https destination.
        url: String,
    },
    /// Click one CSS selector.
    Click {
        /// CSS selector.
        selector: String,
    },
    /// Replace the value of one input. The text is never audited.
    Type {
        /// CSS selector.
        selector: String,
        /// Input value. Never echoed into events or audit.
        text: String,
    },
    /// Read text from one CSS selector.
    ExtractText {
        /// CSS selector.
        selector: String,
    },
}

impl ControlCommand {
    /// Stable label safe for events and audit.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Navigate { .. } => "navigate",
            Self::Click { .. } => "click",
            Self::Type { .. } => "type",
            Self::ExtractText { .. } => "extract_text",
        }
    }
}

/// One fenced command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlCommandRequest {
    /// Session identity.
    pub session_id: String,
    /// Lease generation returned by acquisition.
    pub generation: u64,
    /// Worker process epoch returned by acquisition.
    pub epoch: String,
    /// Command to execute.
    pub command: ControlCommand,
}

/// Bounded page evidence returned after a command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlPage {
    /// Current URL.
    pub url: String,
    /// Current title.
    pub title: String,
    /// Truncated visible page text.
    pub text: String,
}

/// Result of one fenced command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlCommandResponse {
    /// Session identity.
    pub session_id: String,
    /// Generation that executed the command.
    pub generation: u64,
    /// Stable command label.
    pub command: String,
    /// Command output (extracted text for `extract_text`).
    #[serde(default)]
    pub output: String,
    /// Page evidence after the command.
    pub page: ControlPage,
}

/// Explicit hand-back to the Worker's agent owner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlHandBackRequest {
    /// Session identity.
    pub session_id: String,
    /// Current lease generation.
    pub generation: u64,
    /// Current Worker process epoch.
    pub epoch: String,
}

/// One redacted control decision.
///
/// Entries never contain cookies, typed input, navigation URLs, page text, or
/// host paths. `detail` is a command label or a stable error code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlAuditEntry {
    /// Decision time in Unix milliseconds.
    pub at: i64,
    /// `acquire`, `command`, `hand_back`, `expire`, or `reject`.
    pub action: String,
    /// Session identity.
    pub session_id: String,
    /// Authenticated principal.
    pub principal: String,
    /// Generation observed at the decision.
    pub generation: u64,
    /// `accepted` or `rejected`.
    pub outcome: String,
    /// Command label or stable error code.
    pub detail: String,
}
