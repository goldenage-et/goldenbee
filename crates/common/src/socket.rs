//! Authenticated duplex Gateway control frames.
//!
//! The socket is an adapter over the existing task, stream, cancellation, and
//! approval behavior. It adds command identity, resource scope, and explicit
//! acknowledgments; it does not own a second event journal or task state.
//!
//! Authentication uses a short-lived one-use ticket obtained through the
//! authenticated HTTP surface. The caller bearer token never appears in the
//! upgrade query string.

use crate::error::ApiError;
use crate::gateway::{GatewayApproval, GatewaySubmitRequest, GatewaySubmitResponse, GatewayTask};
use crate::profile::ApprovalDecision;
use crate::stream::StreamEvent;
use serde::{Deserialize, Serialize};

/// Duplex protocol schema version.
pub const SOCKET_PROTOCOL_VERSION: u32 = 1;

/// Default per-connection inbound frame ceiling.
pub const SOCKET_MAX_FRAME_BYTES: usize = 64 * 1024;

/// Default bounded subscriber queue per connection.
pub const SOCKET_SUBSCRIBER_QUEUE: usize = 64;

/// Default maximum concurrent task subscriptions on one connection.
pub const SOCKET_MAX_SUBSCRIPTIONS: usize = 4;

/// Default idle timeout before a silent client is disconnected.
pub const SOCKET_IDLE_TIMEOUT_MS: u64 = 60_000;

/// Default server heartbeat interval.
pub const SOCKET_HEARTBEAT_MS: u64 = 20_000;

/// Default bound on one frame send before the connection is closed.
pub const SOCKET_SEND_TIMEOUT_MS: u64 = 5_000;

/// Default short-lived one-use ticket lifetime.
pub const SOCKET_TICKET_TTL_MS: u64 = 30_000;

/// Maximum tickets retained by one Gateway process.
pub const SOCKET_MAX_TICKETS: usize = 256;

/// One short-lived, one-use connection ticket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketTicket {
    /// Opaque ticket value. It is not a caller bearer credential.
    pub ticket: String,
    /// Gateway-clock expiry in Unix milliseconds.
    pub expires_at_ms: i64,
    /// Upgrade path that accepts this ticket.
    pub path: String,
}

/// Resource scope a command claims. The adapter rejects a frame whose scope
/// does not match the command's actual target.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SocketScope {
    /// Explicit Worker Instance, when the command targets one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// Gateway external execution identity, when the command targets one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

/// Typed command carried by one client frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SocketCommand {
    /// Subscribe to one routed task's replay-then-live durable events.
    Subscribe {
        /// Gateway external execution identity.
        task_id: String,
        /// Durable cursor to resume after.
        #[serde(default)]
        after: u64,
    },
    /// Submit or reconcile one task through the existing Gateway path.
    Submit {
        /// Existing Gateway submission payload, including idempotency key.
        request: GatewaySubmitRequest,
    },
    /// Cancel one routed task through the existing Worker cancellation path.
    Cancel {
        /// Gateway external execution identity.
        task_id: String,
    },
    /// Decide one Worker-owned approval through the existing Gateway path.
    Approval {
        /// Gateway external execution identity.
        task_id: String,
        /// Worker approval identity.
        approval_id: String,
        /// Exact observed decision and digest.
        decision: ApprovalDecision,
    },
}

impl SocketCommand {
    /// Stable command label for diagnostics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Subscribe { .. } => "subscribe",
            Self::Submit { .. } => "submit",
            Self::Cancel { .. } => "cancel",
            Self::Approval { .. } => "approval",
        }
    }

    /// Target task identity, when the command addresses one.
    pub fn task_id(&self) -> Option<&str> {
        match self {
            Self::Subscribe { task_id, .. } | Self::Cancel { task_id } => Some(task_id),
            Self::Approval { task_id, .. } => Some(task_id),
            Self::Submit { .. } => None,
        }
    }
}

/// One typed client command frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketCommandFrame {
    /// Duplex protocol schema version.
    pub schema_version: u32,
    /// Caller-assigned command identity, unique within this connection's
    /// command sequence. A reconnect may repeat it with the same payload.
    pub command_id: String,
    /// Claimed resource scope.
    pub scope: SocketScope,
    /// Command body.
    pub command: SocketCommand,
}

impl SocketCommandFrame {
    /// Build a command frame at the current schema version.
    pub fn new(command_id: impl Into<String>, scope: SocketScope, command: SocketCommand) -> Self {
        Self {
            schema_version: SOCKET_PROTOCOL_VERSION,
            command_id: command_id.into(),
            scope,
            command,
        }
    }

    /// Reject a frame that lies about its resource scope.
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.schema_version != SOCKET_PROTOCOL_VERSION {
            return Err(ApiError::new(
                "socket.unsupported_protocol",
                "the socket frame schema version is not supported",
            )
            .with("expected", SOCKET_PROTOCOL_VERSION.to_string())
            .with("received", self.schema_version.to_string()));
        }
        if self.command_id.trim().is_empty() || self.command_id.len() > 128 {
            return Err(ApiError::new(
                "socket.invalid_frame",
                "command_id must be 1-128 characters",
            ));
        }
        match (&self.scope.task_id, self.command.task_id()) {
            (Some(scoped), Some(target)) if scoped == target => {}
            (Some(_), Some(_)) => {
                return Err(ApiError::new(
                    "socket.scope_mismatch",
                    "the frame scope does not match the command task",
                ));
            }
            (None, Some(_)) => {
                return Err(ApiError::new(
                    "socket.scope_mismatch",
                    "a task command must carry its task identity in scope",
                ));
            }
            (Some(_), None) => {
                return Err(ApiError::new(
                    "socket.scope_mismatch",
                    "a submission command must not carry a task scope",
                ));
            }
            (None, None) => {}
        }
        if let SocketCommand::Submit { request } = &self.command {
            let expected = request.worker.as_deref();
            if let Some(scoped) = self.scope.worker.as_deref() {
                if expected != Some(scoped) {
                    return Err(ApiError::new(
                        "socket.scope_mismatch",
                        "the frame scope does not match the submission target",
                    ));
                }
            }
        } else if self.scope.worker.is_some() {
            return Err(ApiError::new(
                "socket.scope_mismatch",
                "only a submission command may carry a Worker scope",
            ));
        }
        Ok(())
    }
}

/// Explicit successful command outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SocketOutcome {
    /// The retained/live subscription is open.
    Subscribed {
        /// Gateway external execution identity.
        task_id: String,
        /// Cursor the stream resumed after.
        after: u64,
    },
    /// The submission was accepted or replayed.
    Submitted {
        /// Existing Gateway acceptance result.
        response: Box<GatewaySubmitResponse>,
    },
    /// The cancellation projection was returned.
    Cancelled {
        /// Existing Gateway task projection.
        task: Box<GatewayTask>,
    },
    /// The Worker-recorded approval decision.
    Approval {
        /// Existing Gateway approval projection.
        approval: Box<GatewayApproval>,
    },
}

/// One typed server frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum SocketServerFrame {
    /// Explicit command acknowledgment carrying the same command identity.
    Ack {
        /// Command identity from the client frame.
        command_id: String,
        /// Successful outcome.
        outcome: SocketOutcome,
    },
    /// Typed command or connection error.
    Error {
        /// Command identity when the frame was attributable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
        /// Stable error, never containing a bearer or ticket value.
        error: ApiError,
    },
    /// One durable or ephemeral event for an acknowledged subscription.
    Event {
        /// Command identity of the subscription.
        command_id: String,
        /// Gateway external execution identity.
        task_id: String,
        /// Existing durable event envelope.
        event: StreamEvent,
    },
    /// Liveness frame. Clients may answer with a WebSocket ping/pong or any
    /// command; a silent connection is closed at the idle bound.
    Heartbeat {
        /// Gateway time in Unix milliseconds.
        at: i64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Limits;

    fn submit(command_id: &str, scope_worker: Option<&str>) -> SocketCommandFrame {
        SocketCommandFrame::new(
            command_id,
            SocketScope {
                worker: scope_worker.map(str::to_string),
                task_id: None,
            },
            SocketCommand::Submit {
                request: GatewaySubmitRequest {
                    worker: Some("worker-a".into()),
                    agent: "general".into(),
                    session_id: "s".into(),
                    input: "hello".into(),
                    idempotency_key: "k".into(),
                    profile: None,
                    team: None,
                    team_version: None,
                    limits: Limits::default(),
                },
            },
        )
    }

    #[test]
    fn scope_must_match_the_command_target() {
        assert!(submit("c1", Some("worker-a")).validate().is_ok());
        assert_eq!(
            submit("c1", Some("worker-b")).validate().unwrap_err().code,
            "socket.scope_mismatch"
        );
        let task = SocketCommandFrame::new(
            "c2",
            SocketScope {
                worker: None,
                task_id: Some("other".into()),
            },
            SocketCommand::Cancel {
                task_id: "exec-1".into(),
            },
        );
        assert_eq!(task.validate().unwrap_err().code, "socket.scope_mismatch");
    }

    #[test]
    fn unsupported_protocol_and_missing_command_identity_are_typed() {
        let mut frame = submit("c1", Some("worker-a"));
        frame.schema_version = 99;
        assert_eq!(
            frame.validate().unwrap_err().code,
            "socket.unsupported_protocol"
        );
        let mut frame = submit("", Some("worker-a"));
        frame.schema_version = SOCKET_PROTOCOL_VERSION;
        assert_eq!(frame.validate().unwrap_err().code, "socket.invalid_frame");
    }
}
