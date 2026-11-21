//! Worker Package startup configuration, asset loading, and authoring entry point.
//!
//! A package defines one [`Worker`]. The operator selects a TOML configuration
//! file at process startup. Every relative agent, prompt, skill, and storage
//! path is resolved against that file's directory; the executable directory and
//! current working directory are never consulted.
//!
//! Native ADK supplies the agent loop, Runner, sessions, tools, callbacks,
//! plugins, launcher, and server. This module adds configuration resolution,
//! reference checking, immutable snapshots, and Worker lifecycle hooks.

pub(crate) mod agent;
pub(crate) mod assets;
mod cli;
pub(crate) mod config;
mod gateway;
mod hooks;
mod instance;
mod models;
mod profiles;

pub use crate::agents::{AgentDefinition, AgentDescriptor, AgentOrigin, SkillAsset};
pub use crate::context::{ContextConfig, MemorySource};
pub use crate::execution::plugins::AgentCallbacks;
pub use crate::process::ProcessPolicy;
pub use crate::tools::{
    BuiltinCategory, BuiltinTool, ToolDescriptor, ToolEffect, ToolKernel, ToolRisk, WebPolicy,
};
pub use config::{
    FileConfig, GatewayConfig, ModelsConfig, PathsConfig, ResolvedModels, StorageConfig,
    TokenConfig, WorkerSettings,
};
pub use hooks::{TaskHookContext, WorkerHooks};
pub use instance::{ShutdownReport, WorkerContext, WorkerInstance};

use crate::agents::graph::{build_graphs, GraphRegistration, GraphRuntime};
#[cfg(feature = "graph")]
use crate::agents::graph::{GraphRegistration as NativeGraphRegistration, GraphServices};
use crate::agents::{CapabilityRegistry, PackageManifest};
use crate::browser::{
    BrowserCapabilityToolset, BrowserController, BrowserOptions, BrowserSettings,
};
use crate::data::graph::NativeKnowledgeGraph;
use crate::data::knowledge_tools::{KNOWLEDGE_OBSERVE_TOOL, KNOWLEDGE_RECALL_TOOL};
use crate::data::memory::NativeMemory;
use crate::execution::plugins::build_plugins;
use crate::execution::profile::build_profile_agent;
use crate::execution::providers::ProviderRegistry;
use crate::execution::runner::NativeExecutor;
use crate::mcp::{McpRegistry, RemoteMcpRegistry};
use crate::sessions::FileSessionService;
use crate::tools::ToolKernelConfig;
use crate::worker::assets::Assets;
use crate::worker::config::canonical_config;
use crate::worker::hooks::CompositeHooks;
use crate::worker::instance::WorkerInner;
use crate::Runtime;
#[cfg(feature = "graph")]
use adk_rust::graph::{GraphAgent, GraphError};
use adk_rust::plugin::{Plugin, PluginManager};
use adk_rust::session::SessionService;
use adk_rust::{Agent, Llm, Tool, Toolset};
use common::{ApiError, CapabilityDescriptor};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Result type used by Worker startup, reload, and host commands.
pub type WorkerResult<T> = std::result::Result<T, ApiError>;

type AgentFactoryFn = Arc<dyn Fn(&WorkerSettings) -> WorkerResult<Arc<dyn Agent>> + Send + Sync>;
type SettingsToolFn = Arc<dyn Fn(&WorkerSettings) -> Arc<dyn Tool> + Send + Sync>;

/// A compiled tool rebuilt from resolved instance settings at startup.
struct SettingsTool {
    id: String,
    descriptor: ToolDescriptor,
    build: SettingsToolFn,
}

/// A package-owned agent constructed from the resolved instance configuration.
///
/// Manual factory agents let a package bind instance-scoped services (for
/// example a provisioned worktree) that are only known after the operator
/// configuration is loaded.
#[derive(Clone)]
struct AgentFactory {
    description: String,
    build: AgentFactoryFn,
}

/// Package-supplied native agent factory bound to a resolved provider model.
pub type ManualAgentFactory =
    Arc<dyn Fn(Arc<dyn Llm>) -> std::result::Result<Arc<dyn Agent>, String> + Send + Sync>;

/// Package-supplied agent factory that also receives the resolved instance
/// settings, including the runtime [`ToolKernel`].
pub type ManualAgentFactoryWithSettings = Arc<
    dyn Fn(Arc<dyn Llm>, &WorkerSettings) -> std::result::Result<Arc<dyn Agent>, String>
        + Send
        + Sync,
>;

pub(crate) enum ManualAgentBuild {
    Model(ManualAgentFactory),
    WithSettings(ManualAgentFactoryWithSettings),
}

/// A manually registered agent whose model reference is resolved through the
/// instance's effective provider configuration.
pub struct ManualAgentDefinition {
    /// Description used by the installed capability inventory.
    pub description: String,
    /// Logical provider reference resolved at startup and reload.
    pub model: String,
    /// Number of tools the factory binds, used for provider preflight.
    pub tools: usize,
    /// Native agent factory.
    pub(crate) build: ManualAgentBuild,
}

pub(crate) enum ManualAgent {
    Built(Arc<dyn Agent>),
    Defined(ManualAgentDefinition),
}

impl ManualAgent {
    fn description(&self) -> &str {
        match self {
            Self::Built(agent) => agent.description(),
            Self::Defined(definition) => &definition.description,
        }
    }
}

/// A package authoring entry point. One definition creates one native
/// composition; loaded file agents are the default path.
pub struct Worker {
    name: String,
    version: String,
    runtime_version: String,
    required_permissions: Vec<String>,
    manual_agents: BTreeMap<String, ManualAgent>,
    agent_factories: BTreeMap<String, AgentFactory>,
    capability_permissions: BTreeMap<String, Vec<String>>,
    graphs: BTreeMap<String, GraphRegistration>,
    pub(crate) tools: BTreeMap<String, Arc<dyn Tool>>,
    pub(crate) tool_permissions: BTreeMap<String, Vec<String>>,
    pub(crate) toolsets: BTreeMap<String, Arc<dyn Toolset>>,
    /// One process-wide pipeline applied to every tool source.
    pub(crate) kernel: ToolKernel,
    /// Tools whose instance policy is only known after configuration loads.
    /// Their static registration keeps the capability inventory truthful; the
    /// resolved tool replaces it before assets bind.
    settings_tools: Vec<SettingsTool>,
    /// Package-declared generic file/search capabilities.
    builtin_tools: BTreeSet<BuiltinTool>,
    /// Package-declared destination policy for the web tools.
    web_policy: Option<WebPolicy>,
    /// Package-declared sandbox and process policy.
    process_policy: Option<crate::ProcessPolicy>,
    /// A package-registered tool declared a workspace file effect or paths.
    pub(crate) tool_workspace_required: bool,
    /// Worker-owned status interfaces for protected effects.
    pub(crate) effect_reconcilers:
        BTreeMap<String, Arc<dyn crate::execution::operations::EffectReconciler>>,
    /// Approval policy registered with each protected native tool.
    pub(crate) effect_approvals: BTreeMap<String, crate::EffectApprovalPolicy>,
    /// Shared binding filled after durable state opens.
    pub(crate) effect_runtime: Arc<std::sync::OnceLock<crate::Runtime>>,
    hooks: Vec<Arc<dyn WorkerHooks>>,
    lifecycle: Vec<Arc<dyn crate::LifecycleOwner>>,
    callbacks: Option<Arc<dyn Fn() -> AgentCallbacks + Send + Sync>>,
    plugins: Vec<Plugin>,
    knowledge: bool,
    features: BTreeSet<String>,
    /// Package declares support for operator-selected progressive disclosure.
    progressive_skills: bool,
    browser: Option<String>,
    pub(crate) duplicate_ids: BTreeSet<String>,
}

impl Worker {
    /// Start a package definition with compiled identity.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        let kernel = ToolKernel::new();
        let effect_runtime = kernel.bind_runtime();
        Self {
            name: name.into(),
            version: version.into(),
            runtime_version: crate::RUNTIME_VERSION.to_string(),
            required_permissions: vec!["task:submit".into(), "session:write".into()],
            manual_agents: BTreeMap::new(),
            graphs: BTreeMap::new(),
            agent_factories: BTreeMap::new(),
            capability_permissions: BTreeMap::new(),
            tools: BTreeMap::new(),
            tool_permissions: BTreeMap::new(),
            toolsets: BTreeMap::new(),
            kernel,
            settings_tools: Vec::new(),
            builtin_tools: BTreeSet::new(),
            web_policy: None,
            process_policy: None,
            tool_workspace_required: false,
            effect_reconcilers: BTreeMap::new(),
            effect_approvals: BTreeMap::new(),
            effect_runtime,
            hooks: Vec::new(),
            lifecycle: Vec::new(),
            callbacks: None,
            plugins: Vec::new(),
            knowledge: false,
            features: ["agents", "runner", "sessions", "server", "plugin"]
                .into_iter()
                .map(String::from)
                .collect(),
            progressive_skills: false,
            browser: None,
            duplicate_ids: BTreeSet::new(),
        }
    }

    /// Declare the shared runtime version this package was built against.
    /// Independent package versions do not change runtime compatibility.
    ///
    /// Packages normally keep the default from [`crate::RUNTIME_VERSION`];
    /// the override exists for tests and host compatibility probes.
    pub fn runtime_version(mut self, version: impl Into<String>) -> Self {
        self.runtime_version = version.into();
        self
    }

    /// Declare one compiled native feature used by this package.
    pub fn feature(mut self, feature: impl Into<String>) -> Self {
        self.features.insert(feature.into());
        self
    }

    /// Declare the package's required permissions.
    pub fn required_permissions<I, S>(mut self, permissions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.required_permissions = permissions.into_iter().map(Into::into).collect();
        self
    }

    /// Register an additional native agent. Duplicate ids fail at startup.
    ///
    /// A prebuilt agent's tools are already owned by that agent, so package code
    /// that binds tools directly must wrap them through [`Worker::tool_kernel`]
    /// or build the agent from an `agent_from`/`model_agent_with_settings`
    /// factory. `Worker::agent` is intended for tool-free compositions and
    /// native workflows.
    pub fn agent(mut self, id: impl Into<String>, agent: Arc<dyn Agent>) -> Self {
        let id = id.into();
        let duplicate_graph = self.graphs.contains_key(&id);
        let duplicate_agent = self
            .manual_agents
            .insert(id.clone(), ManualAgent::Built(agent))
            .is_some();
        if duplicate_graph || duplicate_agent {
            self.duplicate_ids.insert(id);
        }
        self
    }

    /// Register a native agent whose model reference is resolved from the
    /// operator-selected instance configuration.
    ///
    /// The factory runs after provider preflight and receives the resolved
    /// native model. Package code never reads a provider endpoint or API key
    /// from the environment itself.
    pub fn model_agent(
        mut self,
        id: impl Into<String>,
        description: impl Into<String>,
        model: impl Into<String>,
        tools: usize,
        factory: impl Fn(Arc<dyn Llm>) -> std::result::Result<Arc<dyn Agent>, String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        let id = id.into();
        let duplicate_graph = self.graphs.contains_key(&id);
        let duplicate_agent = self
            .manual_agents
            .insert(
                id.clone(),
                ManualAgent::Defined(ManualAgentDefinition {
                    description: description.into(),
                    model: model.into(),
                    tools,
                    build: ManualAgentBuild::Model(Arc::new(factory)),
                }),
            )
            .is_some();
        if duplicate_graph || duplicate_agent {
            self.duplicate_ids.insert(id);
        }
        self
    }

    /// Register a model-resolved agent that also receives the resolved
    /// instance settings and the runtime tool kernel.
    pub fn model_agent_with_settings<F>(
        mut self,
        id: impl Into<String>,
        description: impl Into<String>,
        model: impl Into<String>,
        tools: usize,
        factory: F,
    ) -> Self
    where
        F: Fn(Arc<dyn Llm>, &WorkerSettings) -> std::result::Result<Arc<dyn Agent>, String>
            + Send
            + Sync
            + 'static,
    {
        let id = id.into();
        let duplicate_graph = self.graphs.contains_key(&id);
        let duplicate_agent = self
            .manual_agents
            .insert(
                id.clone(),
                ManualAgent::Defined(ManualAgentDefinition {
                    description: description.into(),
                    model: model.into(),
                    tools,
                    build: ManualAgentBuild::WithSettings(Arc::new(factory)),
                }),
            )
            .is_some();
        if duplicate_graph || duplicate_agent {
            self.duplicate_ids.insert(id);
        }
        self
    }

    /// Register an agent built from the resolved instance configuration.
    ///
    /// The factory runs once during startup after the operator configuration is
    /// validated. It exists for instance-scoped services such as a provisioned
    /// repository worktree. The declared description is advertised by `probe`
    /// before a configuration is selected.
    pub fn agent_from<F>(
        mut self,
        id: impl Into<String>,
        description: impl Into<String>,
        build: F,
    ) -> Self
    where
        F: Fn(&WorkerSettings) -> WorkerResult<Arc<dyn Agent>> + Send + Sync + 'static,
    {
        let id = id.into();
        if self.manual_agents.contains_key(&id)
            || self
                .agent_factories
                .insert(
                    id.clone(),
                    AgentFactory {
                        description: description.into(),
                        build: Arc::new(build),
                    },
                )
                .is_some()
        {
            self.duplicate_ids.insert(id);
        }
        self
    }

    /// Register one native ADK graph workflow. Duplicate ids fail at startup.
    ///
    /// The factory receives instance-scoped native services, including the
    /// definition-bound checkpointer that must be passed to the graph builder.
    /// The shared runtime owns the single SQLite checkpoint store; package code
    /// supplies only the graph definition.
    #[cfg(feature = "graph")]
    pub fn graph<F>(mut self, id: impl Into<String>, version: u64, build: F) -> Self
    where
        F: Fn(&GraphServices) -> std::result::Result<GraphAgent, GraphError>
            + Send
            + Sync
            + 'static,
    {
        let id = id.into();
        if self.graphs.contains_key(&id) || self.manual_agents.contains_key(&id) {
            self.duplicate_ids.insert(id.clone());
        }
        self.graphs
            .insert(id.clone(), NativeGraphRegistration::new(id, version, build));
        self
    }

    /// Declare compiled framework features beyond the shared baseline.
    pub fn features<I, S>(mut self, features: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.features.extend(features.into_iter().map(Into::into));
        self
    }

    /// Declare the permissions a registered capability actually exercises.
    pub fn capability_permissions<I, S>(mut self, id: impl Into<String>, permissions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.capability_permissions
            .insert(id.into(), permissions.into_iter().map(Into::into).collect());
        self
    }

    /// Attach a complete compiled hook implementation.
    pub fn hooks(mut self, hooks: Arc<dyn WorkerHooks>) -> Self {
        self.hooks.push(hooks);
        self
    }

    /// Attach a compiled startup hook.
    pub fn on_start(mut self, hook: Arc<dyn WorkerHooks>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Register a package-owned data lifecycle store with the shared runtime.
    pub fn lifecycle(mut self, owner: Arc<dyn crate::LifecycleOwner>) -> Self {
        self.lifecycle.push(owner);
        self
    }

    /// Attach a compiled before-task hook.
    pub fn before_task(mut self, hook: Arc<dyn WorkerHooks>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Attach a compiled after-task hook.
    pub fn after_task(mut self, hook: Arc<dyn WorkerHooks>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Attach a compiled shutdown hook.
    pub fn on_shutdown(mut self, hook: Arc<dyn WorkerHooks>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Attach per-agent native callbacks to automatically loaded agents.
    pub fn callbacks(
        mut self,
        callbacks: impl Fn() -> AgentCallbacks + Send + Sync + 'static,
    ) -> Self {
        self.callbacks = Some(Arc::new(callbacks));
        self
    }

    /// Attach a run-scope native ADK plugin.
    pub fn adk_plugin(mut self, plugin: Plugin) -> Self {
        self.plugins.push(plugin);
        self
    }

    /// Declare the package's native browser capability under `id`.
    ///
    /// The capability is still controlled by the instance's `[browser]`
    /// configuration. A package that does not declare it cannot expose it, and
    /// a Runtime Profile cannot create it.
    pub fn browser(mut self, id: impl Into<String>) -> Self {
        self.browser = Some(id.into());
        self
    }

    /// Declare that this package supports operator-selected progressive skill
    /// disclosure. Configuration may select `progressive`; it cannot create the
    /// mechanism for a package that did not declare it here.
    pub fn progressive_skills(mut self) -> Self {
        self.progressive_skills = true;
        self.features.insert("skills-progressive-disclosure".into());
        self
    }

    /// Enable the package's runtime-owned native knowledge tools.    ///
    /// The tools read and write the installed native graph store. A profile can
    /// select an agent that references them, but it cannot add a tool the
    /// package did not declare here.
    pub fn knowledge_tools(mut self) -> Self {
        self.knowledge = true;
        self
    }

    /// Enable every generic file/search tool this package may expose.
    ///
    /// The capability is compiled into the package; instance configuration
    /// supplies only the workspace and runtime limits. A profile can narrow the
    /// exposed selection, but cannot create a capability the package did not
    /// declare.
    pub fn file_tools(self) -> Self {
        self.builtin_tools(BuiltinTool::FILES)
    }

    /// Enable one generic file/search tool.
    pub fn file_tool(self, tool: BuiltinTool) -> Self {
        self.builtin_tools([tool])
    }

    /// Enable an explicit generic file/search selection.
    pub fn builtin_tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = BuiltinTool>,
    {
        self.builtin_tools.extend(tools);
        self
    }

    /// Enable the web tools with one explicit destination policy.
    pub fn web_tools(self, policy: WebPolicy) -> Self {
        self.web_policy(policy).builtin_tools(BuiltinTool::WEB)
    }

    /// Enable one web tool with one explicit destination policy.
    pub fn web_tool(self, tool: BuiltinTool, policy: WebPolicy) -> Self {
        self.web_policy(policy).builtin_tools([tool])
    }

    /// Declare the destination policy used by enabled web tools.
    ///
    /// A web tool cannot start without a policy; there is no implicit public
    /// network. The policy can only narrow what the package declared.
    pub fn web_policy(mut self, policy: WebPolicy) -> Self {
        self.web_policy = Some(policy);
        self
    }

    /// Enable the shell, PTY, background, wait, and kill tools.
    ///
    /// The policy supplies the exact sandbox boundary and process limits. A
    /// process tool cannot start without one.
    pub fn process_tools(self, policy: crate::ProcessPolicy) -> Self {
        self.process_policy(policy)
            .builtin_tools(BuiltinTool::PROCESS)
    }

    /// Enable one process tool with one explicit policy.
    pub fn process_tool(self, tool: BuiltinTool, policy: crate::ProcessPolicy) -> Self {
        self.process_policy(policy).builtin_tools([tool])
    }

    /// Declare the sandbox and process policy used by enabled process tools.
    pub fn process_policy(mut self, policy: crate::ProcessPolicy) -> Self {
        self.process_policy = Some(policy);
        self
    }

    /// One process-wide kernel handle, available to package agent factories.
    pub fn tool_kernel(&self) -> ToolKernel {
        self.kernel.clone()
    }

    /// Rebuild one registered tool from resolved instance settings at startup.
    ///
    /// The package still registers the static tool first, so `manifest()` and
    /// `capabilities` remain truthful before a configuration exists. At startup
    /// the factory receives the loaded [`WorkerSettings`] and replaces that
    /// registration; the descriptor keeps the kernel evidence accurate.
    ///
    /// The General package uses this for its contained command tool so the
    /// operator's `[sandbox]` profile, network, command ceiling, proxy, and
    /// process limits reach the OS boundary. Configuration can only narrow the
    /// compiled package policy.
    pub fn tool_with_settings<F>(
        mut self,
        id: impl Into<String>,
        descriptor: ToolDescriptor,
        factory: F,
    ) -> Self
    where
        F: Fn(&WorkerSettings) -> Arc<dyn Tool> + Send + Sync + 'static,
    {
        self.settings_tools.push(SettingsTool {
            id: id.into(),
            descriptor,
            build: Arc::new(factory),
        });
        self
    }

    pub(crate) fn insert_registered(
        mut self,
        id: String,
        tool: Arc<dyn Tool>,
        permissions: Vec<String>,
    ) -> Self {
        if self.tools.insert(id.clone(), tool).is_some() {
            self.duplicate_ids.insert(id.clone());
        }
        self.tool_permissions.insert(id, permissions);
        self
    }

    /// Checked inventory of installed executable functionality.
    ///
    /// Configured agent names are deliberately not part of this list.
    pub fn manifest(&self) -> PackageManifest {
        let mut capabilities = Vec::new();
        let permissions = |id: &str| {
            self.capability_permissions
                .get(id)
                .cloned()
                .unwrap_or_default()
        };
        for (id, agent) in &self.manual_agents {
            capabilities.push(CapabilityDescriptor {
                id: id.clone(),
                kind: "agent".into(),
                description: agent.description().to_string(),
                permissions: permissions(id),
            });
        }
        for (id, factory) in &self.agent_factories {
            capabilities.push(CapabilityDescriptor {
                id: id.clone(),
                kind: "agent".into(),
                description: factory.description.clone(),
                permissions: permissions(id),
            });
        }
        for graph in self.graphs.values() {
            capabilities.push(CapabilityDescriptor {
                id: graph.id.clone(),
                kind: "workflow".into(),
                description: graph.description.clone(),
                permissions: Vec::new(),
            });
        }
        for (id, tool) in &self.tools {
            capabilities.push(CapabilityDescriptor {
                id: id.clone(),
                kind: "tool".into(),
                description: tool.description().to_string(),
                permissions: self
                    .capability_permissions
                    .get(id)
                    .or_else(|| self.tool_permissions.get(id))
                    .cloned()
                    .unwrap_or_default(),
            });
        }
        for id in self.toolsets.keys() {
            capabilities.push(CapabilityDescriptor {
                id: id.clone(),
                kind: "toolset".into(),
                description: "registered native toolset".into(),
                permissions: permissions(id),
            });
        }
        if let Some(id) = &self.browser {
            if !self.toolsets.contains_key(id) {
                capabilities.push(CapabilityDescriptor {
                    id: id.clone(),
                    kind: "toolset".into(),
                    description: "package-declared native browser capability".into(),
                    permissions: Vec::new(),
                });
            }
        }
        for builtin in &self.builtin_tools {
            if self.tools.contains_key(builtin.id()) {
                continue;
            }
            capabilities.push(CapabilityDescriptor {
                id: builtin.id().into(),
                kind: "tool".into(),
                description: format!("package-declared generic {} capability", builtin.id()),
                permissions: vec![builtin.permission().into()],
            });
        }
        if self.knowledge {
            for (id, description) in [
                (
                    KNOWLEDGE_OBSERVE_TOOL,
                    "record a sourced relation in the caller's native knowledge scope",
                ),
                (
                    KNOWLEDGE_RECALL_TOOL,
                    "recall sourced relations from the caller's native knowledge scope",
                ),
            ] {
                capabilities.push(CapabilityDescriptor {
                    id: id.into(),
                    kind: "tool".into(),
                    description: description.into(),
                    permissions: vec!["session:write".into()],
                });
            }
        }
        PackageManifest {
            name: self.name.clone(),
            version: self.version.clone(),
            protocol: common::PROTOCOL_VERSION.into(),
            runtime: self.runtime_version.clone(),
            adk: "2.3.0".into(),
            capabilities,
            required_permissions: self.required_permissions.clone(),
            features: {
                let mut features: Vec<String> = self.features.iter().cloned().collect();
                if !self.graphs.is_empty() && !features.iter().any(|value| value == "graph") {
                    features.push("graph".into());
                }
                features
            },
            launcher_modes: vec!["dev".into(), "serve".into()],
            hooks: if self.hooks.is_empty() {
                Vec::new()
            } else {
                vec![
                    "startup".into(),
                    "before_task".into(),
                    "after_task".into(),
                    "shutdown".into(),
                ]
            },
        }
    }

    /// Read the operator-selected configuration and activate one immutable
    /// composition.
    pub async fn start(mut self, config_path: impl AsRef<Path>) -> WorkerResult<WorkerInstance> {
        if let Some(id) = self.duplicate_ids.iter().next() {
            return Err(ApiError::new(
                "asset.duplicate_id",
                format!("package code registers id '{id}' more than once"),
            ));
        }
        let config_path = canonical_config(config_path.as_ref())?;
        let mut settings = WorkerSettings::load(&config_path)?;
        if let Some(name) = &settings.file.name {
            if name != &self.name {
                return Err(ApiError::new(
                    "worker.name_mismatch",
                    format!(
                        "configuration names package '{name}' but this executable is '{}'",
                        self.name
                    ),
                ));
            }
        }
        settings.kernel = Some(self.kernel.clone());
        if settings.file.skills.disclosure == "progressive" && !self.progressive_skills {
            return Err(ApiError::new(
                "config.capability_not_declared",
                "skills.disclosure = \"progressive\" requires a package that declares progressive skill disclosure",
            ));
        }
        if self
            .builtin_tools
            .iter()
            .any(|tool| tool.category() == BuiltinCategory::Web)
        {
            let Some(policy) = self.web_policy.as_ref() else {
                return Err(ApiError::new(
                    "config.web_policy",
                    "web tools require an explicit destination policy",
                ));
            };
            policy
                .validate()
                .map_err(|message| ApiError::new("config.web_policy", common::redact(&message)))?;
        }
        if self
            .builtin_tools
            .iter()
            .any(|tool| tool.category() == BuiltinCategory::Process)
            && self.process_policy.is_none()
        {
            return Err(ApiError::new(
                "config.process_policy",
                "process tools require an explicit sandbox and process policy",
            ));
        }
        self.kernel
            .configure(ToolKernelConfig {
                workspace: (!self.builtin_tools.is_empty() || self.tool_workspace_required)
                    .then(|| settings.tool_workspace()),
                timeout: std::time::Duration::from_millis(settings.file.tools.timeout_ms.max(1)),
                max_output_bytes: settings.file.tools.max_output_bytes.max(1),
                max_parallel: settings.file.tools.max_parallel.max(1),
                enabled: self
                    .builtin_tools
                    .iter()
                    .map(|tool| tool.id().to_string())
                    .collect(),
                permissions: settings.file.permissions.clone(),
                sandbox: settings.file.sandbox.clone(),
            })
            .map_err(|error| {
                ApiError::new(
                    "config.tool_kernel",
                    format!(
                        "tool kernel limits or workspace are invalid: {}",
                        common::redact(&error.message)
                    ),
                )
            })?;
        for entry in std::mem::take(&mut self.settings_tools) {
            if !self.tools.contains_key(&entry.id) {
                return Err(ApiError::new(
                    "asset.tool_unresolved",
                    format!(
                        "settings-resolved tool '{}' has no static registration",
                        entry.id
                    ),
                ));
            }
            let tool = (entry.build)(&settings);
            let wrapped = self.kernel.wrap(tool, entry.descriptor);
            self.tools.insert(entry.id, wrapped);
        }
        if !self.builtin_tools.is_empty() {
            for (id, tool, permissions) in crate::tools::builtin::tools(&self.kernel) {
                self = self.insert_registered(id, tool, permissions);
            }
        }
        if self.builtin_tools.contains(&BuiltinTool::WriteTodos) {
            let kernel = self.kernel.clone();
            let tool = Arc::new(crate::tools::plan::PlanTool::new(
                kernel.clone(),
                self.effect_runtime.clone(),
            )) as Arc<dyn Tool>;
            self = self.insert_registered(
                "write_todos".into(),
                kernel.wrap(tool, crate::tools::plan::PlanTool::descriptor()),
                vec![BuiltinTool::WriteTodos.permission().into()],
            );
        }
        if self.builtin_tools.contains(&BuiltinTool::AskUser) {
            let kernel = self.kernel.clone();
            let tool = Arc::new(crate::tools::interaction::AskUserTool::new(
                kernel.clone(),
                self.effect_runtime.clone(),
            )) as Arc<dyn Tool>;
            self = self.insert_registered(
                "ask_user".into(),
                kernel.wrap(tool, crate::tools::interaction::AskUserTool::descriptor()),
                vec![BuiltinTool::AskUser.permission().into()],
            );
        }
        if self.builtin_tools.contains(&BuiltinTool::ViewImage) {
            if let Some(workspace) = self.kernel.devtools_workspace() {
                let kernel = self.kernel.clone();
                let tool = Arc::new(crate::tools::interaction::ViewImageTool::new(workspace))
                    as Arc<dyn Tool>;
                self = self.insert_registered(
                    "view_image".into(),
                    kernel.wrap(tool, crate::tools::interaction::ViewImageTool::descriptor()),
                    vec![BuiltinTool::ViewImage.permission().into()],
                );
            }
        }
        if let Some(policy) = self.process_policy.clone() {
            let process = Arc::new(crate::ProcessRuntime::new(policy));
            for builtin in BuiltinTool::PROCESS {
                if !self.builtin_tools.contains(&builtin) {
                    continue;
                }
                let kernel = self.kernel.clone();
                let (tool, descriptor): (Arc<dyn Tool>, crate::tools::ToolDescriptor) =
                    match builtin {
                        BuiltinTool::Shell => {
                            let descriptor = crate::tools::process::ShellTool::descriptor(&process);
                            (
                                Arc::new(crate::tools::process::ShellTool::new(
                                    kernel.clone(),
                                    self.effect_runtime.clone(),
                                    process.clone(),
                                )),
                                descriptor,
                            )
                        }
                        BuiltinTool::Pty => (
                            Arc::new(crate::tools::process::PtyTool::new(
                                kernel.clone(),
                                self.effect_runtime.clone(),
                                process.clone(),
                            )),
                            crate::tools::process::PtyTool::descriptor(),
                        ),
                        BuiltinTool::BackgroundProcess => (
                            Arc::new(crate::tools::process::BackgroundProcessTool::new(
                                kernel.clone(),
                                self.effect_runtime.clone(),
                                process.clone(),
                            )),
                            crate::tools::process::BackgroundProcessTool::descriptor(),
                        ),
                        BuiltinTool::ProcessWait => (
                            Arc::new(crate::tools::process::ProcessWaitTool::new(
                                kernel.clone(),
                                self.effect_runtime.clone(),
                            )),
                            crate::tools::process::ProcessWaitTool::descriptor(),
                        ),
                        BuiltinTool::ProcessKill => (
                            Arc::new(crate::tools::process::ProcessKillTool::new(
                                kernel.clone(),
                                self.effect_runtime.clone(),
                            )),
                            crate::tools::process::ProcessKillTool::descriptor(),
                        ),
                        _ => continue,
                    };
                self = self.insert_registered(
                    builtin.id().into(),
                    kernel.wrap(tool, descriptor),
                    vec![builtin.permission().into()],
                );
            }
        }
        if let Some(policy) = self.web_policy.clone() {
            let policy = Arc::new(policy);
            if self.builtin_tools.contains(&BuiltinTool::WebFetch) {
                let kernel = self.kernel.clone();
                let tool =
                    Arc::new(crate::tools::web::WebFetchTool::new(policy.clone())) as Arc<dyn Tool>;
                self = self.insert_registered(
                    "web_fetch".into(),
                    kernel.wrap(tool, crate::tools::web::WebFetchTool::descriptor()),
                    vec![BuiltinTool::WebFetch.permission().into()],
                );
            }
            if self.builtin_tools.contains(&BuiltinTool::WebSearch) {
                let kernel = self.kernel.clone();
                let tool = Arc::new(crate::tools::web::WebSearchTool::new(policy)) as Arc<dyn Tool>;
                self = self.insert_registered(
                    "web_search".into(),
                    kernel.wrap(tool, crate::tools::web::WebSearchTool::descriptor()),
                    vec![BuiltinTool::WebSearch.permission().into()],
                );
            }
        }
        if let Some(id) = self.duplicate_ids.iter().next() {
            return Err(ApiError::new(
                "asset.duplicate_id",
                format!("package registration and a generic file tool both claim id '{id}'"),
            ));
        }
        let browser = match &self.browser {
            Some(id) => {
                Some(BrowserController::start(id.clone(), browser_settings(&settings)?).await)
            }
            None => None,
        };
        if let Some(controller) = &browser {
            let id = controller.id().to_string();
            if self
                .toolsets
                .insert(
                    id.clone(),
                    Arc::new(BrowserCapabilityToolset::new(controller.clone())),
                )
                .is_some()
            {
                self.duplicate_ids.insert(id);
            }
        }
        if self.browser.is_none() {
            if let Some(browser) = &settings.browser {
                if browser.enabled {
                    return Err(ApiError::new(
                        "browser.not_installed",
                        "configuration enables a browser capability this package does not declare",
                    ));
                }
            }
        }
        let manifest = self.manifest();
        let mut manual_agents = std::mem::take(&mut self.manual_agents);
        for (id, factory) in &self.agent_factories {
            if manual_agents.contains_key(id) {
                return Err(ApiError::new(
                    "asset.duplicate_id",
                    format!("agent factory '{id}' collides with a registered agent"),
                ));
            }
            let agent = (factory.build)(&settings)?;
            manual_agents.insert(id.clone(), ManualAgent::Built(agent));
        }
        self.manual_agents = manual_agents;
        self.agent_factories.clear();
        let registry = self.installed_registry(&manifest)?;
        // Open canonical native storage before durable harness state so a bad
        // storage path is reported as the session failure it actually is.
        let sessions = Arc::new(FileSessionService::open(
            settings.data_dir.join("native-sessions.json"),
        )?);
        let memory = NativeMemory::open(&self.name, &settings.data_dir).await?;
        let graphs = build_graphs(
            &self.name,
            &self.version,
            &settings.file.instance,
            &settings.data_dir,
            &settings.file.settings,
            &self.tools,
            &self.graphs,
        )
        .await?;
        // Provider profiles are instance configuration. The registry is built
        // before assets so file-loaded and package-registered agents resolve
        // model references from the same effective context.
        let providers = models::load(&settings).await?;
        let graph = if self.knowledge {
            Some(NativeKnowledgeGraph::open(&self.name, &settings.data_dir).await?)
        } else {
            None
        };
        let assets = Assets::load(
            &settings,
            &self,
            &graphs,
            Some(&memory),
            &providers,
            graph.as_ref(),
        )?;
        let local_mcp = Arc::new(McpRegistry::new(
            &settings.file.grants,
            Some(self.kernel.clone()),
        ));
        local_mcp
            .reconcile(&settings.file.mcp.servers, &settings.file.grants)
            .await;
        let mut runtime_config = settings.runtime_config(&self.name, &assets);
        // Profile validation and admission read the effective provider catalog
        // from RuntimeConfig. With `models.source = "gateway"` that catalog is
        // the decrypted Gateway bundle, not the local config fallback.
        runtime_config.providers = providers.profiles().clone();
        runtime_config.package = self.name.clone();
        runtime_config.package_version = self.version.clone();
        let runtime = Runtime::open_with_providers(runtime_config, providers.clone()).await?;
        runtime.set_memory(Some(memory.clone())).await;
        runtime.set_graph(graph.clone()).await;
        runtime.set_sessions(sessions.clone()).await;
        runtime
            .set_tool_workspace(Some(settings.tool_workspace()))
            .await;
        runtime
            .set_graph_checkpointers(
                graphs
                    .iter()
                    .map(|(id, graph)| (id.clone(), graph.checkpointer()))
                    .collect(),
            )
            .await;
        runtime
            .set_lifecycle_owners(std::mem::take(&mut self.lifecycle))
            .await;
        runtime.set_settings(settings.file.settings.clone()).await;
        // Bind the already-composed protected tools to the durable runtime,
        // then register their Worker-owned status interfaces for recovery.
        let _ = self.effect_runtime.set(runtime.clone());
        runtime
            .set_effect_reconcilers(self.effect_reconcilers.clone())
            .await;
        // A listener is not bound yet, but admission stays closed so direct
        // startup code cannot be mistaken for a ready Worker.
        runtime.close_admission().await;
        runtime.set_phase(common::LifecyclePhase::Recovering).await;
        let hooks = composite_hooks(std::mem::take(&mut self.hooks));
        runtime.set_hooks(hooks.clone()).await;

        let remote_mcp = RemoteMcpRegistry::new(
            settings.file.mcp.clone(),
            runtime.mcp_credentials(),
            Some(self.kernel.clone()),
        );
        let plugins = build_plugins(std::mem::take(&mut self.plugins));
        let executor = build_executor(
            &self.name,
            &assets,
            &graphs,
            ExecutorServices {
                sessions: sessions.clone(),
                file_sessions: sessions.clone(),
                plugins: plugins.clone(),
                callbacks: self.callbacks.clone(),
                artifacts: runtime.artifact_store(),
                memory: runtime.memory().await,
                mcp: local_mcp.clone(),
                remote_mcp: remote_mcp.clone(),
                providers,
                effect_approvals: self.effect_approvals.clone(),
                max_parallel: settings.file.tools.max_parallel.max(1),
                kernel: self.kernel.clone(),
            },
        )?;
        let executor = Arc::new(executor);
        runtime.set_executor(executor.clone()).await;
        registry.verify(&manifest)?;
        // Inspect durable work before any listener accepts new traffic. The
        // projection and events are durable here; dispatch waits for startup.
        let recovery = runtime.recover_plan().await?;
        // Reconcile protected effects before readiness. No reserved effect may
        // be retried or reported as a definite failure from startup alone.
        let _effects = runtime.reconcile_effects().await?;
        runtime.set_phase(common::LifecyclePhase::Warming).await;

        let revision = Arc::new(AtomicU64::new(settings.file.config_revision.max(1)));
        let cancel = CancellationToken::new();
        let effect_approvals = self.effect_approvals.clone();
        let instance = WorkerInstance {
            inner: Arc::new(WorkerInner {
                name: self.name,
                version: self.version,
                config_path,
                settings: RwLock::new(settings),
                assets: RwLock::new(Arc::new(assets)),
                runtime,
                sessions: sessions.clone(),
                file_sessions: sessions,
                executor: RwLock::new(executor.clone()),
                graph: graph.clone(),
                manifest,
                manual_agents: self.manual_agents,
                graphs: self.graphs,
                tools: self.tools,
                toolsets: self.toolsets,
                effect_approvals,
                callbacks: self.callbacks,
                mcp: local_mcp,
                remote_mcp,
                hooks,
                plugins,
                browser,
                revision,
                cancel,
                shutdown: tokio::sync::OnceCell::new(),
                kernel: self.kernel.clone(),
            }),
        };

        if let Some(hooks) = &instance.inner.hooks {
            if let Err(error) = hooks.startup(&instance).await {
                instance.inner.mcp.shutdown().await;
                executor.close().await;
                instance
                    .inner
                    .runtime
                    .record_degraded("startup_hook_failed")
                    .await;
                instance
                    .inner
                    .runtime
                    .set_phase(common::LifecyclePhase::Failed)
                    .await;
                return Err(error);
            }
        }
        // Startup succeeded. Open admission before advertising readiness, then
        // dispatch only work that was safe to resume.
        instance.inner.runtime.open_admission().await;
        instance
            .inner
            .runtime
            .set_phase(common::LifecyclePhase::Ready)
            .await;
        instance.inner.runtime.resume_recovered(&recovery);
        Ok(instance)
    }

    fn installed_registry(&self, manifest: &PackageManifest) -> WorkerResult<CapabilityRegistry> {
        let mut registry = CapabilityRegistry::default();
        for id in self
            .manual_agents
            .keys()
            .chain(self.graphs.keys())
            .chain(self.tools.keys())
            .chain(self.toolsets.keys())
        {
            registry.register(id.clone())?;
        }
        if self.knowledge {
            registry.register(KNOWLEDGE_OBSERVE_TOOL)?;
            registry.register(KNOWLEDGE_RECALL_TOOL)?;
        }
        registry.verify(manifest)?;
        Ok(registry)
    }
}

struct ExecutorServices {
    sessions: Arc<dyn SessionService>,
    file_sessions: Arc<FileSessionService>,
    plugins: Option<Arc<PluginManager>>,
    callbacks: Option<Arc<dyn Fn() -> AgentCallbacks + Send + Sync>>,
    artifacts: Arc<crate::data::artifacts::NativeArtifactStore>,
    memory: Option<Arc<NativeMemory>>,
    mcp: Arc<McpRegistry>,
    remote_mcp: Arc<RemoteMcpRegistry>,
    providers: Arc<ProviderRegistry>,
    effect_approvals: BTreeMap<String, crate::EffectApprovalPolicy>,
    max_parallel: usize,
    kernel: crate::tools::ToolKernel,
}

#[allow(clippy::too_many_arguments)]
fn build_executor(
    app_name: &str,
    assets: &Assets,
    graphs: &BTreeMap<String, GraphRuntime>,
    services: ExecutorServices,
) -> WorkerResult<NativeExecutor> {
    let ExecutorServices {
        sessions,
        file_sessions,
        plugins,
        callbacks,
        artifacts,
        memory,
        mcp,
        remote_mcp,
        providers,
        effect_approvals,
        max_parallel,
        kernel,
    } = services;
    let agents: Vec<(String, Arc<dyn Agent>)> = assets
        .agents
        .iter()
        .chain(assets.command_agents.iter())
        .map(|(id, agent)| (id.clone(), agent.clone()))
        .collect();
    let specs = assets.specs.clone();
    let declarations = assets
        .specs
        .iter()
        .map(|(id, spec)| (id.clone(), spec.mcp_servers.clone()))
        .chain(
            assets
                .command_mcp
                .iter()
                .map(|(id, servers)| (id.clone(), servers.clone())),
        )
        .collect();
    let registered = assets.agents.clone();
    let skills = assets.skills.clone();
    let skill_disclosure = assets.skill_disclosure.clone();
    let profile_memory = memory.clone();
    let profile_mcp = mcp.clone();
    let profile_context = assets.context.clone();
    let effect_tools: BTreeSet<String> = effect_approvals.keys().cloned().collect();
    let profile_effect_tools = effect_tools.clone();
    let profile_kernel = kernel.clone();
    let builder: crate::execution::profile::ProfileAgentBuilder =
        Arc::new(move |revision, _default_agent, reporter| {
            build_profile_agent(
                revision,
                &registered,
                &specs,
                &skills,
                &skill_disclosure,
                &profile_effect_tools,
                callbacks.as_deref(),
                profile_memory.as_ref(),
                &profile_context,
                &providers,
                reporter,
                Some(&profile_mcp),
                Some(&profile_kernel),
            )
        });
    let mut executor = NativeExecutor::with_plugins(app_name, agents, sessions, plugins)?
        .with_profile_builder(builder)
        .with_graphs(graphs.clone())
        .with_artifacts(artifacts)
        .with_mcp(mcp, declarations)
        .with_remote_mcp(remote_mcp)
        .with_effect_approvals(effect_approvals)
        .with_tool_concurrency(max_parallel)
        .with_context(
            assets.context.clone(),
            assets.prompts.clone(),
            assets.specs.clone(),
        )
        .with_file_sessions(file_sessions)
        .with_tool_kernel(kernel);
    if let Some(memory) = memory {
        executor = executor.with_memory(memory);
    }
    Ok(executor)
}

pub(crate) fn browser_settings(settings: &WorkerSettings) -> WorkerResult<BrowserSettings> {
    match &settings.browser {
        Some(browser) => Ok(browser.clone()),
        None => BrowserOptions::default().resolve(&settings.config_dir),
    }
}

fn composite_hooks(hooks: Vec<Arc<dyn WorkerHooks>>) -> Option<Arc<dyn WorkerHooks>> {
    match hooks.len() {
        0 => None,
        1 => hooks.into_iter().next(),
        _ => Some(Arc::new(CompositeHooks(hooks))),
    }
}
