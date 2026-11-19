//! Authenticated principals and management scopes.

use crate::error::{ApiError, ContractResult};
use serde::{Deserialize, Serialize};

/// Authenticated principal resolved by the receiving process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    /// Stable principal identifier.
    pub id: String,
    /// Application namespace owned by the principal.
    pub namespace: String,
    /// Granted scopes.
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl Principal {
    /// Construct a principal.
    pub fn new(id: impl Into<String>, namespace: impl Into<String>, scopes: &[&str]) -> Self {
        Self {
            id: id.into(),
            namespace: namespace.into(),
            scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// Whether the principal holds a scope.
    pub fn has(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope || s == "*")
    }

    /// Require a scope or return a stable error.
    pub fn require(&self, scope: &str) -> ContractResult<()> {
        if self.has(scope) {
            Ok(())
        } else {
            Err(ApiError::new(
                "scope.denied",
                format!("principal '{}' lacks scope '{scope}'", self.id),
            ))
        }
    }

    /// Require any one of the alternative scopes.
    pub fn require_any(&self, scopes: &[&str]) -> ContractResult<()> {
        if scopes.iter().any(|scope| self.has(scope)) {
            Ok(())
        } else {
            Err(ApiError::new(
                "scope.denied",
                format!(
                    "principal '{}' lacks required scope {}",
                    self.id,
                    scopes.join(" or ")
                ),
            ))
        }
    }

    /// Require that the principal may act in the namespace.
    pub fn require_namespace(&self, namespace: &str) -> ContractResult<()> {
        if self.namespace == namespace {
            Ok(())
        } else {
            Err(ApiError::new(
                "scope.namespace_denied",
                "principal cannot access another namespace",
            ))
        }
    }
}

/// Management scopes used by all processes.
pub mod scopes {
    /// Submit and inspect tasks.
    pub const TASK_SUBMIT: &str = "task:submit";
    /// Read task state and events.
    pub const TASK_READ: &str = "task:read";
    /// Cancel a task.
    pub const TASK_CANCEL: &str = "task:cancel";
    /// Write or resume a session.
    pub const SESSION_WRITE: &str = "session:write";
    /// Read sessions and canonical history.
    pub const SESSION_READ: &str = "session:read";
    /// Take and hand back fenced human control of an interactive session.
    pub const SESSION_CONTROL: &str = "session:control";
    /// Read and write profiles.
    pub const PROFILE_WRITE: &str = "profile:write";
    /// Read profiles.
    pub const PROFILE_READ: &str = "profile:read";
    /// Read approvals.
    pub const APPROVAL_READ: &str = "approval:read";
    /// Decide approvals.
    pub const APPROVAL_DECIDE: &str = "approval:decide";
    /// Upload or finalize artifacts.
    pub const ARTIFACT_WRITE: &str = "artifact:write";
    /// Download artifacts.
    pub const ARTIFACT_READ: &str = "artifact:read";
    /// Register or decommission Workers and teams.
    pub const ADMIN: &str = "admin";
    /// Read visible team rosters.
    pub const TEAM_READ: &str = "team:read";
    /// Create, update, validate, activate, or deactivate team rosters.
    pub const TEAM_WRITE: &str = "team:write";
    /// Manage instances and releases.
    pub const INSTANCE_ADMIN: &str = "instance:admin";
    /// Read operational evidence and metrics.
    pub const OPERATE: &str = "operate";
    /// Manage the Gateway provider catalog and write-only provider credentials.
    pub const PROVIDER_ADMIN: &str = "provider:admin";
}
