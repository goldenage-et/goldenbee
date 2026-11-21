//! Runtime-owned neutral commands plus Worker-defined command assets.
//!
//! One registry owns discovery and preparation for both surfaces. Neutral
//! commands execute against the durable Runtime; Worker commands expand from an
//! operator-selected asset and submit through the same task admission path with
//! a prebuilt narrowed agent. No command synthesizes a profile, grants a tool,
//! or bypasses the Worker's declared capabilities.

mod asset;
mod execute;
mod neutral;
pub(crate) mod parse;

use crate::agents::definition::AgentSpec;
use crate::agents::skills::SkillDisclosure;
use crate::agents::{AgentDescriptor, SkillAsset};
use crate::data::memory::NativeMemory;
use crate::execution::plugins::AgentCallbacks;
use crate::execution::providers::ProviderRegistry;
use crate::tools::ToolKernel;
use crate::worker::agent::{build_command_agent, command_route, AgentBindings};
use crate::worker::config::WorkerSettings;
use crate::worker::WorkerResult;
use adk_rust::{Agent, Tool, Toolset};
use common::{ApiError, CommandDescriptor, SkillDescriptor};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub(crate) use asset::CommandAsset;
pub(crate) use execute::{descriptors as descriptor_list, invoke};
pub(crate) use parse::PreparedCommand;

/// Immutable command inventory and preparation surface for one asset revision.
pub(crate) struct CommandRegistry {
    worker: BTreeMap<String, Arc<CommandAsset>>,
    descriptors: Vec<CommandDescriptor>,
    skills: Vec<SkillDescriptor>,
    catalog: Vec<AgentDescriptor>,
    prompts: Vec<String>,
    default_agent: String,
    required_permissions: Vec<String>,
    disclosure: SkillDisclosure,
}

impl CommandRegistry {
    /// Every discoverable command sorted by name.
    pub(crate) fn descriptors(&self) -> &[CommandDescriptor] {
        &self.descriptors
    }

    /// One discoverable command.
    pub(crate) fn descriptor(&self, name: &str) -> Option<&CommandDescriptor> {
        self.descriptors.iter().find(|command| command.name == name)
    }

    /// One Worker-defined asset, when the name resolves to one.
    pub(crate) fn command(&self, name: &str) -> Option<&Arc<CommandAsset>> {
        self.worker.get(name)
    }

    /// Discovered skill summaries.
    pub(crate) fn skills(&self) -> &[SkillDescriptor] {
        &self.skills
    }

    /// Resolved agent catalog for the active revision.
    pub(crate) fn catalog(&self) -> &[AgentDescriptor] {
        &self.catalog
    }

    /// Prompt ids available to the active revision.
    pub(crate) fn prompts(&self) -> &[String] {
        &self.prompts
    }

    /// Configured default agent.
    pub(crate) fn default_agent(&self) -> &str {
        &self.default_agent
    }

    /// Package-required permissions for the permission view.
    pub(crate) fn required_permissions(&self) -> &[String] {
        &self.required_permissions
    }

    /// Stable disclosure label for the context view.
    pub(crate) fn disclosure(&self) -> &'static str {
        self.disclosure.as_str()
    }

    /// Validate and expand one Worker command with caller input.
    pub(crate) fn prepare(&self, name: &str, input: &str) -> Result<PreparedCommand, ApiError> {
        let command = self.worker.get(name).ok_or_else(|| {
            if neutral::is_neutral(name) {
                ApiError::new(
                    "command.arguments_unexpected",
                    format!("command '{name}' does not take raw input"),
                )
            } else {
                ApiError::new(
                    "command.not_found",
                    format!("command '{name}' is not defined"),
                )
            }
        })?;
        parse::prepare(command, input)
    }

    pub(crate) fn is_neutral(&self, name: &str) -> bool {
        neutral::is_neutral(name)
    }
}

/// Inputs resolved by [`crate::worker::assets::Assets`] for command loading.
pub(crate) struct CommandInput<'a> {
    pub settings: &'a WorkerSettings,
    pub specs: &'a BTreeMap<String, AgentSpec>,
    pub prompts: &'a BTreeMap<String, String>,
    pub skills: &'a [SkillAsset],
    pub tools: &'a BTreeMap<String, Arc<dyn Tool>>,
    pub toolsets: &'a BTreeMap<String, Arc<dyn Toolset>>,
    pub mcp_ids: &'a BTreeSet<String>,
    pub providers: &'a Arc<ProviderRegistry>,
    pub kernel: Option<&'a ToolKernel>,
    pub callbacks: Option<&'a (dyn Fn() -> AgentCallbacks + Send + Sync)>,
    pub memory: Option<&'a Arc<NativeMemory>>,
    pub catalog: &'a BTreeMap<String, AgentDescriptor>,
    pub effect_tools: &'a BTreeSet<String>,
    pub declared_models: &'a BTreeSet<String>,
    pub required_permissions: &'a [String],
    pub disclosure: &'a SkillDisclosure,
}

/// Loaded commands and the prebuilt narrowed agents they route to.
pub(crate) struct LoadedCommands {
    pub registry: Arc<CommandRegistry>,
    pub agents: BTreeMap<String, Arc<dyn Agent>>,
    pub mcp: BTreeMap<String, Vec<String>>,
    /// Command routes whose model reference resolves through a provider profile.
    pub providers: BTreeMap<String, String>,
    /// Provider preflight requirements for command routes.
    pub requirements: BTreeMap<String, crate::AgentRequirement>,
}

/// Load command assets, validate their narrowing against declared assets, and
/// build one bounded agent per command.
pub(crate) fn load(input: CommandInput<'_>) -> WorkerResult<LoadedCommands> {
    let assets = asset::load(input.settings.commands_dir.as_deref())?;
    let mut commands = BTreeMap::new();
    let mut agents = BTreeMap::new();
    let mut mcp = BTreeMap::new();
    let mut providers = BTreeMap::new();
    let mut requirements = BTreeMap::new();
    let mut descriptors: Vec<CommandDescriptor> = neutral::NEUTRAL
        .iter()
        .map(neutral::NeutralCommand::descriptor)
        .collect();
    for command in assets {
        let target = command
            .agent
            .clone()
            .unwrap_or_else(|| input.settings.file.default_agent.clone());
        if !input.catalog.contains_key(&target) {
            return Err(ApiError::new(
                "asset.command_agent_unknown",
                format!(
                    "command '{}' targets agent '{}' but this Worker resolves no such agent",
                    command.name, target
                ),
            ));
        }
        let spec = input.specs.get(&target).ok_or_else(|| {
            ApiError::new(
                "asset.command_agent_unsupported",
                format!(
                    "command '{}' targets agent '{}' which is compiled package code; command narrowing requires a loaded agent",
                    command.name, target
                ),
            )
        })?;
        if let Some(model) = &command.model {
            if !input.declared_models.contains(model) {
                return Err(ApiError::new(
                    "asset.command_model_unknown",
                    format!(
                        "command '{}' references model '{}' but this Worker declares no such model",
                        command.name, model
                    ),
                ));
            }
        }
        let bindings = AgentBindings {
            tools: input.tools,
            toolsets: input.toolsets,
            mcp_ids: input.mcp_ids,
            effect_tools: input.effect_tools.clone(),
            prompts: input.prompts,
            skills: input.skills,
            skill_disclosure: input.disclosure,
            kernel: input.kernel,
            callbacks: input.callbacks,
            memory: input.memory,
        };
        let (agent, servers) =
            build_command_agent(&target, &command, spec, &bindings, input.providers)?;
        let route = command_route(&command);
        if input.catalog.contains_key(&route) || agents.contains_key(&route) {
            return Err(ApiError::new(
                "asset.command_route_duplicate",
                format!(
                    "command '{}' collides with an existing agent route",
                    command.name
                ),
            ));
        }
        let model_reference = command.model.as_deref().unwrap_or(&spec.model);
        if input.providers.profiles().contains_key(model_reference) {
            providers.insert(route.clone(), model_reference.to_string());
        }
        requirements.insert(
            route.clone(),
            crate::AgentRequirement {
                tools: if command.allowed_tools.is_empty() {
                    spec.tools.len() + spec.toolsets.len()
                } else {
                    command.allowed_tools.len()
                },
                toolsets: 0,
            },
        );
        descriptors.push(command.descriptor());
        mcp.insert(route.clone(), servers);
        agents.insert(route, agent);
        commands.insert(command.name.clone(), Arc::new(command));
    }
    descriptors.sort_by(|left, right| left.name.cmp(&right.name));
    let skills = input
        .skills
        .iter()
        .map(|skill| SkillDescriptor {
            name: skill.name.clone(),
            description: skill.description.clone(),
            tags: skill.tags.clone(),
            references: skill.references.clone(),
            allowed_tools: skill.allowed_tools.clone(),
        })
        .collect();
    let mut prompts: Vec<String> = input.prompts.keys().cloned().collect();
    prompts.sort();
    let catalog = input.catalog.values().cloned().collect();
    Ok(LoadedCommands {
        registry: Arc::new(CommandRegistry {
            worker: commands,
            descriptors,
            skills,
            catalog,
            prompts,
            default_agent: input.settings.file.default_agent.clone(),
            required_permissions: input.required_permissions.to_vec(),
            disclosure: input.disclosure.clone(),
        }),
        agents,
        mcp,
        providers,
        requirements,
    })
}

impl crate::worker::WorkerInstance {
    /// Discoverable command inventory for the active asset revision.
    pub async fn command_descriptors(&self) -> Vec<CommandDescriptor> {
        execute::descriptors(self.command_registry().await.as_ref()).await
    }

    /// Validate and invoke one command through the shared task admission path.
    pub async fn execute_command(
        &self,
        principal: &common::Principal,
        name: &str,
        request: common::CommandRequest,
    ) -> Result<common::CommandResponse, ApiError> {
        let registry = self.command_registry().await;
        execute::invoke(&registry, &self.runtime(), principal, name, request).await
    }
}
