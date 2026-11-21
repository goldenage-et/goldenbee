//! The package's single Worker definition and derived capability inventory.

use crate::agent::{manual_agent, MANUAL_MODEL, MANUAL_TOOLS};
use crate::artifact::ArtifactTool;
use crate::callbacks::native_callbacks;
use crate::hooks::{audit_plugin, PackageHooks};
use crate::sandbox::{default_sandbox_workspace, sandbox_policy, sandbox_tool};
use adk_rust::Tool;
use runtime::native::{PackageManifest, SandboxCommandTool};
use runtime::worker::{ToolDescriptor, ToolEffect, ToolRisk, Worker};
use std::sync::Arc;

/// The package's single Worker definition.
pub fn worker() -> Result<Worker, String> {
    Ok(Worker::new("general", env!("CARGO_PKG_VERSION"))
        .required_permissions(["task:submit", "session:write"])
        .model_agent_with_settings(
            "manual",
            "Manual native ADK diagnostic agent",
            MANUAL_MODEL,
            MANUAL_TOOLS,
            manual_agent,
        )
        .tool(
            "exec_command",
            Arc::new(sandbox_tool(default_sandbox_workspace())) as Arc<dyn Tool>,
        )
        // Rebuild the contained command tool once the operator `[sandbox]`
        // policy is known. The static registration above keeps `manifest()`
        // truthful; this replacement projects the resolved profile, network,
        // command ceiling, proxy, and process limits onto the OS boundary.
        .tool_with_settings(
            "exec_command",
            ToolDescriptor::new("exec:command")
                .risk(ToolRisk::Medium)
                .effect(ToolEffect::None),
            |settings| {
                let policy = sandbox_policy(default_sandbox_workspace())
                    .with_rules(settings.sandbox_rules().clone());
                Arc::new(SandboxCommandTool::new(policy)) as Arc<dyn Tool>
            },
        )
        .tool("save_artifact", Arc::new(ArtifactTool) as Arc<dyn Tool>)
        .browser("browser")
        .callbacks(native_callbacks)
        .knowledge_tools()
        .adk_plugin(audit_plugin())
        .hooks(Arc::new(PackageHooks)))
}

/// Installed capability inventory derived from the one Worker definition.
pub fn manifest() -> PackageManifest {
    worker()
        .expect("general Worker definition is compiled from credential-free native pieces")
        .manifest()
}
