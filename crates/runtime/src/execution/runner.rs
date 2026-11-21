//! Native ADK execution for admitted harness tasks.
//!
//! The executor adapts the verified native `Runner` to the runtime
//! [`Executor`](crate::Executor) boundary. It does not replace the Runner,
//! agent loop, session service, or streaming model interface.

use crate::agents::definition::AgentSpec;
use crate::agents::graph::{GraphRuntime, INTERRUPT_METADATA_KEY};
use crate::agents::CapabilityRegistry;
use crate::context::{ContextBundle, ManifestInput};
use crate::data::artifacts::NativeArtifactStore;
use crate::data::memory::NativeMemory;
use crate::execution::profile::ProfileAgentBuilder;
use crate::execution::ResumeInspection;
use crate::mcp::{McpRegistry, RemoteMcpRegistry};
use crate::sessions::{FileSessionService, SESSION_STORE_FILE};
use crate::{ExecutionOutcome, Result, Runtime};
use adk_rust::plugin::PluginManager;
use adk_rust::prelude::*;
use adk_rust::runner::compaction::{
    estimate_event_tokens, CompactionConfig, CompactionStrategy, TruncationCompaction,
};
use adk_rust::runner::Runner;
use adk_rust::session::{CreateRequest, GetRequest, SessionService};
use adk_rust::{
    Agent, RuntimeToolset, SessionId, ToolConfirmationDecision, ToolConfirmationHandler,
    ToolConfirmationRequest, UserId,
};
use common::{ApiError, ContextCompaction, ProfileRevision, Task, TaskState, Usage};
use futures::future::BoxFuture;
use futures::StreamExt;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// One native compaction attempt observed by the harness.
#[derive(Clone)]
struct CompactionObservation {
    strategy: String,
    context_budget: u64,
    preserve_recent: usize,
    max_retries: usize,
    attempt: usize,
    events_before: usize,
    events_after: usize,
    forced: bool,
}

type CompactionReporter =
    Arc<dyn Fn(CompactionObservation) -> BoxFuture<'static, ()> + Send + Sync>;

/// Pinned native truncation with harness-level observability.
///
/// The event selection, system-event preservation, and budget handling are
/// delegated to `adk_runner::compaction::TruncationCompaction`. This wrapper
/// only records what the native strategy did; it does not summarize or rewrite
/// conversation history.
struct ObservedTruncation {
    preserve_recent: usize,
    strategy: String,
    context_budget: u64,
    max_retries: usize,
    forced: bool,
    attempts: AtomicUsize,
    reporter: CompactionReporter,
}

#[async_trait::async_trait]
impl CompactionStrategy for ObservedTruncation {
    async fn compact(&self, events: Vec<Event>, budget: usize) -> adk_rust::Result<Vec<Event>> {
        let before = events.len();
        let compacted = TruncationCompaction {
            preserve_recent: self.preserve_recent,
        }
        .compact(events, budget)
        .await?;
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        (self.reporter)(CompactionObservation {
            strategy: self.strategy.clone(),
            context_budget: self.context_budget,
            preserve_recent: self.preserve_recent,
            max_retries: self.max_retries,
            attempt,
            events_before: before,
            events_after: compacted.len(),
            forced: self.forced,
        })
        .await;
        Ok(compacted)
    }
}

/// Native ADK confirmation source for protected effect tools.
///
/// The pinned `LlmAgent` calls this before dispatching a tool whose native
/// confirmation policy names it. The decision is bound to the exact canonical
/// arguments of the pending call, persisted before any effect, and reused
/// idempotently when the same operation is replayed after restart.
struct EffectConfirmationHandler {
    runtime: Runtime,
    task_id: String,
    policies: BTreeMap<String, crate::EffectApprovalPolicy>,
}

impl std::fmt::Debug for EffectConfirmationHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EffectConfirmationHandler")
            .field("task_id", &self.task_id)
            .field("policies", &self.policies)
            .finish()
    }
}

#[async_trait::async_trait]
impl ToolConfirmationHandler for EffectConfirmationHandler {
    async fn decide(
        &self,
        request: &ToolConfirmationRequest,
    ) -> adk_rust::Result<ToolConfirmationDecision> {
        let decided = self.decide_inner(request).await;
        // Whether the native answer is approve or deny, the invocation resumes
        // after this await and must hold an execution slot again. The approval
        // wait released it; a denial does not terminate the native turn.
        self.runtime
            .set_pause(&self.task_id, crate::PAUSE_RUNNING)
            .await;
        decided
    }
}

impl EffectConfirmationHandler {
    async fn decide_inner(
        &self,
        request: &ToolConfirmationRequest,
    ) -> adk_rust::Result<ToolConfirmationDecision> {
        let Some(policy) = self.policies.get(&request.tool_name) else {
            // A handler is only installed for protected tools. Deny is the
            // safe answer if native confirmation reaches an unexpected name.
            return Ok(ToolConfirmationDecision::Deny);
        };
        let canonical = crate::tools::effect::canonical_arguments(&request.args);
        let digest = crate::tools::effect::effect_digest(&request.tool_name, &canonical);
        loop {
            match self
                .runtime
                .approval_for_operation(&self.task_id, &request.tool_name, &digest)
                .await
            {
                Some(approval) if approval.status == "approved" => {
                    return Ok(if common::now_ms() < approval.expires_at {
                        ToolConfirmationDecision::Approve
                    } else {
                        ToolConfirmationDecision::Deny
                    });
                }
                Some(approval) if approval.status == "pending" => {
                    let remaining = (approval.expires_at - common::now_ms()).max(1) as u64;
                    match self
                        .runtime
                        .wait_for_approval(
                            &approval.id,
                            std::time::Duration::from_millis(remaining),
                        )
                        .await
                    {
                        Ok(decided) => {
                            if decided.status == "approved" && common::now_ms() < decided.expires_at
                            {
                                return Ok(ToolConfirmationDecision::Approve);
                            }
                            return Ok(ToolConfirmationDecision::Deny);
                        }
                        Err(_) => return Ok(ToolConfirmationDecision::Deny),
                    }
                }
                Some(_) => return Ok(ToolConfirmationDecision::Deny),
                None => {
                    let approval = self
                        .runtime
                        .request_approval(
                            &self.task_id,
                            &request.tool_name,
                            &policy.target(&canonical),
                            &digest,
                            &policy.risk,
                            policy.ttl_ms,
                        )
                        .await
                        .map_err(|error| {
                            adk_rust::AdkError::new(
                                adk_rust::ErrorComponent::Tool,
                                adk_rust::ErrorCategory::Internal,
                                "effect.approval_unavailable",
                                common::redact(&error.message),
                            )
                        })?;
                    if approval.status != "approved" {
                        continue;
                    }
                }
            }
        }
    }
}

/// Native executor composed by one Worker Package.
pub struct NativeExecutor {
    app_name: String,
    agents: BTreeMap<String, Arc<dyn Agent>>,
    sessions: Arc<dyn SessionService>,
    plugins: Option<Arc<PluginManager>>,
    profile_builder: Option<ProfileAgentBuilder>,
    graphs: BTreeMap<String, GraphRuntime>,
    artifacts: Option<Arc<NativeArtifactStore>>,
    memory: Option<Arc<NativeMemory>>,
    mcp: Option<Arc<McpRegistry>>,
    mcp_declarations: BTreeMap<String, Vec<String>>,
    remote_mcp: Option<Arc<RemoteMcpRegistry>>,
    effect_approvals: BTreeMap<String, crate::EffectApprovalPolicy>,
    max_tool_concurrency: Option<usize>,
    /// Configured memory files for this asset revision.
    context: ContextBundle,
    /// Loaded agent prompt text keyed by agent id.
    agent_prompts: BTreeMap<String, String>,
    /// Loaded agent tool and skill bindings keyed by agent id.
    agent_specs: BTreeMap<String, AgentSpec>,
    /// Canonical session store, when one exposes manual-compaction scheduling.
    file_sessions: Option<Arc<FileSessionService>>,
    tool_kernel: crate::tools::ToolKernel,
}

impl NativeExecutor {
    /// Build an executor from compiled capability registrations and a native
    /// session service.
    ///
    /// Duplicate or empty capability ids are rejected before any task is
    /// admitted. The first registered capability is served as the root agent by
    /// native serving adapters.
    pub fn new(
        app_name: impl Into<String>,
        agents: Vec<(String, Arc<dyn Agent>)>,
        sessions: Arc<dyn SessionService>,
    ) -> Result<Self> {
        Self::with_plugins(app_name, agents, sessions, None)
    }

    /// Build an executor with run-scope native plugins.
    ///
    /// Plugins are registered once on the Runner and therefore execute once per
    /// run. Skill binding is not claimed by this slice; the catalog is exposed
    /// through context and remains ticket 24.
    pub fn with_plugins(
        app_name: impl Into<String>,
        agents: Vec<(String, Arc<dyn Agent>)>,
        sessions: Arc<dyn SessionService>,
        plugins: Option<Arc<PluginManager>>,
    ) -> Result<Self> {
        if agents.is_empty() {
            return Err(ApiError::new(
                "package.capability_missing",
                "a Worker Package must register at least one executable capability",
            ));
        }
        let mut registry = CapabilityRegistry::default();
        let mut map = BTreeMap::new();
        for (id, agent) in agents {
            registry.register(id.clone())?;
            map.insert(id, agent);
        }
        Ok(Self {
            app_name: app_name.into(),
            agents: map,
            sessions,
            plugins,
            profile_builder: None,
            graphs: BTreeMap::new(),
            artifacts: None,
            memory: None,
            mcp: None,
            mcp_declarations: BTreeMap::new(),
            remote_mcp: None,
            effect_approvals: BTreeMap::new(),
            max_tool_concurrency: None,
            context: ContextBundle::default(),
            agent_prompts: BTreeMap::new(),
            agent_specs: BTreeMap::new(),
            file_sessions: None,
            tool_kernel: crate::tools::ToolKernel::new(),
        })
    }

    /// Build an executor with an optional native memory service.
    pub fn new_with_memory(
        app_name: impl Into<String>,
        agents: Vec<(String, Arc<dyn Agent>)>,
        sessions: Arc<dyn SessionService>,
        memory: Option<Arc<NativeMemory>>,
    ) -> Result<Self> {
        let mut executor = Self::new(app_name, agents, sessions)?;
        executor.memory = memory;
        Ok(executor)
    }

    /// Attach the package's profile-revision native composition builder.
    pub fn with_profile_builder(mut self, builder: ProfileAgentBuilder) -> Self {
        self.profile_builder = Some(builder);
        self
    }

    /// Attach the native graph runtimes registered by package code.
    pub fn with_graphs(mut self, graphs: BTreeMap<String, GraphRuntime>) -> Self {
        self.graphs = graphs;
        self
    }

    /// Attach the native artifact store used to persist package-produced
    /// artifacts. The same store is passed to the pinned `Runner`.
    pub fn with_artifacts(mut self, artifacts: Arc<NativeArtifactStore>) -> Self {
        self.artifacts = Some(artifacts);
        self
    }

    /// Attach the native long-term memory service to the pinned `Runner`.
    pub fn with_memory(mut self, memory: Arc<NativeMemory>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Attach instance-owned local MCP connections and the loaded agents that
    /// declared them. Resolution is per invocation, after profile admission.
    pub fn with_mcp(
        mut self,
        mcp: Arc<McpRegistry>,
        declarations: BTreeMap<String, Vec<String>>,
    ) -> Self {
        self.mcp = Some(mcp);
        self.mcp_declarations = declarations;
        self
    }

    /// Attach the instance-owned remote MCP registry. Its toolset is injected
    /// per invocation, so every agent kind resolves the same connection context.
    pub fn with_remote_mcp(mut self, mcp: Arc<RemoteMcpRegistry>) -> Self {
        self.remote_mcp = Some(mcp);
        self
    }

    /// Attach the protected operation approval policies registered by package
    /// code. The native confirmation handler is installed only for these tools.
    pub fn with_effect_approvals(
        mut self,
        approvals: BTreeMap<String, crate::EffectApprovalPolicy>,
    ) -> Self {
        self.effect_approvals = approvals;
        self
    }

    /// Bound native parallel tool dispatch with the runtime limit.
    pub fn with_tool_concurrency(mut self, max_parallel: usize) -> Self {
        self.max_tool_concurrency = Some(max_parallel.max(1));
        self
    }

    /// Attach the resolved memory bundle and file-agent bindings used to build
    /// the per-task context manifest.
    pub(crate) fn with_context(
        mut self,
        context: ContextBundle,
        prompts: BTreeMap<String, String>,
        specs: BTreeMap<String, AgentSpec>,
    ) -> Self {
        self.context = context;
        self.agent_prompts = prompts;
        self.agent_specs = specs;
        self
    }

    /// Attach the canonical file session store so a scheduled manual
    /// compaction can be consumed on the next model turn.
    pub fn with_file_sessions(mut self, sessions: Arc<FileSessionService>) -> Self {
        self.file_sessions = Some(sessions);
        self
    }

    /// Attach the process-wide tool kernel so skill toolsets share the same
    /// bounded execution pipeline.
    pub fn with_tool_kernel(mut self, kernel: crate::tools::ToolKernel) -> Self {
        self.tool_kernel = kernel;
        self
    }

    /// Native tool names whose exact operations require approval.
    pub fn effect_approval_names(&self) -> Vec<String> {
        self.effect_approvals.keys().cloned().collect()
    }

    /// Build an executor whose canonical history is the durable native session
    /// store below `data_dir`.
    ///
    /// Storage is opened and verified before this returns, so an unavailable or
    /// unsupported configuration fails startup instead of promising history
    /// that would vanish at the next restart.
    pub fn durable(
        app_name: impl Into<String>,
        agents: Vec<(String, Arc<dyn Agent>)>,
        data_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        let data_dir = data_dir.as_ref();
        let sessions = Arc::new(FileSessionService::open(data_dir.join(SESSION_STORE_FILE))?);
        let artifacts = NativeArtifactStore::open(data_dir)?;
        Ok(Self::new(app_name, agents, sessions.clone())?
            .with_file_sessions(sessions)
            .with_artifacts(Arc::new(artifacts)))
    }

    /// Build an executor whose canonical history and long-term memory are the
    /// durable native stores below `data_dir`.
    pub fn durable_with_memory(
        app_name: impl Into<String>,
        agents: Vec<(String, Arc<dyn Agent>)>,
        data_dir: impl AsRef<Path>,
        memory: Arc<NativeMemory>,
    ) -> Result<Self> {
        Ok(Self::durable(app_name, agents, data_dir)?.with_memory(memory))
    }

    /// Native memory service, when this executor was composed with one.
    pub fn memory(&self) -> Option<Arc<NativeMemory>> {
        self.memory.clone()
    }

    /// Actual compiled capability registrations.
    pub fn registry(&self) -> CapabilityRegistry {
        let mut registry = CapabilityRegistry::default();
        for id in self.agents.keys() {
            registry
                .register(id.clone())
                .expect("executor registrations were validated at construction");
        }
        registry
    }

    /// The root agent used by native development serving.
    pub fn root_agent(&self) -> Arc<dyn Agent> {
        self.agents
            .values()
            .next()
            .cloned()
            .expect("executor always contains an agent")
    }

    /// Native session service used by development serving.
    pub fn session_service(&self) -> Arc<dyn SessionService> {
        self.sessions.clone()
    }

    /// Look up a compiled capability agent by id.
    pub fn agent(&self, id: &str) -> Option<Arc<dyn Agent>> {
        self.agents.get(id).cloned()
    }

    /// Build a native agent from one immutable profile revision.
    pub fn profile_agent(&self, revision: &ProfileRevision) -> Result<Arc<dyn Agent>> {
        if revision.composition == "single" && revision.members.is_empty() {
            // Documented base-agent sentinel for an empty single composition.
            return Ok(self.root_agent());
        }
        let builder = self.profile_builder.as_ref().ok_or_else(|| {
            ApiError::new(
                "profile.composition_unavailable",
                "this Worker has no profile composition builder",
            )
        })?;
        builder(revision, "", None)
    }

    /// Close registered run-scope plugins after admitted work has drained.
    pub async fn close(&self) {
        if let Some(plugins) = &self.plugins {
            plugins.close().await;
        }
    }

    /// Build the native compaction override that applies a requested manual
    /// compaction on the next model turn.
    ///
    /// The pinned ADK Runner has no public "compact now" method. It applies
    /// `CompactionStrategy::compact` proactively when the next request exceeds
    /// the configured budget. This chooses the smallest achievable budget for
    /// the current session so the native proactive path compacts exactly once
    /// and retains canonical session events in the store.
    async fn forced_compaction_spec(
        &self,
        user: &str,
        session: &str,
        configured: Option<&ContextCompaction>,
    ) -> Option<ContextCompaction> {
        let session = self
            .sessions
            .get(GetRequest {
                app_name: self.app_name.clone(),
                user_id: user.to_string(),
                session_id: session.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .ok()?;
        let events = session.events().all();
        let preserve = configured
            .map(|spec| spec.preserve_recent)
            .unwrap_or(8)
            .max(1);
        if events.len() <= preserve.saturating_add(1) {
            return None;
        }
        let compacted = TruncationCompaction {
            preserve_recent: preserve,
        }
        .compact(events.clone(), 0)
        .await
        .ok()?;
        let budget = estimate_event_tokens(&compacted).max(1) as u64;
        if budget >= estimate_event_tokens(&events) as u64 {
            return None;
        }
        Some(ContextCompaction {
            strategy: "truncation".into(),
            context_budget: budget,
            preserve_recent: preserve,
            max_retries: 1,
        })
    }

    /// Project the assembled context for one invocation. Digests and source
    /// identities are durable evidence; prompt and memory text are not.
    fn context_manifest(
        &self,
        task: &Task,
        revision: Option<&ProfileRevision>,
        compaction: Option<&ContextCompaction>,
        forced_compaction: bool,
    ) -> serde_json::Value {
        let base = revision
            .map(|revision| revision.instruction.clone())
            .or_else(|| self.agent_prompts.get(&task.agent).cloned())
            .unwrap_or_default();
        let effective = self.context.apply_to_instruction(&base);
        let (skills, tools) = if let Some(revision) = revision {
            let skills = if revision.skills.is_empty() {
                revision
                    .members
                    .iter()
                    .filter_map(|member| self.agent_specs.get(member))
                    .flat_map(|spec| spec.skills.iter().cloned())
                    .collect()
            } else {
                revision.skills.clone()
            };
            let tools = revision
                .members
                .iter()
                .filter_map(|member| self.agent_specs.get(member))
                .flat_map(|spec| spec.tools.iter().map(|tool| tool.name().to_string()))
                .collect();
            (skills, tools)
        } else if let Some(spec) = self.agent_specs.get(&task.agent) {
            (
                spec.skills.clone(),
                spec.tools
                    .iter()
                    .map(|tool| tool.name().to_string())
                    .collect(),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        crate::context::manifest(ManifestInput {
            bundle: &self.context,
            agent: &task.agent,
            profile_revision: task.profile_revision,
            asset_revision: task.asset_revision,
            attempt: task.attempt,
            prompt_digest: common::canonical_digest(&base),
            instruction_digest: common::canonical_digest(&effective),
            skills,
            tools,
            limits: &task.limits,
            compaction,
            forced_compaction,
        })
    }

    async fn ensure_session(&self, user: &str, session: &str) -> adk_rust::Result<()> {
        let existing = self
            .sessions
            .get(GetRequest {
                app_name: self.app_name.clone(),
                user_id: user.to_string(),
                session_id: session.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await;
        match existing {
            Ok(_) => Ok(()),
            // Only a genuinely missing identity is created. A storage failure is
            // reported instead of being mistaken for an empty session.
            Err(error) if error.category == adk_rust::ErrorCategory::NotFound => {
                self.sessions
                    .create(CreateRequest {
                        app_name: self.app_name.clone(),
                        user_id: user.to_string(),
                        session_id: Some(session.to_string()),
                        state: Default::default(),
                    })
                    .await?;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

/// Preserve the actionable native graph error behind the pinned
/// `GraphAgent`'s legacy agent error wrapper.
fn graph_error_code(error: &adk_rust::AdkError) -> String {
    let message = error.message.to_ascii_lowercase();
    if error.code == "graph.recursion_limit" || message.contains("recursion limit exceeded") {
        "graph.recursion_limit".into()
    } else if message.contains("checkpoint error") {
        "graph.checkpoint".into()
    } else if message.contains("execution interrupted") {
        "graph.interrupted".into()
    } else if message.contains("node '") && message.contains("execution failed") {
        "graph.node_execution_failed".into()
    } else {
        "graph.execution_failed".into()
    }
}

/// Extract the configured MCP connection id from a qualified exposed tool name.
fn server_of_tool(name: &str) -> Option<&str> {
    name.strip_prefix("mcp__")?
        .split_once("__")
        .map(|(server, _)| server)
}

/// Join native thinking parts without mixing them into answer text.
fn thinking_text(content: Option<&Content>) -> String {
    content
        .map(|content| {
            content
                .parts
                .iter()
                .filter_map(|part| match part {
                    Part::Thinking { thinking, .. } => Some(thinking.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Encode the authenticated `(namespace, principal)` pair into one native ADK
/// user identity. The length prefix keeps the encoding unambiguous even when
/// either value contains the separator.
pub fn native_user_id(namespace: &str, principal: &str) -> Result<String> {
    let user = format!("v1|{}|{}|{}", namespace.len(), namespace, principal);
    if user.len() > 512 {
        return Err(ApiError::new(
            "validation.session_identity",
            "namespace and principal exceed the native session identity limit",
        ));
    }
    Ok(user)
}

/// Decode the unambiguous native ADK user identity produced by
/// [`native_user_id`].
pub fn decode_native_user_id(user: &str) -> Result<(String, String)> {
    let rest = user.strip_prefix("v1|").ok_or_else(|| {
        ApiError::new(
            "validation.session_identity",
            "native user identity has an unknown version",
        )
    })?;
    let (length, rest) = rest.split_once('|').ok_or_else(|| {
        ApiError::new(
            "validation.session_identity",
            "native user identity is missing its namespace length",
        )
    })?;
    let length = length.parse::<usize>().map_err(|_| {
        ApiError::new(
            "validation.session_identity",
            "native user identity has an invalid namespace length",
        )
    })?;
    if rest.len() < length + 1 {
        return Err(ApiError::new(
            "validation.session_identity",
            "native user identity is truncated",
        ));
    }
    let (namespace, principal) = rest.split_at(length);
    let Some(principal) = principal.strip_prefix('|') else {
        return Err(ApiError::new(
            "validation.session_identity",
            "native user identity is missing its principal separator",
        ));
    };
    if principal.is_empty() {
        return Err(ApiError::new(
            "validation.session_identity",
            "native user identity has an empty principal",
        ));
    }
    Ok((namespace.to_string(), principal.to_string()))
}

#[async_trait::async_trait]
impl crate::Executor for NativeExecutor {
    async fn execute(
        &self,
        runtime: Runtime,
        task: Task,
        cancel: CancellationToken,
    ) -> ExecutionOutcome {
        let graph = self.graphs.get(&task.agent).cloned();
        let base_agent = self.agents.get(&task.agent).cloned();
        if graph.is_none() && base_agent.is_none() {
            return ExecutionOutcome::failure(ApiError::new(
                "capability.not_registered",
                format!("capability '{}' is not registered", task.agent),
            ));
        }
        let profile_revision = if task.profile.is_empty() {
            None
        } else {
            runtime
                .profile_revision_unscoped(&task.namespace, &task.profile, task.profile_revision)
                .await
        };
        if let (Some(mcp), Some(revision)) = (&self.remote_mcp, &profile_revision) {
            if let Err(error) = mcp.preflight(revision).await {
                return ExecutionOutcome::failure(error);
            }
        }
        let mut admitted_revision = None;
        // A policy fallback is reported synchronously by the provider wrapper,
        // before any output. Keep the record here and commit it through the
        // runtime before a terminal outcome so the durable projection names the
        // provider that actually ran.
        let fallback_slot: Arc<std::sync::Mutex<Option<common::ResolvedModel>>> =
            Arc::new(std::sync::Mutex::new(None));
        let (agent, graph_thread) = match &graph {
            Some(graph) => (graph.agent.clone(), Some(task.session_id.clone())),
            None => {
                let base_agent = base_agent.expect("base agent is present for a non-graph task");
                let agent = match (&self.profile_builder, &task.profile) {
                    (Some(builder), profile) => {
                        let revision = runtime
                            .profile_revision_unscoped(
                                &task.namespace,
                                profile,
                                task.profile_revision,
                            )
                            .await;
                        match revision {
                            Some(revision)
                                if revision.composition == "single"
                                    && revision.members.is_empty() =>
                            {
                                // Documented base-agent sentinel: an empty
                                // single composition (including the bootstrap
                                // `default` profile) executes the task's
                                // registered base agent.
                                admitted_revision = Some(revision);
                                base_agent
                            }
                            Some(revision) => {
                                let slot = fallback_slot.clone();
                                let reporter: crate::execution::providers::FallbackReporter =
                                    Arc::new(move |record| {
                                        *slot.lock().expect("fallback slot is not poisoned") =
                                            Some(record);
                                    });
                                match builder(&revision, &task.agent, Some(reporter)) {
                                    Ok(agent) => agent,
                                    Err(error) => {
                                        let code = error.code;
                                        return ExecutionOutcome::failure(ApiError::new(
                                            code,
                                            common::redact(&error.message),
                                        ));
                                    }
                                }
                            }
                            None => {
                                return ExecutionOutcome::failure(
                                    ApiError::new(
                                        "profile.revision_unavailable",
                                        "the pinned profile revision is no longer available",
                                    )
                                    .with("profile", task.profile.clone())
                                    .with("revision", task.profile_revision.to_string()),
                                )
                            }
                        }
                    }
                    _ => base_agent,
                };
                (agent, None)
            }
        };
        // A base (non-composed) loaded agent still selects its configured local
        // MCP toolsets through the pinned per-invocation runtime-toolset hook.
        // The profile allowlist and instance grant are re-applied here, not at
        // package registration time.
        let mut runtime_toolsets = Vec::new();
        if let (Some(registry), Some(revision)) = (&self.mcp, &admitted_revision) {
            if let Some(declared) = self.mcp_declarations.get(&task.agent) {
                let grants: BTreeSet<String> = revision.grants.iter().cloned().collect();
                for server in declared {
                    if revision.mcp.iter().any(|allowed| allowed == server) {
                        runtime_toolsets.push(registry.view(server, grants.clone()));
                    }
                }
            }
        }
        let remote_toolset = self
            .remote_mcp
            .as_ref()
            .zip(profile_revision.as_ref())
            .and_then(|(mcp, revision)| mcp.toolset(revision));
        if let Some(mcp) = &self.remote_mcp {
            let _ = mcp.take_uncertainty(&task.session_id).await;
        }
        let user = match native_user_id(&task.namespace, &task.caller) {
            Ok(user) => user,
            Err(error) => return ExecutionOutcome::failure(error),
        };
        if let Err(error) = self.ensure_session(&user, &task.session_id).await {
            return ExecutionOutcome::failure(ApiError::new(
                "session.unavailable",
                format!("native session could not be opened: {error}"),
            ));
        }
        let mut forced_compaction = false;
        if let Some(file_sessions) = &self.file_sessions {
            match file_sessions
                .take_compaction_request(&self.app_name, &user, &task.session_id)
                .await
            {
                Ok(true) => forced_compaction = true,
                Ok(false) => {}
                Err(error) => {
                    let _ = runtime
                        .append_event(
                            &task.id,
                            "context.compaction_request_failed",
                            serde_json::json!({
                                "code": error.code,
                                "message": common::redact(&error.message),
                            }),
                            true,
                        )
                        .await;
                }
            }
        }
        let memory_service = self.memory.as_ref().map(|memory| {
            Arc::new(adk_memory::MemoryServiceAdapter::new(
                memory.clone(),
                self.app_name.clone(),
                user.clone(),
            )) as Arc<dyn adk_rust::Memory>
        });
        let configured_compaction = admitted_revision
            .as_ref()
            .or(profile_revision.as_ref())
            .and_then(|revision| revision.compaction.clone());
        let mut compaction = configured_compaction.clone();
        if forced_compaction {
            compaction = self
                .forced_compaction_spec(&user, &task.session_id, configured_compaction.as_ref())
                .await;
            // A session with no compactable history cannot apply a manual
            // request. The manifest records the truthful outcome.
            forced_compaction = compaction.is_some();
        }
        let manifest = self.context_manifest(
            &task,
            admitted_revision.as_ref().or(profile_revision.as_ref()),
            compaction.as_ref(),
            forced_compaction,
        );
        let _ = runtime
            .append_event(&task.id, "context.manifest", manifest, true)
            .await;
        let mut builder = Runner::builder()
            .app_name(self.app_name.clone())
            .agent(agent)
            .session_service(self.sessions.clone());
        if let Some(spec) = compaction {
            let context_budget = match usize::try_from(spec.context_budget) {
                Ok(value) => value,
                Err(_) => {
                    return ExecutionOutcome::failure(ApiError::new(
                        "profile.compaction_budget_invalid",
                        "context compaction budget does not fit this platform",
                    ))
                }
            };
            // The reporter is awaited by the native strategy before the model
            // request continues, so the durable event describes compaction
            // that has already happened for this attempt.
            let reporter_runtime = runtime.clone();
            let reporter_task = task.id.clone();
            let reporter_session = task.session_id.clone();
            let reporter_revision = task.profile_revision;
            let reporter_provider = task
                .resolved_model
                .as_ref()
                .map(|model| model.provider.clone())
                .unwrap_or_default();
            let reporter_adapter = task
                .resolved_model
                .as_ref()
                .map(|model| model.adapter.clone())
                .unwrap_or_default();
            let reporter: CompactionReporter = Arc::new(move |observation| {
                let runtime = reporter_runtime.clone();
                let task_id = reporter_task.clone();
                let session_id = reporter_session.clone();
                let provider = reporter_provider.clone();
                let adapter = reporter_adapter.clone();
                Box::pin(async move {
                    let attributes = BTreeMap::from([
                        ("strategy".into(), observation.strategy.clone()),
                        (
                            "context_budget".into(),
                            observation.context_budget.to_string(),
                        ),
                        (
                            "preserve_recent".into(),
                            observation.preserve_recent.to_string(),
                        ),
                        (
                            "events_before".into(),
                            observation.events_before.to_string(),
                        ),
                        ("events_after".into(), observation.events_after.to_string()),
                        ("profile_revision".into(), reporter_revision.to_string()),
                        ("provider".into(), provider.clone()),
                        ("adapter".into(), adapter.clone()),
                        ("forced".into(), observation.forced.to_string()),
                    ]);
                    let _ = runtime
                        .trace(
                            &crate::new_id("trace"),
                            &task_id,
                            "context.compaction",
                            0,
                            attributes,
                        )
                        .await;
                    let _ = runtime
                        .append_event(
                            &task_id,
                            "context.compacted",
                            serde_json::json!({
                                "strategy": observation.strategy,
                                "context_budget": observation.context_budget,
                                "preserve_recent": observation.preserve_recent,
                                "max_retries": observation.max_retries,
                                "attempt": observation.attempt,
                                "events_before": observation.events_before,
                                "events_after": observation.events_after,
                                "profile_revision": reporter_revision,
                                "provider": provider,
                                "adapter": adapter,
                                "session_id": session_id,
                                "forced": observation.forced,
                                "manual_compaction": observation.forced,
                            }),
                            true,
                        )
                        .await;
                })
            });
            builder = builder.context_compaction(CompactionConfig {
                strategy: Box::new(ObservedTruncation {
                    preserve_recent: spec.preserve_recent,
                    strategy: spec.strategy.clone(),
                    context_budget: spec.context_budget,
                    max_retries: spec.max_retries,
                    forced: forced_compaction,
                    attempts: AtomicUsize::new(0),
                    reporter,
                }),
                context_budget,
                max_retries: spec.max_retries,
            });
        }
        if let Some(plugins) = self.plugins.clone() {
            builder = builder.plugin_manager(plugins);
        }
        if let Some(artifacts) = &self.artifacts {
            builder = builder.artifact_service(artifacts.clone());
        }
        if let Some(memory_service) = memory_service {
            builder = builder.memory_service(memory_service);
        }
        // The per-attempt runtime token is a child of the native Runner's
        // cooperative cancellation path. Cancelling it interrupts provider and
        // tool awaits inside the native run, not just the harness stream poll.
        let runner = match builder.cancellation_token(cancel.clone()).build() {
            Ok(runner) => runner,
            Err(error) => {
                return ExecutionOutcome::failure(ApiError::new(
                    "execution.unavailable",
                    format!("native Runner could not be constructed: {error}"),
                ));
            }
        };

        let (user_id, session_id) = match (UserId::new(&user), SessionId::new(&task.session_id)) {
            (Ok(user_id), Ok(session_id)) => (user_id, session_id),
            _ => {
                return ExecutionOutcome::failure(ApiError::new(
                    "validation.session_id",
                    "session identity is not valid for the native Runner",
                ));
            }
        };
        // A graph task's native thread is the session id under `Agent::run`.
        // Bind it durably before dispatch, so restart recovery can resume the
        // same thread instead of inventing a new one.
        if let Some(thread) = &graph_thread {
            if let Some(existing) = &task.native_thread_id {
                if existing != thread {
                    return ExecutionOutcome::failure(
                        ApiError::new(
                            "graph.thread_mismatch",
                            "the task is bound to a different native graph thread",
                        )
                        .with("bound", existing.clone())
                        .with("current", thread.clone()),
                    );
                }
            }
            if let Err(error) = runtime.bind_thread(&task.id, thread).await {
                return ExecutionOutcome::failure(ApiError::new(
                    "graph.thread_binding_failed",
                    format!(
                        "native graph thread could not be bound: {}",
                        common::redact(&error.message)
                    ),
                ));
            }
            let _ = runtime
                .append_event(
                    &task.id,
                    "graph.thread_bound",
                    serde_json::json!({
                        "thread_id": thread,
                        "agent": task.agent,
                    }),
                    true,
                )
                .await;
        }
        let mut run_config = runner.run_config().clone();
        if let Some(max_parallel) = self.max_tool_concurrency {
            run_config.tool_concurrency = adk_rust::ToolConcurrencyConfig {
                max_concurrency: Some(max_parallel),
                ..Default::default()
            };
        }
        if !self.effect_approvals.is_empty() {
            run_config.tool_confirmation_handler = Some(Arc::new(EffectConfirmationHandler {
                runtime: runtime.clone(),
                task_id: task.id.clone(),
                policies: self.effect_approvals.clone(),
            }));
        }
        run_config.runtime_toolsets = runtime_toolsets
            .into_iter()
            .map(RuntimeToolset::new)
            .collect();
        if let Some(toolset) = remote_toolset {
            run_config
                .runtime_toolsets
                .push(RuntimeToolset::new(toolset));
        }
        let mut stream = match runner
            .run_with_config(
                user_id,
                session_id,
                Content::new("user").with_text(task.input.clone()),
                Some(run_config),
            )
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                return ExecutionOutcome::failure(ApiError::new(
                    "execution.failed",
                    common::redact(&format!("native invocation was not admitted: {error}")),
                ));
            }
        };
        crate::core::fault::exit_if(crate::core::fault::Point::AfterNativeStart);

        let pending_fallback = {
            fallback_slot
                .lock()
                .expect("fallback slot is not poisoned")
                .take()
        };
        if let Some(record) = pending_fallback {
            runtime.record_model_fallback(&task.id, record).await;
        }

        let mut output = String::new();
        let mut invocation_id = None;
        let mut usage = Usage::default();
        let mut last_receipt = String::new();
        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => {
                    return ExecutionOutcome {
                        output: (!output.is_empty()).then_some(output),
                        usage,
                        error: None,
                        state: Some(TaskState::Cancelled),
                        native_invocation_id: invocation_id,
                        native_thread_id: graph_thread,
                        native_a2a_task_id: None,
                        uncertainty: None,
                    };
                }
                next = stream.next() => next,
            };
            let Some(event) = event else { break };
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    if let Some(mcp) = &self.remote_mcp {
                        if let Some(reason) = mcp.take_uncertainty(&task.session_id).await {
                            let _ = runtime
                                .append_event(
                                    &task.id,
                                    "tool.outcome_unknown",
                                    serde_json::json!({"reason": reason}),
                                    true,
                                )
                                .await;
                            return ExecutionOutcome {
                                output: (!output.is_empty()).then_some(output),
                                usage,
                                error: None,
                                state: Some(TaskState::Unknown),
                                native_invocation_id: invocation_id,
                                native_thread_id: None,
                                native_a2a_task_id: None,
                                uncertainty: Some(reason),
                            };
                        }
                    }
                    // Preserve exact native team policy codes so callers can
                    // observe the real bound that stopped the work. Graph
                    // errors retain their actionable native graph code.
                    let api_error = if graph.is_some() {
                        ApiError::new(
                            graph_error_code(&error),
                            format!("native graph invocation failed: {error}"),
                        )
                    } else if error.code.starts_with("agent.team.") {
                        ApiError::new(error.code, common::redact(&error.message))
                    } else {
                        ApiError::new(
                            "execution.failed",
                            common::redact(&format!("native invocation failed: {error}")),
                        )
                    };
                    return ExecutionOutcome {
                        output: (!output.is_empty()).then_some(output),
                        usage,
                        error: Some(api_error),
                        state: Some(TaskState::Failed),
                        native_invocation_id: invocation_id,
                        native_thread_id: graph_thread,
                        native_a2a_task_id: None,
                        uncertainty: None,
                    };
                }
            };
            let pending_fallback = {
                fallback_slot
                    .lock()
                    .expect("fallback slot is not poisoned")
                    .take()
            };
            if let Some(record) = pending_fallback {
                runtime.record_model_fallback(&task.id, record).await;
            }
            if graph.is_some()
                && (event.llm_response.interrupted
                    || event.provider_metadata.contains_key(INTERRUPT_METADATA_KEY))
            {
                // A native graph interrupt is a checkpointed pause, not a
                // successful answer or model output. The task stays
                // non-terminal and the native checkpoint is retained.
                let payload = event
                    .provider_metadata
                    .get(INTERRUPT_METADATA_KEY)
                    .cloned()
                    .unwrap_or_default();
                let _ = runtime
                    .append_event(
                        &task.id,
                        "graph.interrupted",
                        serde_json::json!({
                            "thread_id": graph_thread,
                            "interrupt": payload,
                        }),
                        true,
                    )
                    .await;
                return ExecutionOutcome {
                    output: None,
                    usage,
                    error: Some(ApiError::new(
                        "graph.interrupted",
                        "native graph paused at a committed checkpoint",
                    )),
                    state: Some(TaskState::Unknown),
                    native_invocation_id: None,
                    native_thread_id: graph_thread,
                    native_a2a_task_id: None,
                    uncertainty: Some(
                        "native graph paused; resuming the same task continues from its committed checkpoint"
                            .into(),
                    ),
                };
            }
            if invocation_id.is_none() {
                if graph.is_none() {
                    invocation_id = Some(event.invocation_id.clone());
                    // Persist the native identity as soon as the Runner exposes
                    // it, so a later restart can map the handle to the
                    // invocation that may already have produced an effect.
                    let _ = runtime.bind_native(&task.id, &event.invocation_id).await;
                }
                // A graph's output event is emitted after its terminal
                // checkpoint is committed. This boundary lets a restart test
                // kill the process between that commit and task completion.
                crate::core::fault::exit_if(crate::core::fault::Point::AfterNativeEvent);
            }
            // Native events name each immutable artifact version they saved.
            // Finalize provenance now, while the producing invocation is known.
            // Graph output uses a synthetic event id, so it is not a native
            // invocation and must not be recorded as artifact provenance.
            if graph.is_none() {
                for (file_name, version) in &event.actions.artifact_delta {
                    if let Err(error) = runtime
                        .record_native_artifact(
                            &task,
                            &event.invocation_id,
                            &self.app_name,
                            &user,
                            file_name,
                            *version,
                        )
                        .await
                    {
                        let _ = runtime
                            .append_event(
                                &task.id,
                                "artifact.recording_failed",
                                serde_json::json!({
                                    "name": file_name,
                                    "version": version,
                                    "error": error,
                                }),
                                true,
                            )
                            .await;
                    }
                }
            }
            if let Some(metadata) = &event.llm_response.usage_metadata {
                if !event.llm_response.partial {
                    usage.model_calls += 1;
                }
                usage.prompt_tokens = Some(
                    usage.prompt_tokens.unwrap_or(0) + metadata.prompt_token_count.max(0) as u64,
                );
                usage.candidate_tokens = Some(
                    usage.candidate_tokens.unwrap_or(0)
                        + metadata.candidates_token_count.max(0) as u64,
                );
            } else if !event.llm_response.partial
                && (event.llm_response.finish_reason.is_some() || !event.tool_calls().is_empty())
            {
                // A model call occurred even though the provider omitted token
                // counts. Count the call; leave the token values unavailable.
                usage.model_calls += 1;
            }
            if let Some(model) = &task.resolved_model {
                usage.estimated_cost_micros =
                    crate::execution::budget::estimated_cost_micros(&usage, model);
            }
            let pending_tool_calls = if event.llm_response.partial {
                0
            } else {
                event.tool_calls().len()
            };
            if let Some(stop) = crate::execution::budget::check_after_model(
                &task.limits,
                &usage,
                pending_tool_calls as u32,
            ) {
                // The stop was selected before the requested tools were
                // dispatched. Return the truthful partial result; the runtime
                // records the durable policy event when the task finishes.
                return ExecutionOutcome {
                    output: (!output.is_empty()).then_some(output),
                    usage,
                    error: Some(stop.error()),
                    state: Some(TaskState::Failed),
                    native_invocation_id: invocation_id,
                    native_thread_id: graph_thread,
                    native_a2a_task_id: None,
                    uncertainty: Some(stop.uncertainty),
                };
            }
            if !event.llm_response.partial {
                for call in event.tool_calls() {
                    let _ = runtime
                        .append_event(
                            &task.id,
                            "tool.call",
                            serde_json::json!({
                                "name": call.name,
                                "id": call.call_id,
                                "agent": event.author.clone(),
                                "server": server_of_tool(call.name),
                            }),
                            true,
                        )
                        .await;
                }
                for result in event.tool_results() {
                    let error_text = result
                        .response
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    let error_code = crate::mcp::error_code(error_text);
                    let ok = error_text.is_empty();
                    if ok {
                        usage.tool_calls += 1;
                    }
                    let _ = runtime
                        .append_event(
                            &task.id,
                            "tool.result",
                            serde_json::json!({
                                "name": result.name,
                                "id": result.call_id,
                                "agent": event.author.clone(),
                                "server": server_of_tool(result.name),
                                "ok": ok,
                                "error_code": error_code,
                            }),
                            true,
                        )
                        .await;
                }
            }
            let thinking = thinking_text(event.llm_response.content.as_ref());
            if !thinking.is_empty() {
                let kind = if event.llm_response.partial {
                    "model.thinking"
                } else {
                    "model.thinking.complete"
                };
                let _ = runtime
                    .append_event(
                        &task.id,
                        kind,
                        serde_json::json!({"text": thinking, "author": event.author}),
                        false,
                    )
                    .await;
            }
            let text = event
                .llm_response
                .content
                .as_ref()
                .map(|content| {
                    content
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            Part::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            if !text.is_empty() {
                if !event.llm_response.partial {
                    output.push_str(&text);
                }
                let kind = if graph.is_some() {
                    "graph.output"
                } else if event.llm_response.partial {
                    "model.delta"
                } else {
                    "model.delta.complete"
                };
                let _ = runtime
                    .append_event(
                        &task.id,
                        kind,
                        serde_json::json!({"text": text, "author": event.author}),
                        false,
                    )
                    .await;
            }
            // Preserve native transfers instead of rewriting them. The native
            // action remains on the event consumed by the Runner; this durable
            // record only makes it inspectable after the stream closes.
            if let Some(target) = &event.actions.transfer_to_agent {
                let _ = runtime
                    .append_event(
                        &task.id,
                        "task.transfer",
                        serde_json::json!({"target": target, "author": event.author}),
                        true,
                    )
                    .await;
            }
            // The pinned team runtime exports its serializable causal receipt
            // through the session-state delta. Store the last distinct receipt
            // as a durable event; it is native data, not a harness summary.
            if let Some(snapshot) = event
                .actions
                .state_delta
                .get(adk_agent::team::TEAM_EXECUTION_STATE_KEY)
            {
                let canonical = serde_json::to_string(snapshot).unwrap_or_default();
                if canonical != last_receipt {
                    let _ = runtime
                        .append_event(&task.id, "team.receipt", snapshot.clone(), true)
                        .await;
                    last_receipt = canonical;
                }
            }
        }
        if cancel.is_cancelled() {
            return ExecutionOutcome {
                output: (!output.is_empty()).then_some(output),
                usage,
                error: None,
                state: Some(TaskState::Cancelled),
                native_invocation_id: invocation_id,
                native_thread_id: graph_thread,
                native_a2a_task_id: None,
                uncertainty: None,
            };
        }
        if let Some(mcp) = &self.remote_mcp {
            if let Some(reason) = mcp.take_uncertainty(&task.session_id).await {
                let _ = runtime
                    .append_event(
                        &task.id,
                        "tool.outcome_unknown",
                        serde_json::json!({"reason": reason}),
                        true,
                    )
                    .await;
                return ExecutionOutcome {
                    output: (!output.is_empty()).then_some(output),
                    usage,
                    error: None,
                    state: Some(TaskState::Unknown),
                    native_invocation_id: invocation_id,
                    native_thread_id: None,
                    native_a2a_task_id: None,
                    uncertainty: Some(reason),
                };
            }
        }
        ExecutionOutcome {
            output: Some(output),
            usage,
            error: None,
            state: Some(TaskState::Succeeded),
            native_invocation_id: invocation_id,
            native_thread_id: graph_thread,
            native_a2a_task_id: None,
            uncertainty: None,
        }
    }

    async fn cancel(&self, _task_id: &str) -> bool {
        // Cancellation is cooperative: the runtime token stops the stream and
        // the task is marked cancelled when the invocation actually returns.
        false
    }

    async fn inspect_resume(&self, task: &Task) -> ResumeInspection {
        let Some(graph) = self.graphs.get(&task.agent) else {
            return ResumeInspection::Unsupported;
        };
        if let Some(thread) = &task.native_thread_id {
            if thread != &task.session_id {
                return ResumeInspection::Incompatible {
                    detail: format!(
                        "graph.thread_mismatch: task is bound to thread '{thread}' but the native graph thread is '{}'",
                        task.session_id
                    ),
                };
            }
        }
        graph.inspect_resume(&task.session_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_parts_are_separated_from_answer_text() {
        let content = Content {
            role: "model".into(),
            parts: vec![
                Part::Thinking {
                    thinking: "inspect first".into(),
                    signature: None,
                },
                Part::Text {
                    text: "answer".into(),
                },
            ],
        };
        assert_eq!(thinking_text(Some(&content)), "inspect first");
        assert_eq!(thinking_text(None), "");
    }
}
