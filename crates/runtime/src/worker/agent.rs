//! Native agents built from loaded asset definitions.

use crate::agents::definition::{skill_index, AgentSpec};
use crate::agents::{AgentDefinition, SkillAsset};
use crate::data::memory::{preload_callback, NativeMemory};
use crate::execution::plugins::AgentCallbacks;
use crate::execution::providers::{ProviderRegistry, ProviderRequirements};
use crate::worker::WorkerResult;
use adk_rust::prelude::LlmAgentBuilder;
use adk_rust::{Agent, Tool, Toolset};
use common::ApiError;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(super) struct AgentBindings<'a> {
    pub(super) tools: &'a BTreeMap<String, Arc<dyn Tool>>,
    pub(super) toolsets: &'a BTreeMap<String, Arc<dyn Toolset>>,
    pub(super) mcp_ids: &'a std::collections::BTreeSet<String>,
    pub(super) effect_tools: std::collections::BTreeSet<String>,
    pub(super) prompts: &'a BTreeMap<String, String>,
    pub(super) skills: &'a [SkillAsset],
    pub(super) callbacks: Option<&'a (dyn Fn() -> AgentCallbacks + Send + Sync)>,
    pub(super) memory: Option<&'a Arc<NativeMemory>>,
}

pub(super) fn build_loaded_agent(
    definition: &AgentDefinition,
    bindings: &AgentBindings<'_>,
    providers: &Arc<ProviderRegistry>,
) -> WorkerResult<(Arc<dyn Agent>, AgentSpec)> {
    let tools = bindings.tools;
    let toolsets = bindings.toolsets;
    let mcp_ids = bindings.mcp_ids;
    let prompts = bindings.prompts;
    let skills = bindings.skills;
    let callbacks = bindings.callbacks;
    let memory = bindings.memory;
    let requirements = ProviderRequirements {
        modalities: vec!["text".into()],
        tools: definition.tools.len() + definition.toolsets.len(),
        max_tokens: 0,
    };
    let model = if providers.profiles().contains_key(&definition.model) {
        providers
            .resolve_primary(&definition.model, 0, &requirements)
            .map_err(|error| {
                ApiError::new(
                    "asset.model_unresolved",
                    format!(
                        "agent '{}' model reference '{}' failed preflight: {}",
                        definition.name,
                        definition.model,
                        common::redact(&error.message)
                    ),
                )
            })?
            .model
    } else {
        // Legacy inline diagnostic references are environment-free; provider
        // profiles remain the only way to select a credentialed model.
        crate::execution::model::model_for(&definition.model).map_err(|error| {
            ApiError::new(
                "asset.model_unresolved",
                format!(
                    "agent '{}' model reference '{}' failed: {}",
                    definition.name,
                    definition.model,
                    common::redact(&format!("{error}"))
                ),
            )
        })?
    };
    let instruction = prompts.get(&definition.prompt).cloned().ok_or_else(|| {
        ApiError::new(
            "asset.prompt_missing",
            format!(
                "agent '{}' references prompt '{}' but it was not loaded",
                definition.name, definition.prompt
            ),
        )
    })?;
    for name in &definition.skills {
        if !skills.iter().any(|skill| &skill.name == name) {
            return Err(ApiError::new(
                "asset.skill_missing",
                format!(
                    "agent '{}' references skill '{}' but it was not discovered",
                    definition.name, name
                ),
            ));
        }
    }
    let mut builder = LlmAgentBuilder::new(&definition.name)
        .instruction(instruction)
        .model(model);
    if let Some(index) = skill_index(skills, &definition.skills)? {
        builder = builder.with_skills(index);
    }
    if let Some(description) = &definition.description {
        builder = builder.description(description);
    }
    let mut bound_tools = Vec::new();
    for name in &definition.tools {
        let tool = tools.get(name).cloned().ok_or_else(|| {
            ApiError::new(
                "asset.tool_unresolved",
                format!(
                    "agent '{}' references tool '{}' but package code did not register it",
                    definition.name, name
                ),
            )
        })?;
        if bindings.effect_tools.contains(name) {
            // Native `LlmAgent` confirmation is the pause point for a
            // protected operation; the Worker runtime supplies the decision
            // handler and the exact-operation policy.
            builder = builder.require_tool_confirmation(name.clone());
        }
        bound_tools.push(tool.clone());
        builder = builder.tool(tool);
    }
    let mut bound_toolsets = Vec::new();
    let mut mcp_servers = Vec::new();
    for name in &definition.toolsets {
        if let Some(id) = name.strip_prefix("mcp:") {
            if !mcp_ids.contains(id) {
                return Err(ApiError::new(
                    "asset.toolset_unresolved",
                    format!(
                        "agent '{}' references MCP connection '{}' but it is not configured",
                        definition.name, id
                    ),
                ));
            }
            if !mcp_servers.iter().any(|server| server == id) {
                mcp_servers.push(id.to_string());
            }
            continue;
        }
        let toolset = toolsets.get(name).cloned().ok_or_else(|| {
            ApiError::new(
                "asset.toolset_unresolved",
                format!(
                    "agent '{}' references toolset '{}' but package code did not register it",
                    definition.name, name
                ),
            )
        })?;
        bound_toolsets.push(toolset.clone());
        builder = builder.toolset(toolset);
    }
    let mut agent_callbacks = callbacks.map(|factory| factory()).unwrap_or_default();
    if let Some(memory) = memory {
        let preload = preload_callback(memory.clone()).map_err(|error| {
            ApiError::new(
                "asset.memory_callback_failed",
                format!(
                    "agent '{}' could not bind native memory recall: {error}",
                    definition.name
                ),
            )
        })?;
        agent_callbacks = agent_callbacks.with_before_model(preload);
    }
    builder = agent_callbacks.apply(builder);
    let agent = builder.build().map_err(|error| {
        ApiError::new(
            "asset.agent_build_failed",
            format!("agent '{}' failed to build: {error}", definition.name),
        )
    })?;
    Ok((
        Arc::new(agent),
        AgentSpec {
            description: definition.description.clone().unwrap_or_default(),
            tools: bound_tools,
            toolsets: bound_toolsets,
            mcp_servers,
            skills: definition.skills.clone(),
        },
    ))
}
