//! Neutral command discovery and invocation contracts.
//!
//! Commands are one runtime capability surface: neutral session/context/
//! permission commands owned by the runtime, and Worker-defined commands loaded
//! from operator-selected assets. Both surfaces share these wire types so a CLI,
//! HTTP client, or SDK renders the same inventory and results.
//!
//! A command narrows a turn. It never grants a capability, permission, model, or
//! tool that the Worker Package did not already declare.

use crate::limits::Limits;
use crate::task::SubmitTaskResponse;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Command kind advertised to clients.
pub mod kinds {
    /// Runtime-owned neutral command.
    pub const NEUTRAL: &str = "neutral";
    /// Worker asset command.
    pub const WORKER: &str = "worker";
}

/// One declared command argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandArgument {
    /// Stable argument name, used as `--name value` and `{{name}}`.
    pub name: String,
    /// Operator-facing description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the caller must supply the argument.
    #[serde(default)]
    pub required: bool,
    /// Value applied when the caller omits the argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// Discoverable command inventory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDescriptor {
    /// Command name, for example `session.info` or `review`.
    pub name: String,
    /// Operator-facing description.
    pub description: String,
    /// [`kinds::NEUTRAL`] or [`kinds::WORKER`].
    pub kind: String,
    /// `runtime` for neutral commands, `assets` for Worker asset commands.
    pub source: String,
    /// Declared arguments, in prompt order.
    #[serde(default)]
    pub arguments: Vec<CommandArgument>,
    /// Tools the command narrows the turn to. Empty inherits the target agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    /// Model reference the command selects from declared Worker models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// One discovered skill summary. Bodies stay behind progressive disclosure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDescriptor {
    /// Skill name.
    pub name: String,
    /// Skill description.
    pub description: String,
    /// Native discovery tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Supporting resource paths declared by the skill.
    #[serde(default)]
    pub references: Vec<String>,
    /// Tools the skill declares. Descriptive only; never authorization.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

/// Command invocation request shared by CLI, HTTP, and SDK clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRequest {
    /// Raw input for Worker-defined commands. Neutral commands ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// Named arguments for neutral commands.
    #[serde(default)]
    pub arguments: BTreeMap<String, String>,
    /// Session identity used by session/context commands.
    #[serde(default = "default_session")]
    pub session_id: String,
    /// Caller-scoped idempotency key for Worker commands that submit a task.
    #[serde(default)]
    pub idempotency_key: String,
    /// Explicit target agent. It must equal the command's declared agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Requested execution limits.
    #[serde(default)]
    pub limits: Limits,
}

fn default_session() -> String {
    "local-session".into()
}

/// Command invocation outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResponse {
    /// Invoked command name.
    pub command: String,
    /// [`kinds::NEUTRAL`] or [`kinds::WORKER`].
    pub kind: String,
    /// Neutral command result. Worker commands return `null` here.
    #[serde(default)]
    pub result: serde_json::Value,
    /// Admitted task for a Worker command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<SubmitTaskResponse>,
}
