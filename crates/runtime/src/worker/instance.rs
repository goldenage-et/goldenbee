//! Active Worker composition, reload, shutdown, and scoped context.

use crate::agents::graph::{build_graphs, GraphRegistration, GraphRuntime};
use crate::agents::PackageManifest;
use crate::agents::{AgentDescriptor, SkillAsset};
use crate::browser::BrowserController;
use crate::data::graph::NativeKnowledgeGraph;
use crate::execution::plugins::AgentCallbacks;
use crate::execution::runner::NativeExecutor;
use crate::mcp::McpRegistry;
use crate::worker::assets::Assets;
use crate::worker::build_executor;
use crate::worker::config::WorkerSettings;
use crate::worker::hooks::WorkerHooks;
use crate::worker::{ManualAgent, WorkerResult};
use crate::Runtime;
use adk_rust::plugin::PluginManager;
use adk_rust::session::SessionService;
use adk_rust::{Agent, Tool, Toolset};
use common::{ApiError, LifecyclePhase, Principal};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{OnceCell, RwLock};
use tokio_util::sync::CancellationToken;

/// Truthful result of one Worker shutdown sequence.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ShutdownReport {
    /// Final lifecycle phase as a stable wire name.
    pub phase: String,
    /// No hook, drain, or plugin-cleanup failure was observed.
    pub clean: bool,
    /// Admitted work exceeded the bounded completion window.
    pub timed_out: bool,
    /// Remaining execution tokens were cancelled.
    pub forced: bool,
    /// Redacted shutdown-hook error code and message, when it failed.
    pub hook_error: Option<String>,
    /// Non-terminal tasks left for restart reconciliation.
    pub unfinished: usize,
    /// This call joined an already completed shutdown.
    pub repeated: bool,
}

/// Active Worker composition with independently resolved assets and services.
#[derive(Clone)]
pub struct WorkerInstance {
    pub(super) inner: Arc<WorkerInner>,
}

pub(super) struct WorkerInner {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) config_path: PathBuf,
    pub(super) settings: RwLock<WorkerSettings>,
    pub(super) assets: RwLock<Arc<Assets>>,
    pub(super) runtime: Runtime,
    pub(super) sessions: Arc<dyn SessionService>,
    pub(super) file_sessions: Arc<crate::sessions::FileSessionService>,
    pub(super) executor: RwLock<Arc<NativeExecutor>>,
    pub(super) graph: Option<Arc<NativeKnowledgeGraph>>,
    pub(super) manifest: PackageManifest,
    pub(super) manual_agents: BTreeMap<String, ManualAgent>,
    pub(super) graphs: BTreeMap<String, GraphRegistration>,
    pub(super) tools: BTreeMap<String, Arc<dyn Tool>>,
    pub(super) toolsets: BTreeMap<String, Arc<dyn Toolset>>,
    pub(super) effect_approvals: BTreeMap<String, crate::EffectApprovalPolicy>,
    pub(super) callbacks: Option<Arc<dyn Fn() -> AgentCallbacks + Send + Sync>>,
    pub(super) mcp: Arc<McpRegistry>,
    pub(super) remote_mcp: Arc<crate::mcp::RemoteMcpRegistry>,
    pub(super) hooks: Option<Arc<dyn WorkerHooks>>,
    pub(super) plugins: Option<Arc<PluginManager>>,
    pub(super) browser: Option<Arc<BrowserController>>,
    pub(super) revision: Arc<AtomicU64>,
    pub(super) cancel: CancellationToken,
    pub(super) shutdown: OnceCell<ShutdownReport>,
    pub(super) kernel: crate::tools::ToolKernel,
}

impl WorkerInstance {
    /// Package name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Package version.
    pub fn version(&self) -> &str {
        &self.inner.version
    }

    /// Operator-selected configuration path.
    pub fn config_path(&self) -> &Path {
        &self.inner.config_path
    }

    /// Checked installed capability inventory.
    pub fn manifest(&self) -> &PackageManifest {
        &self.inner.manifest
    }

    /// Active process-wide runtime.
    pub fn runtime(&self) -> Runtime {
        self.inner.runtime.clone()
    }

    /// Canonical native session service.
    pub fn session_service(&self) -> Arc<dyn SessionService> {
        self.inner.sessions.clone()
    }

    /// Registered run-scope native plugins, when the package supplies them.
    pub fn plugins(&self) -> Option<Arc<PluginManager>> {
        self.inner.plugins.clone()
    }

    /// Current resolved asset revision.
    pub fn asset_revision(&self) -> u64 {
        self.inner.revision.load(Ordering::SeqCst)
    }

    /// Instance-owned optional browser capability.
    pub fn browser(&self) -> Option<Arc<BrowserController>> {
        self.inner.browser.clone()
    }

    /// Configured default agent.
    pub async fn default_agent(&self) -> String {
        self.inner.settings.read().await.file.default_agent.clone()
    }

    /// Configured application namespace.
    pub async fn namespace(&self) -> String {
        self.inner.settings.read().await.file.namespace.clone()
    }

    /// Configured default listener address.
    pub async fn default_addr(&self) -> String {
        self.inner.settings.read().await.file.addr.clone()
    }

    /// Optional Gateway registration and lease configuration.
    pub async fn gateway_config(&self) -> Option<crate::worker::GatewayConfig> {
        self.inner.settings.read().await.file.gateway.clone()
    }

    /// Principal bound to the operator-selected development token.
    pub async fn operator_principal(&self) -> WorkerResult<Principal> {
        let settings = self.inner.settings.read().await;
        if settings.file.token.is_none() {
            return Err(ApiError::new(
                "profile.token_required",
                "configuration must bind a development token for Worker-native management",
            ));
        }
        Ok(Principal::new(
            "local",
            settings.file.namespace.clone(),
            &settings
                .file
                .token_scopes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        ))
    }

    /// Build one native agent from an immutable profile revision.
    pub async fn profile_agent(
        &self,
        revision: &common::ProfileRevision,
    ) -> WorkerResult<Arc<dyn Agent>> {
        self.inner
            .executor
            .read()
            .await
            .clone()
            .profile_agent(revision)
    }

    /// Scoped context access for compiled package code and hooks.
    pub fn context(&self) -> WorkerContext {
        WorkerContext {
            instance: self.clone(),
            cancel: self.inner.cancel.clone(),
        }
    }

    /// Validate a complete candidate and activate it for new invocations.
    ///
    /// Active native executions already hold their previous agent instance and
    /// continue unaffected. A failed candidate leaves the active revision and
    /// executor untouched.
    pub async fn reload(&self) -> WorkerResult<u64> {
        let mut settings = WorkerSettings::load(&self.inner.config_path)?;
        settings.kernel = Some(self.inner.kernel.clone());
        if let Some(name) = &settings.file.name {
            if name != &self.inner.name {
                return Err(ApiError::new(
                    "worker.name_mismatch",
                    "reload candidate names another package",
                ));
            }
        }
        let callbacks = self.inner.callbacks.clone();
        let memory = self.inner.runtime.memory().await;
        let graphs: BTreeMap<String, GraphRuntime> = build_graphs(
            &self.inner.name,
            &self.inner.version,
            &settings.file.instance,
            &settings.data_dir,
            &settings.file.settings,
            &self.inner.tools,
            &self.inner.graphs,
        )
        .await?;
        self.inner
            .runtime
            .set_graph_checkpointers(
                graphs
                    .iter()
                    .map(|(id, graph)| (id.clone(), graph.checkpointer()))
                    .collect(),
            )
            .await;
        let providers = match settings.models()? {
            Some(_) => crate::worker::models::load(&settings).await?,
            None => self.inner.runtime.providers(),
        };
        let assets = Assets::load_with(
            &settings,
            &self.inner,
            &graphs,
            callbacks.as_deref(),
            memory.as_ref(),
            &providers,
            self.inner.graph.as_ref(),
        )?;
        let previous = self.inner.assets.read().await.clone();
        let previous_ids: BTreeSet<_> = previous.catalog.keys().cloned().collect();
        let candidate_ids: BTreeSet<_> = assets.catalog.keys().cloned().collect();
        if previous_ids != candidate_ids {
            return Err(ApiError::new(
                "worker.reload_catalog_changed",
                "candidate reload must keep the same configured agent ids",
            )
            .with(
                "active",
                previous_ids.iter().cloned().collect::<Vec<_>>().join(","),
            )
            .with(
                "candidate",
                candidate_ids.iter().cloned().collect::<Vec<_>>().join(","),
            ));
        }
        if settings.file.mcp.remote_only()
            != self.inner.settings.read().await.file.mcp.remote_only()
        {
            return Err(ApiError::new(
                "worker.reload_mcp_changed",
                "restart the Worker to change remote MCP configuration",
            ));
        }
        let executor = Arc::new(build_executor(
            &self.inner.name,
            &assets,
            &graphs,
            crate::worker::ExecutorServices {
                sessions: self.inner.sessions.clone(),
                file_sessions: self.inner.file_sessions.clone(),
                plugins: self.inner.plugins.clone(),
                callbacks: self.inner.callbacks.clone(),
                artifacts: self.inner.runtime.artifact_store(),
                memory,
                mcp: self.inner.mcp.clone(),
                remote_mcp: self.inner.remote_mcp.clone(),
                providers,
                effect_approvals: self.inner.effect_approvals.clone(),
                max_parallel: settings.file.tools.max_parallel.max(1),
            },
        )?);
        // Reconcile after the candidate executor is built so a build failure
        // leaves the previous active revision and its connections untouched.
        // Removed or disabled connections are cancelled before the swap; new
        // work sees the reconciled registry immediately.
        self.inner
            .mcp
            .reconcile(&settings.file.mcp.servers, &settings.file.grants)
            .await;
        self.inner.runtime.set_mcp_policies(settings.mcp_policies());
        self.inner
            .runtime
            .set_instance_grants(settings.file.grants.clone());
        let browser_settings = if self.inner.browser.is_some() {
            Some(crate::worker::browser_settings(&settings)?)
        } else {
            None
        };
        let next_revision = self.asset_revision() + 1;
        *self.inner.executor.write().await = executor.clone();
        self.inner
            .runtime
            .set_executor_at(next_revision, executor)
            .await;
        if let (Some(browser), Some(settings)) = (&self.inner.browser, browser_settings) {
            browser.reconfigure(settings).await;
        }
        *self.inner.assets.write().await = Arc::new(assets);
        *self.inner.settings.write().await = settings;
        self.inner
            .runtime
            .set_settings(self.inner.settings.read().await.file.settings.clone())
            .await;
        self.inner.revision.store(next_revision, Ordering::SeqCst);
        self.inner.runtime.set_config_revision(next_revision).await;
        Ok(next_revision)
    }

    /// Stop admitting work, bound admitted invocations, run the shutdown hook,
    /// close native plugins, and record a truthful stop phase.
    ///
    /// The sequence is: admission stop and stream close → bounded drain →
    /// package shutdown hook → native plugin/executor close → instance token
    /// cancel → stopped (or `stopped_degraded`). The method is idempotent;
    /// concurrent and repeated calls observe the same report.
    pub async fn shutdown(&self) -> ShutdownReport {
        let repeated = self.inner.shutdown.get().is_some();
        let report = self
            .inner
            .shutdown
            .get_or_init(|| self.finish_shutdown())
            .await
            .clone();
        ShutdownReport { repeated, ..report }
    }

    async fn finish_shutdown(&self) -> ShutdownReport {
        let started = self.inner.runtime.begin_drain().await;
        let mut drained = self.inner.runtime.await_drain().await;
        drained.already_draining |= started.already_draining;
        if drained.timed_out {
            self.inner.runtime.record_degraded("drain_timeout").await;
        }
        // The hook cannot replace cleanup. Record its failure and continue.
        let hook_error = match &self.inner.hooks {
            Some(hooks) => match hooks.shutdown(self).await {
                Ok(()) => None,
                Err(error) => {
                    let redacted = format!("{}: {}", error.code, common::redact(&error.message));
                    self.inner
                        .runtime
                        .record_degraded("shutdown_hook_failed")
                        .await;
                    eprintln!("shutdown: package hook failed: {redacted}");
                    Some(redacted)
                }
            },
            None => None,
        };
        self.inner.cancel.cancel();
        self.inner.runtime.cancel_all_tasks().await;
        self.inner.mcp.shutdown().await;
        if let Some(browser) = &self.inner.browser {
            browser.shutdown().await;
        }
        let executor = self.inner.executor.read().await.clone();
        executor.close().await;
        let degraded = self.inner.runtime.degraded().await;
        let clean = hook_error.is_none() && !drained.timed_out && degraded.is_empty();
        let phase = if clean {
            LifecyclePhase::Stopped
        } else {
            LifecyclePhase::StoppedDegraded
        };
        self.inner.runtime.set_phase(phase).await;
        let report = ShutdownReport {
            phase: phase.as_str().into(),
            clean,
            timed_out: drained.timed_out,
            forced: drained.forced,
            hook_error,
            unfinished: drained.unfinished,
            repeated: false,
        };
        eprintln!(
            "shutdown: phase={} clean={} timed_out={} forced={} unfinished={}",
            report.phase, report.clean, report.timed_out, report.forced, report.unfinished
        );
        report
    }
}

/// Owned context supplied to compiled package code.
#[derive(Clone)]
pub struct WorkerContext {
    instance: WorkerInstance,
    cancel: CancellationToken,
}

impl WorkerContext {
    /// Resolved agent catalog for the current revision.
    pub async fn agents(&self) -> BTreeMap<String, Arc<dyn Agent>> {
        self.instance.inner.assets.read().await.agents.clone()
    }

    /// One resolved agent.
    pub async fn agent(&self, id: &str) -> Option<Arc<dyn Agent>> {
        self.instance
            .inner
            .assets
            .read()
            .await
            .agents
            .get(id)
            .cloned()
    }

    /// Resolved prompt catalog.
    pub async fn prompts(&self) -> BTreeMap<String, String> {
        self.instance.inner.assets.read().await.prompts.clone()
    }

    /// One resolved prompt.
    pub async fn prompt(&self, id: &str) -> Option<String> {
        self.instance
            .inner
            .assets
            .read()
            .await
            .prompts
            .get(id)
            .cloned()
    }

    /// Current resolved asset revision selected for new invocations.
    pub fn asset_revision(&self) -> u64 {
        self.instance.asset_revision()
    }

    /// Discovered skill catalog.
    pub async fn skills(&self) -> Vec<SkillAsset> {
        self.instance.inner.assets.read().await.skills.clone()
    }

    /// Effective package settings.
    pub async fn settings(&self) -> BTreeMap<String, String> {
        self.instance
            .inner
            .assets
            .read()
            .await
            .config
            .file
            .settings
            .clone()
    }

    /// Instance runtime handle.
    pub fn runtime(&self) -> Runtime {
        self.instance.inner.runtime.clone()
    }

    /// Canonical native session service.
    pub fn session_service(&self) -> Arc<dyn SessionService> {
        self.instance.inner.sessions.clone()
    }

    /// Instance data directory.
    pub async fn data_dir(&self) -> PathBuf {
        self.instance
            .inner
            .assets
            .read()
            .await
            .config
            .data_dir
            .clone()
    }

    /// Checked catalog with origin and per-agent skills.
    pub async fn catalog(&self) -> Vec<AgentDescriptor> {
        self.instance
            .inner
            .assets
            .read()
            .await
            .catalog
            .values()
            .cloned()
            .collect()
    }

    /// Instance cancellation token.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancel.clone()
    }
}
