//! Manually registered native diagnostic agent.
//!
//! The agent declares a logical model reference. The Worker resolves it
//! through the operator-selected instance configuration and passes the
//! constructed native model to this factory, so package code never reads a
//! provider endpoint or credential from the environment.
//!
//! The compiled sandbox tool is wrapped by the shared runtime tool kernel so a
//! manually registered agent uses the same limits, events, evidence, and audit
//! path as a file-defined agent.

use crate::callbacks::native_callbacks;
use crate::sandbox::{default_sandbox_workspace, sandbox_policy};
use adk_rust::prelude::LlmAgentBuilder;
use adk_rust::{Agent, Llm, Tool};
use runtime::native::SandboxCommandTool;
use runtime::worker::{ToolDescriptor, ToolEffect, ToolRisk, WorkerSettings};
use std::sync::Arc;

/// Logical provider reference used by the manual diagnostic agent.
pub(crate) const MANUAL_MODEL: &str = "deterministic";

/// Number of tools the manual agent binds, used for admission preflight.
pub(crate) const MANUAL_TOOLS: usize = 1;

/// Build the manual agent from the resolved native model and instance kernel.
pub(crate) fn manual_agent(
    model: Arc<dyn Llm>,
    settings: &WorkerSettings,
) -> Result<Arc<dyn Agent>, String> {
    let policy =
        sandbox_policy(default_sandbox_workspace()).with_rules(settings.sandbox_rules().clone());
    let sandbox: Arc<dyn Tool> = Arc::new(SandboxCommandTool::new(policy));
    let sandbox = match settings.tool_kernel() {
        Some(kernel) => kernel.wrap(
            sandbox,
            ToolDescriptor::new("exec:command")
                .risk(ToolRisk::High)
                .effect(ToolEffect::None),
        ),
        None => sandbox,
    };
    let builder = LlmAgentBuilder::new("manual")
        .description("Manual native ADK diagnostic agent")
        .instruction("You are the manually registered Goldenbee diagnostic agent.")
        .model(model)
        .tool(sandbox);
    let builder = native_callbacks().apply(builder);
    let agent = builder.build().map_err(|error| error.to_string())?;
    Ok(Arc::new(agent))
}
