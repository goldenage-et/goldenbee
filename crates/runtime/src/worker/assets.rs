//! Automatic prompt, skill, and agent-definition loading.

use crate::agents::definition::AgentSpec;
use crate::agents::graph::GraphRuntime;
use crate::agents::{AgentDefinition, AgentDescriptor, AgentOrigin, SkillAsset};
use crate::context::ContextBundle;
use crate::data::graph::NativeKnowledgeGraph;
use crate::data::knowledge_tools::{ObserveKnowledgeTool, RecallKnowledgeTool};
use crate::data::memory::{NativeMemory, RememberMemoryTool};
use crate::execution::plugins::AgentCallbacks;
use crate::execution::providers::{ProviderRegistry, ProviderRequirements};
use crate::worker::agent::{build_loaded_agent, AgentBindings};
use crate::worker::config::WorkerSettings;
use crate::worker::instance::WorkerInner;
use crate::worker::{ManualAgent, Worker, WorkerResult};
use crate::AgentRequirement;
use adk_rust::skill::parse_skill_markdown;
use adk_rust::{Agent, Tool, Toolset};
use common::ApiError;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Resolved assets and settings for one immutable revision.
pub(super) struct Assets {
    pub(super) config: WorkerSettings,
    pub(super) agents: BTreeMap<String, Arc<dyn Agent>>,
    pub(super) catalog: BTreeMap<String, AgentDescriptor>,
    pub(super) prompts: BTreeMap<String, String>,
    pub(super) skills: Vec<SkillAsset>,
    pub(super) specs: BTreeMap<String, AgentSpec>,
    pub(super) agent_providers: BTreeMap<String, String>,
    pub(super) agent_requirements: BTreeMap<String, AgentRequirement>,
    /// Configured memory files for this immutable asset revision.
    pub(super) context: ContextBundle,
}

impl Assets {
    pub(super) fn load(
        settings: &WorkerSettings,
        worker: &Worker,
        graphs: &BTreeMap<String, GraphRuntime>,
        memory: Option<&Arc<NativeMemory>>,
        providers: &Arc<ProviderRegistry>,
        graph: Option<&Arc<NativeKnowledgeGraph>>,
    ) -> WorkerResult<Self> {
        Self::load_parts(
            settings,
            &worker.manual_agents,
            graphs,
            &worker.tools,
            &worker.toolsets,
            &worker.effect_approvals,
            worker.callbacks.as_deref(),
            memory,
            providers,
            graph,
        )
    }

    pub(super) fn load_with(
        settings: &WorkerSettings,
        inner: &WorkerInner,
        graphs: &BTreeMap<String, GraphRuntime>,
        callbacks: Option<&(dyn Fn() -> AgentCallbacks + Send + Sync)>,
        memory: Option<&Arc<NativeMemory>>,
        providers: &Arc<ProviderRegistry>,
        graph: Option<&Arc<NativeKnowledgeGraph>>,
    ) -> WorkerResult<Self> {
        Self::load_parts(
            settings,
            &inner.manual_agents,
            graphs,
            &inner.tools,
            &inner.toolsets,
            &inner.effect_approvals,
            callbacks,
            memory,
            providers,
            graph,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn load_parts(
        settings: &WorkerSettings,
        manual_agents: &BTreeMap<String, ManualAgent>,
        graphs: &BTreeMap<String, GraphRuntime>,
        tools: &BTreeMap<String, Arc<dyn Tool>>,
        toolsets: &BTreeMap<String, Arc<dyn Toolset>>,
        effect_approvals: &BTreeMap<String, crate::EffectApprovalPolicy>,
        callbacks: Option<&(dyn Fn() -> AgentCallbacks + Send + Sync)>,
        memory: Option<&Arc<NativeMemory>>,
        providers: &Arc<ProviderRegistry>,
        graph: Option<&Arc<NativeKnowledgeGraph>>,
    ) -> WorkerResult<Self> {
        let mcp_ids: BTreeSet<String> = settings.file.mcp.servers.keys().cloned().collect();
        let mut tools = tools.clone();
        let kernel = settings.tool_kernel();
        if let Some(memory) = memory {
            let tool: Arc<dyn Tool> = Arc::new(RememberMemoryTool::new(memory.clone()));
            let tool = kernel
                .as_ref()
                .map(|kernel| {
                    kernel.wrap(
                        tool.clone(),
                        crate::tools::ToolDescriptor::new("session:write")
                            .risk(crate::tools::ToolRisk::Medium),
                    )
                })
                .unwrap_or(tool);
            tools.insert("remember_memory".into(), tool);
        }
        if let Some(graph) = graph {
            let observe: Arc<dyn Tool> = Arc::new(ObserveKnowledgeTool::new(graph.clone()));
            let observe = kernel
                .as_ref()
                .map(|kernel| {
                    kernel.wrap(
                        observe.clone(),
                        crate::tools::ToolDescriptor::new("knowledge:ingest")
                            .risk(crate::tools::ToolRisk::Medium),
                    )
                })
                .unwrap_or(observe);
            tools.insert(
                crate::data::knowledge_tools::KNOWLEDGE_OBSERVE_TOOL.into(),
                observe,
            );
            let recall: Arc<dyn Tool> = Arc::new(RecallKnowledgeTool::new(graph.clone()));
            let recall = kernel
                .as_ref()
                .map(|kernel| {
                    kernel.wrap(
                        recall.clone(),
                        crate::tools::ToolDescriptor::new("knowledge:read")
                            .risk(crate::tools::ToolRisk::Low),
                    )
                })
                .unwrap_or(recall);
            tools.insert(
                crate::data::knowledge_tools::KNOWLEDGE_RECALL_TOOL.into(),
                recall,
            );
        }
        let prompts = load_prompts(settings.prompts_dir.as_deref())?;
        let context = ContextBundle::load(&settings.file.context, &settings.config_dir)?;
        let skills = load_skills(settings.skills_dir.as_deref())?;
        let definitions = load_definitions(settings.agents_dir.as_deref())?;
        let ManualResolution {
            agents: manual_agents,
            providers: mut agent_providers,
            requirements: mut agent_requirements,
        } = resolve_manual_agents(manual_agents, providers, settings)?;
        let mut names: BTreeSet<String> = manual_agents.keys().cloned().collect();
        for id in graphs.keys() {
            if !names.insert(id.clone()) {
                return Err(ApiError::new(
                    "asset.agent_duplicate",
                    format!("graph id '{id}' is registered twice"),
                ));
            }
        }
        for definition in &definitions {
            if !names.insert(definition.name.clone()) {
                return Err(ApiError::new(
                    "asset.agent_duplicate",
                    format!(
                        "agent id '{}' is registered by package code and an agent file",
                        definition.name
                    ),
                ));
            }
        }

        let mut loaded: BTreeMap<String, Arc<dyn Agent>> = BTreeMap::new();
        let mut specs = BTreeMap::new();
        for definition in &definitions {
            let bindings = AgentBindings {
                tools: &tools,
                toolsets,
                mcp_ids: &mcp_ids,
                prompts: &prompts,
                skills: &skills,
                context: &context,
                effect_tools: effect_approvals.keys().cloned().collect(),
                callbacks,
                memory,
            };
            let (agent, spec) = build_loaded_agent(definition, &bindings, providers)?;
            if providers.profiles().contains_key(&definition.model) {
                agent_providers.insert(definition.name.clone(), definition.model.clone());
            }
            agent_requirements.insert(
                definition.name.clone(),
                AgentRequirement {
                    tools: spec.tools.len(),
                    toolsets: spec.toolsets.len(),
                },
            );
            loaded.insert(definition.name.clone(), agent);
            specs.insert(definition.name.clone(), spec);
        }

        let mut agents: BTreeMap<String, Arc<dyn Agent>> = manual_agents.clone();
        let mut catalog = BTreeMap::new();
        for (id, agent) in &manual_agents {
            catalog.insert(
                id.clone(),
                AgentDescriptor {
                    id: id.clone(),
                    origin: AgentOrigin::Manual,
                    description: agent.description().to_string(),
                    skills: Vec::new(),
                },
            );
        }
        for (id, graph) in graphs {
            agents.insert(id.clone(), graph.agent.clone());
            catalog.insert(
                id.clone(),
                AgentDescriptor {
                    id: id.clone(),
                    origin: AgentOrigin::Graph,
                    description: graph.description.clone(),
                    skills: Vec::new(),
                },
            );
        }
        for (id, agent) in loaded {
            let descriptor = definitions
                .iter()
                .find(|definition| definition.name == id)
                .ok_or_else(|| {
                    ApiError::new("asset.agent_missing", "loaded agent lost its definition")
                })?;
            catalog.insert(
                id.clone(),
                AgentDescriptor {
                    id: id.clone(),
                    origin: AgentOrigin::Loaded,
                    description: agent.description().to_string(),
                    skills: descriptor.skills.clone(),
                },
            );
            agents.insert(id, agent);
        }
        if agents.is_empty() {
            return Err(ApiError::new(
                "asset.agent_missing",
                "configuration resolves no agents; add an agent file or register one in package code",
            ));
        }
        if !agents.contains_key(&settings.file.default_agent) {
            return Err(ApiError::new(
                "asset.default_agent_missing",
                format!(
                    "default_agent '{}' is not resolved by this configuration",
                    settings.file.default_agent
                ),
            ));
        }

        Ok(Self {
            config: settings.clone(),
            agents,
            catalog,
            prompts,
            skills,
            specs,
            agent_providers,
            agent_requirements,
            context,
        })
    }
}

struct ManualResolution {
    agents: BTreeMap<String, Arc<dyn Agent>>,
    providers: BTreeMap<String, String>,
    requirements: BTreeMap<String, AgentRequirement>,
}

fn resolve_manual_agents(
    definitions: &BTreeMap<String, ManualAgent>,
    providers: &Arc<ProviderRegistry>,
    settings: &WorkerSettings,
) -> WorkerResult<ManualResolution> {
    let mut agents = BTreeMap::new();
    let mut agent_providers = BTreeMap::new();
    let mut agent_requirements = BTreeMap::new();
    for (id, definition) in definitions {
        match definition {
            ManualAgent::Built(agent) => {
                agents.insert(id.clone(), agent.clone());
            }
            ManualAgent::Defined(definition) => {
                let requirements = ProviderRequirements {
                    modalities: vec!["text".into()],
                    tools: definition.tools,
                    max_tokens: 0,
                };
                let resolved = providers
                    .resolve_primary(&definition.model, 0, &requirements)
                    .map_err(|error| {
                        ApiError::new(
                            "asset.model_unresolved",
                            format!(
                                "agent '{id}' model reference '{}' failed preflight: {}",
                                definition.model,
                                common::redact(&error.message)
                            ),
                        )
                    })?;
                let agent = match &definition.build {
                    crate::worker::ManualAgentBuild::Model(factory) => factory(resolved.model),
                    crate::worker::ManualAgentBuild::WithSettings(factory) => {
                        factory(resolved.model, settings)
                    }
                }
                .map_err(|message| {
                    ApiError::new(
                        "asset.agent_build_failed",
                        format!("agent '{id}' failed to build: {}", common::redact(&message)),
                    )
                })?;
                agent_providers.insert(id.clone(), definition.model.clone());
                agent_requirements.insert(
                    id.clone(),
                    AgentRequirement {
                        tools: definition.tools,
                        toolsets: 0,
                    },
                );
                agents.insert(id.clone(), agent);
            }
        }
    }
    Ok(ManualResolution {
        agents,
        providers: agent_providers,
        requirements: agent_requirements,
    })
}

fn load_prompts(dir: Option<&Path>) -> WorkerResult<BTreeMap<String, String>> {
    let Some(dir) = dir else {
        return Ok(BTreeMap::new());
    };
    let root = canonical_dir(dir, "prompt")?;
    let mut prompts = BTreeMap::new();
    for path in read_dir(&root, "prompt")? {
        if !path.is_file() {
            continue;
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if extension != "md" {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                ApiError::new(
                    "asset.prompt_invalid",
                    format!("prompt file '{}' has no valid name", path.display()),
                )
            })?
            .to_string();
        let safe = safe_file(&path, &root, "prompt")?;
        let content = std::fs::read_to_string(&safe).map_err(|error| {
            ApiError::new(
                "asset.prompt_unreadable",
                format!("cannot read prompt '{}': {error}", path.display()),
            )
        })?;
        if prompts.insert(id.clone(), content).is_some() {
            return Err(ApiError::new(
                "asset.prompt_duplicate",
                format!("prompt id '{id}' is defined by more than one file"),
            ));
        }
    }
    Ok(prompts)
}

fn load_skills(dir: Option<&Path>) -> WorkerResult<Vec<SkillAsset>> {
    let Some(dir) = dir else {
        return Ok(Vec::new());
    };
    let root = canonical_dir(dir, "skills")?;
    let mut skills = Vec::new();
    for entry in read_dir(&root, "skill")? {
        if !entry.is_dir() {
            continue;
        }
        let path = entry.join("SKILL.md");
        if !path.is_file() {
            continue;
        }
        let safe = safe_file(&path, &root, "skill")?;
        let content = std::fs::read_to_string(&safe).map_err(|error| {
            ApiError::new(
                "asset.skill_unreadable",
                format!("cannot read skill '{}': {error}", path.display()),
            )
        })?;
        let parsed = parse_skill_markdown(&safe, &content).map_err(|error| {
            ApiError::new(
                "asset.skill_malformed",
                format!("skill '{}' is invalid: {error}", path.display()),
            )
        })?;
        if skills
            .iter()
            .any(|skill: &SkillAsset| skill.name == parsed.name)
        {
            return Err(ApiError::new(
                "asset.skill_duplicate",
                format!("skill '{}' is defined more than once", parsed.name),
            ));
        }
        skills.push(SkillAsset {
            name: parsed.name,
            description: parsed.description,
            version: parsed.version,
            license: parsed.license,
            compatibility: parsed.compatibility,
            tags: parsed.tags,
            allowed_tools: parsed.allowed_tools,
            references: parsed.references,
            trigger: parsed.trigger,
            hint: parsed.hint,
            metadata: parsed.metadata.into_iter().collect(),
            triggers: parsed.triggers,
            path: safe,
            body: parsed.body,
        });
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(skills)
}

fn load_definitions(dir: Option<&Path>) -> WorkerResult<Vec<AgentDefinition>> {
    let Some(dir) = dir else {
        return Ok(Vec::new());
    };
    let root = canonical_dir(dir, "agent")?;
    let mut definitions = Vec::new();
    for path in read_dir(&root, "agent")? {
        if !path.is_file() {
            continue;
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if extension != "toml" {
            continue;
        }
        let safe = safe_file(&path, &root, "agent")?;
        let content = std::fs::read_to_string(&safe).map_err(|error| {
            ApiError::new(
                "asset.agent_unreadable",
                format!("cannot read agent definition '{}': {error}", path.display()),
            )
        })?;
        let definition: AgentDefinition = toml::from_str(&content).map_err(|error| {
            ApiError::new(
                "asset.agent_malformed",
                format!(
                    "agent definition '{}' is invalid TOML: {error}",
                    path.display()
                ),
            )
        })?;
        if definition.name.trim().is_empty() {
            return Err(ApiError::new(
                "asset.agent_malformed",
                format!("agent definition '{}' has an empty name", path.display()),
            ));
        }
        if definitions
            .iter()
            .any(|existing: &AgentDefinition| existing.name == definition.name)
        {
            return Err(ApiError::new(
                "asset.agent_duplicate",
                format!("agent '{}' is defined more than once", definition.name),
            ));
        }
        definitions.push(definition);
    }
    definitions.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(definitions)
}

fn canonical_dir(dir: &Path, kind: &str) -> WorkerResult<PathBuf> {
    dir.canonicalize().map_err(|error| {
        ApiError::new(
            format!("asset.{kind}_missing"),
            format!(
                "{kind} directory '{}' is not readable: {error}",
                dir.display()
            ),
        )
    })
}

/// Resolve one asset file and require it to remain inside its configured root.
///
/// The configured root itself may be a symlink or absolute path; only an asset
/// that resolves outside that root is rejected. This prevents a file or skill
/// directory symlink from exposing package-source or unrelated host files as
/// operator assets.
fn safe_file(path: &Path, root: &Path, kind: &str) -> WorkerResult<PathBuf> {
    let resolved = path.canonicalize().map_err(|error| {
        ApiError::new(
            format!("asset.{kind}_unreadable"),
            format!("cannot resolve {kind} '{}': {error}", path.display()),
        )
    })?;
    if !resolved.starts_with(root) {
        return Err(ApiError::new(
            "asset.path_denied",
            format!(
                "{kind} '{}' resolves outside the configured asset directory",
                path.display()
            ),
        ));
    }
    Ok(resolved)
}

fn read_dir(dir: &Path, kind: &str) -> WorkerResult<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        ApiError::new(
            format!("asset.{}_missing", kind),
            format!(
                "{kind} directory '{}' is not readable: {error}",
                dir.display()
            ),
        )
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            ApiError::new(
                format!("asset.{}_unreadable", kind),
                format!("cannot read {kind} directory '{}': {error}", dir.display()),
            )
        })?;
        paths.push(entry.path());
    }
    paths.sort();
    Ok(paths)
}
