//! Manually registered native diagnostic agent.
//!
//! The agent declares a logical model reference. The Worker resolves it
//! through the operator-selected instance configuration and passes the
//! constructed native model to this factory, so package code never reads a
//! provider endpoint or credential from the environment.

use crate::callbacks::native_callbacks;
use crate::sandbox::{default_sandbox_workspace, sandbox_tool};
use adk_rust::prelude::LlmAgentBuilder;
use adk_rust::{Agent, Llm, Tool};
use std::sync::Arc;

/// Logical provider reference used by the manual diagnostic agent.
pub(crate) const MANUAL_MODEL: &str = "deterministic";

/// Number of tools the manual agent binds, used for admission preflight.
pub(crate) const MANUAL_TOOLS: usize = 1;

/// Build the manual agent from the resolved native model.
pub(crate) fn manual_agent(model: Arc<dyn Llm>) -> Result<Arc<dyn Agent>, String> {
    let builder = LlmAgentBuilder::new("manual")
        .description("Manual native ADK diagnostic agent")
        .instruction("You are the manually registered Goldenbee diagnostic agent.")
        .model(model)
        .tool(Arc::new(sandbox_tool(default_sandbox_workspace())) as Arc<dyn Tool>);
    let builder = native_callbacks().apply(builder);
    let agent = builder.build().map_err(|error| error.to_string())?;
    Ok(Arc::new(agent))
}
