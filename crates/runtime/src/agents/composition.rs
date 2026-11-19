//! Native ADK agent composition for profile revisions.

use adk_rust::prelude::*;
use adk_rust::{Agent, Llm};
use common::{Limits, ProfileRevision};
use std::sync::Arc;

/// Build the native ADK agent described by an immutable profile revision.
///
/// Compositions reuse native sequential, parallel, loop, and coordinator
/// patterns. Unknown compositions fall back to a single agent, and neither the
/// package nor the runtime introduces another model loop.
pub fn compose_agent(
    profile: &ProfileRevision,
    model: Arc<dyn Llm>,
    max_iterations: u32,
) -> adk_rust::Result<Arc<dyn Agent>> {
    let leaf = |name: &str| -> adk_rust::Result<Arc<dyn Agent>> {
        let instruction = if profile.members.is_empty() {
            profile.instruction.clone()
        } else {
            format!(
                "{}\nYou are the '{name}' specialist. Return only your own result.",
                profile.instruction
            )
        };
        let agent = LlmAgentBuilder::new(name)
            .description(format!("{name} native ADK agent"))
            .instruction(instruction)
            .model(model.clone())
            .max_iterations(max_iterations.max(1))
            .build()?;
        Ok(Arc::new(agent))
    };

    match profile.composition.as_str() {
        "sequential" => {
            let children = profile
                .members
                .iter()
                .map(|member| leaf(member))
                .collect::<adk_rust::Result<Vec<_>>>()?;
            Ok(Arc::new(SequentialAgent::new("sequential", children)))
        }
        "parallel" => {
            let children = profile
                .members
                .iter()
                .map(|member| leaf(member))
                .collect::<adk_rust::Result<Vec<_>>>()?;
            Ok(Arc::new(ParallelAgent::new("parallel", children)))
        }
        "loop" => {
            let children = profile
                .members
                .iter()
                .map(|member| leaf(member))
                .collect::<adk_rust::Result<Vec<_>>>()?;
            Ok(Arc::new(
                LoopAgent::new("loop", children).with_max_iterations(max_iterations.clamp(1, 10)),
            ))
        }
        "team" => {
            let children = profile
                .members
                .iter()
                .map(|member| leaf(member))
                .collect::<adk_rust::Result<Vec<_>>>()?;
            let mut builder = LlmAgentBuilder::new("coordinator")
                .description("Coordinator for a native ADK agent team")
                .instruction(profile.instruction.clone())
                .model(model.clone())
                .max_iterations(max_iterations.max(1));
            for child in children {
                builder = builder.sub_agent(child);
            }
            Ok(Arc::new(builder.build()?))
        }
        _ => leaf("agent"),
    }
}

/// Agent iteration ceiling derived from profile limits.
pub fn effective_iterations(limits: &Limits) -> u32 {
    if limits.max_model_calls == 0 {
        4
    } else {
        limits.max_model_calls.clamp(1, 20)
    }
}
