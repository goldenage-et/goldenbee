//! Native agents built from loaded asset definitions and Worker commands.

use crate::agents::definition::AgentSpec;
use crate::agents::skills::{self, SkillDisclosure};
use crate::agents::{AgentDefinition, SkillAsset};
use crate::commands::parse;
use crate::commands::CommandAsset;
use crate::data::memory::{preload_callback, NativeMemory};
use crate::execution::plugins::AgentCallbacks;
use crate::execution::providers::{ProviderRegistry, ProviderRequirements};
use crate::tools::ToolKernel;
use crate::worker::WorkerResult;
use adk_rust::prelude::LlmAgentBuilder;
use adk_rust::{Agent, Llm, Tool, Toolset};
use common::ApiError;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) struct AgentBindings<'a> {
    pub(crate) tools: &'a BTreeMap<String, Arc<dyn Tool>>,
    pub(crate) toolsets: &'a BTreeMap<String, Arc<dyn Toolset>>,
    pub(crate) mcp_ids: &'a std::collections::BTreeSet<String>,
    pub(crate) effect_tools: std::collections::BTreeSet<String>,
    pub(crate) prompts: &'a BTreeMap<String, String>,
    pub(crate) skills: &'a [SkillAsset],
    pub(crate) skill_disclosure: &'a SkillDisclosure,
    pub(crate) kernel: Option<&'a ToolKernel>,
    pub(crate) callbacks: Option<&'a (dyn Fn() -> AgentCallbacks + Send + Sync)>,
    pub(crate) memory: Option<&'a Arc<NativeMemory>>,
}

pub(crate) fn build_loaded_agent(
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
    let model = resolve_model(
        &definition.name,
        &definition.model,
        &requirements,
        providers,
    )?;
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
    let skill_tools = bound_tool_map(&definition.tools, tools);
    builder = skills::bind(
        builder,
        skills,
        &definition.skills,
        bindings.skill_disclosure,
        skill_tools,
        bindings.kernel,
    )?;
    let agent_callbacks = callbacks_with_memory(callbacks, memory, &definition.name)?;
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
            model: definition.model.clone(),
            tools: bound_tools,
            toolsets: bound_toolsets,
            mcp_servers,
            skills: definition.skills.clone(),
        },
    ))
}

/// Build the narrowed native agent for one Worker-defined command.
///
/// The command reuses the target agent's declared tools, toolsets, skills, and
/// MCP connections. A non-empty `allowed_tools` list restricts the turn to that
/// subset of the target's declared tools and drops unverifiable dynamic
/// surfaces. It can never add a tool.
pub(crate) fn build_command_agent(
    target_agent: &str,
    command: &CommandAsset,
    spec: &AgentSpec,
    bindings: &AgentBindings<'_>,
    providers: &Arc<ProviderRegistry>,
) -> WorkerResult<(Arc<dyn Agent>, Vec<String>)> {
    let model_reference = command.model.as_deref().unwrap_or(&spec.model);
    let requirements = ProviderRequirements {
        modalities: vec!["text".into()],
        tools: if command.allowed_tools.is_empty() {
            spec.tools.len() + spec.toolsets.len()
        } else {
            command.allowed_tools.len()
        },
        max_tokens: 0,
    };
    let model = resolve_model(&command.name, model_reference, &requirements, providers)?;
    let template = Arc::new(command.clone());
    let description = if command.description.trim().is_empty() {
        format!("Worker command '{}'", command.name)
    } else {
        command.description.clone()
    };
    let mut builder = LlmAgentBuilder::new(command_route(command))
        .description(description)
        .model(model)
        .instruction_provider(Box::new(move |ctx| {
            let template = template.clone();
            Box::pin(async move {
                let input = ctx
                    .user_content()
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        adk_rust::Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                parse::prepare(&template, &input)
                    .map(|prepared| prepared.prompt)
                    .map_err(|error| {
                        adk_rust::AdkError::agent(format!("{}: {}", error.code, error.message))
                    })
            })
        }));
    let selected = if command.allowed_tools.is_empty() {
        spec.tools.clone()
    } else {
        let mut selected = Vec::new();
        for name in &command.allowed_tools {
            let tool = spec
                .tools
                .iter()
                .find(|tool| tool.name() == name)
                .cloned()
                .ok_or_else(|| {
                    ApiError::new(
                        "asset.command_tool_unknown",
                        format!(
                            "command '{}' narrows to tool '{}' but agent '{target_agent}' does not declare it",
                            command.name, name,
                        ),
                    )
                })?;
            selected.push(tool);
        }
        selected
    };
    for tool in &selected {
        if bindings.effect_tools.contains(tool.name()) {
            builder = builder.require_tool_confirmation(tool.name().to_string());
        }
        builder = builder.tool(tool.clone());
    }
    if command.allowed_tools.is_empty() {
        for toolset in &spec.toolsets {
            builder = builder.toolset(toolset.clone());
        }
    }
    let selected_skills: Vec<String> = if command.allowed_tools.is_empty() {
        spec.skills.clone()
    } else {
        Vec::new()
    };
    let mut skill_tools = BTreeMap::new();
    for tool in &selected {
        skill_tools.insert(tool.name().to_string(), tool.clone());
    }
    builder = skills::bind(
        builder,
        bindings.skills,
        &selected_skills,
        bindings.skill_disclosure,
        skill_tools,
        bindings.kernel,
    )?;
    let agent_callbacks =
        callbacks_with_memory(bindings.callbacks, bindings.memory, &command_route(command))?;
    builder = agent_callbacks.apply(builder);
    let agent = builder.build().map_err(|error| {
        ApiError::new(
            "asset.command_agent_failed",
            format!("command '{}' failed to build: {error}", command.name),
        )
    })?;
    let mcp_servers = if command.allowed_tools.is_empty() {
        spec.mcp_servers.clone()
    } else {
        Vec::new()
    };
    Ok((Arc::new(agent), mcp_servers))
}

/// Stable internal agent route for one command. It is an admission route, not
/// an advertised capability.
pub(crate) fn command_route(command: &CommandAsset) -> String {
    format!("command:{}", command.name)
}

fn resolve_model(
    owner: &str,
    reference: &str,
    requirements: &ProviderRequirements,
    providers: &Arc<ProviderRegistry>,
) -> WorkerResult<Arc<dyn Llm>> {
    if providers.profiles().contains_key(reference) {
        return providers
            .resolve_primary(reference, 0, requirements)
            .map(|resolved| resolved.model)
            .map_err(|error| {
                ApiError::new(
                    "asset.model_unresolved",
                    format!(
                        "{owner} model reference '{reference}' failed preflight: {}",
                        common::redact(&error.message)
                    ),
                )
            });
    }
    // Legacy inline diagnostic references are environment-free; provider
    // profiles remain the only way to select a credentialed model.
    crate::execution::model::model_for(reference).map_err(|error| {
        ApiError::new(
            "asset.model_unresolved",
            format!(
                "{owner} model reference '{reference}' failed: {}",
                common::redact(&format!("{error}"))
            ),
        )
    })
}

fn bound_tool_map(
    names: &[String],
    tools: &BTreeMap<String, Arc<dyn Tool>>,
) -> BTreeMap<String, Arc<dyn Tool>> {
    let mut bound = BTreeMap::new();
    for name in names {
        if let Some(tool) = tools.get(name) {
            bound.insert(name.clone(), tool.clone());
        }
    }
    bound
}

fn callbacks_with_memory(
    callbacks: Option<&(dyn Fn() -> AgentCallbacks + Send + Sync)>,
    memory: Option<&Arc<NativeMemory>>,
    agent: &str,
) -> WorkerResult<AgentCallbacks> {
    let mut agent_callbacks = callbacks.map(|factory| factory()).unwrap_or_default();
    if let Some(memory) = memory {
        let preload = preload_callback(memory.clone()).map_err(|error| {
            ApiError::new(
                "asset.memory_callback_failed",
                format!("agent '{agent}' could not bind native memory recall: {error}"),
            )
        })?;
        agent_callbacks = agent_callbacks.with_before_model(preload);
    }
    Ok(agent_callbacks)
}
