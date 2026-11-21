//! Runtime-local subagent execution.
//!
//! The runtime owns the mechanism: durable child admission on the existing
//! task path, narrowed model/tool/permission policy, bounded depth and fanout,
//! background lifecycle operations, cancellation cascade, and bounded result
//! merge. The Worker Package owns the named definitions and the ceilings.
//!
//! Cross-Worker teams stay Gateway-owned; this module never routes between
//! Workers.

mod config;
mod lifecycle;
mod tools;

pub use config::{
    SubagentConfig, SubagentDefinition, DEFAULT_RESULT_BYTES, DEFAULT_WAIT_TIMEOUT_MS,
};
pub use lifecycle::SpawnRequest;

use crate::policy::PermissionMode;
use crate::{ApiError, Result};
use common::{ProfileRevision, SubagentPolicy};

/// Narrow one pinned parent profile revision for a child.
///
/// The derived revision is deterministic: admission and execution compute the
/// same value from the same immutable inputs, so no extra profile store is
/// required and restart re-entry cannot drift.
pub(crate) fn narrowed_revision(
    revision: &ProfileRevision,
    policy: &SubagentPolicy,
) -> Result<ProfileRevision> {
    let mut narrowed = revision.clone();
    if let Some(model) = &policy.model {
        if narrowed.members.is_empty() {
            return Err(ApiError::new(
                "subagent.model_unsupported",
                "a base agent owns its native model reference and cannot be re-modelled",
            )
            .with("model", model.clone()));
        }
        narrowed.model = model.clone();
    }
    Ok(narrowed)
}

/// Frozen permission-mode ceiling for one child, when it is well formed.
pub(crate) fn mode_ceiling(policy: &SubagentPolicy) -> Option<PermissionMode> {
    policy
        .permission_mode
        .as_deref()
        .and_then(PermissionMode::from_wire)
}

/// Build the runtime-local subagent tools for one Worker definition.
pub(crate) fn registrations(
    kernel: &crate::tools::ToolKernel,
    runtime: std::sync::Arc<std::sync::OnceLock<crate::Runtime>>,
    config: &SubagentConfig,
) -> Vec<(String, std::sync::Arc<dyn adk_rust::Tool>, Vec<String>)> {
    tools::registrations(kernel, runtime, config)
}

impl crate::worker::Worker {
    /// Enable runtime-local subagents with this Worker-declared policy.
    ///
    /// Registering the policy is what exposes the lifecycle tools and what
    /// bounds every child. A Worker that does not call this rejects every
    /// subagent operation.
    pub fn subagents(self, config: SubagentConfig) -> Self {
        self.with_subagent_config(config)
    }
}
