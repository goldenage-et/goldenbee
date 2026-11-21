//! Runtime handle, lifecycle, health, and internal coordination state.

use crate::core::error::{Error, Result};
use crate::core::ids::profile_key;
use crate::core::store::{State, Store};
use crate::execution::providers::{ProviderRegistry, ProviderView};
use crate::{ApiError, ExecutionOutcome, Executor, RuntimeConfig};
use common::{Health, LifecyclePhase, Principal};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{
    broadcast, Mutex, Notify, OwnedSemaphorePermit, RwLock, Semaphore, TryAcquireError,
};
use tokio_util::sync::CancellationToken;

/// Execution-pause value: the native attempt is running.
pub(crate) const PAUSE_RUNNING: u8 = 0;
/// Execution-pause value: the attempt is waiting for approval.
pub(crate) const PAUSE_WAITING: u8 = 1;

/// Result of beginning or awaiting a bounded drain.
#[derive(Debug, Clone, Copy, Default)]
pub struct DrainOutcome {
    /// A drain was already active when this call arrived.
    pub already_draining: bool,
    /// The bounded completion window expired.
    pub timed_out: bool,
    /// Remaining execution tokens were cancelled at the deadline.
    pub forced: bool,
    /// Non-terminal tasks still visible when the drain returned.
    pub unfinished: usize,
}

struct Inner {
    store: Store,
    config: RuntimeConfig,
    providers: Arc<ProviderRegistry>,
    artifacts: Arc<crate::data::artifacts::NativeArtifactStore>,
    executor: RwLock<Option<Arc<dyn Executor>>>,
    /// Executors retained by the resolved asset revision they were built from.
    /// A task admitted under an older revision keeps that exact snapshot.
    executors: RwLock<BTreeMap<u64, Arc<dyn Executor>>>,
    memory: RwLock<Option<Arc<crate::data::memory::NativeMemory>>>,
    graph: RwLock<Option<Arc<crate::data::graph::NativeKnowledgeGraph>>>,
    sessions: RwLock<Option<Arc<crate::sessions::FileSessionService>>>,
    /// Turn-boundary file snapshot bytes, content-addressed below `data_dir`.
    checkpoint_files: Arc<crate::checkpoints::CheckpointStore>,
    /// Workspace root registered for checkpoint restore.
    tool_workspace: RwLock<Option<PathBuf>>,
    checkpoints: RwLock<BTreeMap<String, Arc<crate::agents::graph::DefinitionCheckpointer>>>,
    lifecycle: RwLock<Vec<Arc<dyn crate::LifecycleOwner>>>,
    slots: Arc<Semaphore>,
    streams: Arc<Semaphore>,
    active: AtomicUsize,
    queued: AtomicUsize,
    cancels: Mutex<BTreeMap<String, CancellationToken>>,
    pauses: Mutex<BTreeMap<String, Arc<AtomicU8>>>,
    seqs: Mutex<BTreeMap<String, u64>>,
    live: broadcast::Sender<common::StreamEvent>,
    approval_notify: Notify,
    session_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    hooks: RwLock<Option<Arc<dyn crate::worker::WorkerHooks>>>,
    subagents: RwLock<Option<Arc<crate::subagents::SubagentConfig>>>,
    /// Sessions started in this process, keyed by canonical identity. The
    /// stored context is replayed to `session_end` hooks at shutdown.
    hooked_sessions: Mutex<BTreeMap<String, crate::worker::SessionHookContext>>,
    settings: RwLock<BTreeMap<String, String>>,
    mcp_credentials: crate::mcp::McpCredentials,
    /// Worker-owned status interfaces for registered protected effects.
    effect_reconcilers:
        RwLock<BTreeMap<String, Arc<dyn crate::execution::operations::EffectReconciler>>>,
    /// Task-owned shell, PTY, and background processes.
    processes: crate::process::ProcessSupervisor,
    config_revision: AtomicU64,
    phase: RwLock<LifecyclePhase>,
    phase_history: RwLock<Vec<LifecyclePhase>>,
    /// Held read during an admission transaction so drain cannot interleave
    /// between the admission decision and the durable acceptance write.
    admission: RwLock<bool>,
    admission_open: AtomicBool,
    drain: CancellationToken,
    drain_complete: AtomicBool,
    forced: AtomicBool,
    degraded: RwLock<Vec<String>>,
    started: Instant,
}

/// Cloneable runtime handle.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
}

impl Runtime {
    /// Open or create a store under the configured data directory.
    pub async fn open(config: RuntimeConfig) -> Result<Self> {
        let providers =
            ProviderRegistry::from_profiles(config.providers.clone(), config.allow_remote)
                .map_err(|error| Error::new(error.code, error.message))?;
        Self::open_with_providers(config, providers).await
    }

    /// Open a runtime that shares an already-built provider registry.
    ///
    /// Worker startup builds the registry before loading package assets so
    /// file-loaded and package-registered agents resolve model references from
    /// one effective context.
    pub async fn open_with_providers(
        config: RuntimeConfig,
        providers: Arc<ProviderRegistry>,
    ) -> Result<Self> {
        let path = config.data_dir.join("state.json");
        let initial_revision = config.config_revision.max(1);
        let initial = State {
            tokens: config.tokens.clone(),
            ..State::default()
        };
        let store = Store::open(path, initial).await?;
        // Merge configured tokens with persisted bindings so a restart keeps
        // caller identity while still allowing operator rotation.
        store
            .write(|state| {
                for (token, principal) in &config.tokens {
                    state.tokens.insert(token.clone(), principal.clone());
                }
                Ok(())
            })
            .await?;
        let artifacts = Arc::new(crate::data::artifacts::NativeArtifactStore::open(
            config.data_dir.clone(),
        )?);
        let checkpoint_files =
            Arc::new(crate::checkpoints::CheckpointStore::open(&config.data_dir)?);
        let (live, _) = broadcast::channel(config.stream_buffer.max(1));
        let slots = Arc::new(Semaphore::new(config.max_active.max(1)));
        let streams = Arc::new(Semaphore::new(config.max_streams.max(1)));
        let mcp_credentials = crate::mcp::McpCredentials::new(&config.mcp);
        let runtime = Self {
            inner: Arc::new(Inner {
                store,
                config,
                providers,
                artifacts,
                executor: RwLock::new(None),
                executors: RwLock::new(BTreeMap::new()),
                memory: RwLock::new(None),
                graph: RwLock::new(None),
                sessions: RwLock::new(None),
                checkpoint_files,
                tool_workspace: RwLock::new(None),
                checkpoints: RwLock::new(BTreeMap::new()),
                lifecycle: RwLock::new(Vec::new()),
                slots,
                streams,
                active: AtomicUsize::new(0),
                queued: AtomicUsize::new(0),
                cancels: Mutex::new(BTreeMap::new()),
                pauses: Mutex::new(BTreeMap::new()),
                seqs: Mutex::new(BTreeMap::new()),
                live,
                approval_notify: Notify::new(),
                session_locks: Mutex::new(BTreeMap::new()),
                hooks: RwLock::new(None),
                subagents: RwLock::new(None),
                hooked_sessions: Mutex::new(BTreeMap::new()),
                settings: RwLock::new(BTreeMap::new()),
                mcp_credentials,
                effect_reconcilers: RwLock::new(BTreeMap::new()),
                processes: crate::process::ProcessSupervisor::new(),
                config_revision: AtomicU64::new(initial_revision),
                phase: RwLock::new(LifecyclePhase::Initializing),
                phase_history: RwLock::new(vec![LifecyclePhase::Initializing]),
                admission: RwLock::new(true),
                admission_open: AtomicBool::new(true),
                drain: CancellationToken::new(),
                drain_complete: AtomicBool::new(false),
                forced: AtomicBool::new(false),
                degraded: RwLock::new(Vec::new()),
                started: Instant::now(),
            }),
        };
        runtime.bootstrap().await?;
        Ok(runtime)
    }

    /// Create a temporary runtime for tests.
    pub async fn temporary() -> Result<(Self, tempfile::TempDir)>
    where
        Self: Sized,
    {
        let dir = tempfile::tempdir()
            .map_err(|error| Error::new("storage.unavailable", error.to_string()))?;
        let runtime = Self::open(RuntimeConfig::local(dir.path())).await?;
        Ok((runtime, dir))
    }

    /// Install the native execution boundary at the current configuration
    /// revision.
    pub async fn set_executor(&self, executor: Arc<dyn Executor>) {
        let revision = self.config_revision();
        *self.inner.executor.write().await = Some(executor.clone());
        self.inner
            .executors
            .write()
            .await
            .insert(revision, executor);
    }

    /// Install the native execution boundary for an explicit resolved revision.
    ///
    /// Recommendations switch to a new revision only after the candidate
    /// executor has been built, so queued work admitted under an older revision
    /// can still resolve its pinned snapshot.
    pub async fn set_executor_at(&self, revision: u64, executor: Arc<dyn Executor>) {
        *self.inner.executor.write().await = Some(executor.clone());
        self.inner
            .executors
            .write()
            .await
            .insert(revision.max(1), executor);
    }

    /// Install the native memory service that owns long-term memory.
    pub async fn set_memory(&self, memory: Option<Arc<crate::data::memory::NativeMemory>>) {
        *self.inner.memory.write().await = memory;
    }

    /// Native memory service, when this runtime was composed with one.
    pub async fn memory(&self) -> Option<Arc<crate::data::memory::NativeMemory>> {
        self.inner.memory.read().await.clone()
    }

    /// Instance-owned MCP credential references.
    pub fn mcp_credentials(&self) -> crate::mcp::McpCredentials {
        self.inner.mcp_credentials.clone()
    }

    /// Revoke an owned MCP credential. New calls fail before any request.
    pub async fn revoke_mcp_credential(
        &self,
        principal: &Principal,
        reference: &str,
    ) -> Result<()> {
        principal.require(common::scopes::INSTANCE_ADMIN)?;
        self.inner.mcp_credentials.revoke(reference)?;
        self.audit(
            &principal.id,
            "mcp.credential_revoke",
            reference,
            "revoked",
            "",
        )
        .await?;
        Ok(())
    }

    /// Install the native knowledge graph that owns entity and relation facts.
    pub async fn set_graph(&self, graph: Option<Arc<crate::data::graph::NativeKnowledgeGraph>>) {
        *self.inner.graph.write().await = graph;
    }

    /// Native knowledge graph, when this runtime was composed with one.
    pub async fn graph(&self) -> Option<Arc<crate::data::graph::NativeKnowledgeGraph>> {
        self.inner.graph.read().await.clone()
    }

    /// Install the canonical native session store so retention and erasure can
    /// reach the owner instead of duplicating session history in the harness
    /// metadata store.
    pub async fn set_sessions(&self, sessions: Arc<crate::sessions::FileSessionService>) {
        *self.inner.sessions.write().await = Some(sessions);
    }

    /// Canonical native session store, when composed by the Worker package.
    pub async fn sessions(&self) -> Option<Arc<crate::sessions::FileSessionService>> {
        self.inner.sessions.read().await.clone()
    }

    pub(crate) fn checkpoint_store_ref(&self) -> &Arc<crate::checkpoints::CheckpointStore> {
        &self.inner.checkpoint_files
    }

    pub(crate) fn tool_workspace_ref(&self) -> &RwLock<Option<PathBuf>> {
        &self.inner.tool_workspace
    }

    /// Install the definition-bound native checkpoint owners for lifecycle
    /// operations. Checkpoint state remains owned by the native checkpointer.
    pub async fn set_graph_checkpointers(
        &self,
        checkpointers: BTreeMap<String, Arc<crate::agents::graph::DefinitionCheckpointer>>,
    ) {
        *self.inner.checkpoints.write().await = checkpointers;
    }

    /// Current definition-bound native checkpoint owners by graph capability.
    pub async fn graph_checkpointers(
        &self,
    ) -> BTreeMap<String, Arc<crate::agents::graph::DefinitionCheckpointer>> {
        self.inner.checkpoints.read().await.clone()
    }

    /// Register package-owned lifecycle stores. The owning package decides what
    /// its data classes are; the runtime only calls and reports their outcome.
    pub async fn set_lifecycle_owners(&self, owners: Vec<Arc<dyn crate::LifecycleOwner>>) {
        *self.inner.lifecycle.write().await = owners;
    }

    /// Registered package-owned lifecycle stores.
    pub async fn lifecycle_owners(&self) -> Vec<Arc<dyn crate::LifecycleOwner>> {
        self.inner.lifecycle.read().await.clone()
    }

    /// Configuration snapshot.
    pub fn config(&self) -> &RuntimeConfig {
        &self.inner.config
    }

    /// Deterministic evidence redactor for this configuration revision.
    pub(crate) fn redactor(&self) -> crate::core::redaction::Redactor {
        crate::core::redaction::Redactor::new(&self.inner.config.redaction_roots)
    }

    /// Shared provider registry used by admission and construction.
    pub fn providers(&self) -> Arc<ProviderRegistry> {
        self.inner.providers.clone()
    }

    /// Revoke a credential reference. New provider calls through any adapter
    /// built by this runtime fail without rebuilding the package.
    pub fn revoke_credential(&self, reference: &str) {
        self.inner.providers.revoke(reference);
    }

    /// Restore a credential reference previously revoked in this process.
    pub fn restore_credential(&self, reference: &str) {
        self.inner.providers.restore(reference);
    }

    /// Secret-safe provider inspection.
    pub fn provider_views(&self) -> Vec<ProviderView> {
        self.inner.providers.views()
    }

    /// Apply the MCP policies of an already validated reload. Future
    /// admissions see the new enabled/credential policy; immutable revisions
    /// stored earlier are not rewritten.
    pub fn set_mcp_policies(&self, policies: BTreeMap<String, crate::mcp::McpServerPolicy>) {
        if let Ok(mut current) = self.inner.config.mcp_policies.write() {
            *current = policies;
        }
    }

    /// Apply the instance grants of an already validated reload.
    pub fn set_instance_grants(&self, grants: Vec<String>) {
        if let Ok(mut current) = self.inner.config.instance_grants.write() {
            *current = grants;
        }
    }

    /// Register Worker-owned status interfaces for protected effect tools.
    pub async fn set_effect_reconcilers(
        &self,
        reconcilers: BTreeMap<String, Arc<dyn crate::execution::operations::EffectReconciler>>,
    ) {
        *self.inner.effect_reconcilers.write().await = reconcilers;
    }

    /// Status interface registered for one effect tool.
    pub(crate) async fn effect_reconciler(
        &self,
        tool: &str,
    ) -> Option<Arc<dyn crate::execution::operations::EffectReconciler>> {
        self.inner
            .effect_reconcilers
            .read()
            .await
            .get(tool)
            .cloned()
    }

    /// Store access.
    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// Resolve a bearer token to a principal.
    pub async fn authenticate(&self, token: &str) -> Result<Principal> {
        if token.is_empty() {
            return Err(Error::new(
                "auth.unauthenticated",
                "bearer token is required",
            ));
        }
        self.inner
            .store
            .read(|state| state.tokens.get(token).cloned())
            .await
            .ok_or_else(|| Error::new("auth.unauthenticated", "token is not recognized"))
    }

    /// Subscribe to live events.
    pub fn subscribe(&self) -> broadcast::Receiver<common::StreamEvent> {
        self.inner.live.subscribe()
    }

    /// Reserve one bounded stream slot for one public SSE connection.
    ///
    /// The permit is released when the returned value drops, so a closed
    /// viewer cannot permanently consume fan-out capacity. Exhaustion is a
    /// typed retryable error and never blocks execution.
    pub fn stream_slot(&self) -> crate::Result<OwnedSemaphorePermit> {
        self.inner
            .streams
            .clone()
            .try_acquire_owned()
            .map_err(|error| match error {
                TryAcquireError::NoPermits => ApiError::retryable(
                    "stream.subscriber_limit",
                    "this Worker has reached its bounded number of task streams",
                )
                .with("max_streams", self.inner.config.max_streams.to_string()),
                TryAcquireError::Closed => ApiError::retryable(
                    "stream.subscriber_limit",
                    "task stream admission is closed",
                ),
            })
    }

    /// Highest sequence issued for a task in this incarnation, or its newest
    /// retained durable sequence after a restart.
    pub(crate) async fn latest_seq(&self, task_id: &str) -> u64 {
        let live = self
            .inner
            .seqs
            .lock()
            .await
            .get(task_id)
            .copied()
            .map(|next| next.saturating_sub(1));
        if let Some(seq) = live {
            return seq;
        }
        self.inner
            .store
            .read(|state| {
                state
                    .events
                    .get(task_id)
                    .and_then(|stream| stream.last().map(|event| event.seq))
                    .unwrap_or(0)
            })
            .await
    }

    /// Health projection.
    ///
    /// Readiness derives from the lifecycle phase and the admission gate only.
    /// Provider reachability is reported as an execution subsystem observation
    /// and never changes liveness or readiness.
    pub async fn health(&self) -> Health {
        let (capacity, capabilities, _schema, usage, states) = self
            .inner
            .store
            .read(|state| {
                let active = self.inner.active.load(Ordering::Relaxed) as u32;
                let queued = self.inner.queued.load(Ordering::Relaxed) as u32;
                let mut waiting_approval = 0u64;
                let mut recovering = 0u64;
                let mut prompt_tokens = 0u64;
                let mut estimated_tokens = 0u64;
                let mut tool_calls = 0u64;
                let mut confirmed = false;
                let mut estimated = false;
                for task in state.tasks.values() {
                    if task.state == common::TaskState::WaitingApproval {
                        waiting_approval += 1;
                    }
                    if task.state.is_uncertain() {
                        recovering += 1;
                    }
                    if let Some(tokens) = task.usage.prompt_tokens {
                        prompt_tokens = prompt_tokens.saturating_add(tokens);
                        confirmed = true;
                    }
                    if let Some(tokens) = task.usage.estimated_tokens {
                        estimated_tokens = estimated_tokens.saturating_add(tokens);
                        estimated = true;
                    }
                    tool_calls = tool_calls.saturating_add(task.usage.tool_calls as u64);
                }
                (
                    common::Capacity {
                        active,
                        limit: self.inner.config.max_active as u32,
                        queued,
                        queue_limit: self.inner.config.max_queue as u32,
                    },
                    self.inner
                        .config
                        .capabilities
                        .iter()
                        .map(|c| c.id.clone())
                        .collect::<Vec<_>>(),
                    state.schema as u64,
                    (
                        (confirmed.then_some(prompt_tokens)),
                        (estimated.then_some(estimated_tokens)),
                        tool_calls,
                    ),
                    (waiting_approval, recovering),
                )
            })
            .await;
        let config_revision = self.config_revision();
        let lifecycle = self.phase().await;
        let admission_open = self.inner.admission_open.load(Ordering::SeqCst);
        let ready = lifecycle.admits_work() && admission_open;
        let readiness_reason = if ready {
            String::new()
        } else if !admission_open {
            "admission_closed".into()
        } else {
            format!("phase_{}", lifecycle.as_str())
        };
        let mut labels = BTreeMap::new();
        labels.insert(
            "instance".to_string(),
            self.inner.config.instance_id.clone(),
        );
        labels.insert("package".to_string(), self.inner.config.package.clone());
        labels.insert("phase".to_string(), lifecycle.as_str().to_string());
        let mut metrics = Vec::new();
        let mut push = |name: &str, value: Option<u64>, quality: common::MetricQuality| {
            metrics.push(common::MetricSample::new(
                name,
                value,
                quality,
                labels.clone(),
            ));
        };
        push(
            "tasks.active",
            Some(capacity.active as u64),
            common::MetricQuality::Observed,
        );
        push(
            "tasks.queued",
            Some(capacity.queued as u64),
            common::MetricQuality::Observed,
        );
        push(
            "tasks.waiting_approval",
            Some(states.0),
            common::MetricQuality::Observed,
        );
        push(
            "tasks.recovering",
            Some(states.1),
            common::MetricQuality::Observed,
        );
        push(
            "tasks.usage.prompt_tokens",
            usage.0,
            if usage.0.is_some() {
                common::MetricQuality::ProviderConfirmed
            } else {
                common::MetricQuality::Unknown
            },
        );
        push(
            "tasks.usage.estimated_tokens",
            usage.1,
            if usage.1.is_some() {
                common::MetricQuality::Estimated
            } else {
                common::MetricQuality::Unknown
            },
        );
        push(
            "tasks.usage.tool_calls",
            Some(usage.2),
            common::MetricQuality::Observed,
        );
        let mut subsystems = BTreeMap::new();
        subsystems.insert("storage".into(), "ready".into());
        // No provider probe owns this field yet. Report the truth instead of a
        // permanent fabricated degradation. Provider failure is visible on the
        // task outcome and liveness remains independent of it.
        subsystems.insert("provider".into(), "unknown".into());
        subsystems.insert(
            "executor".into(),
            if self.inner.executor.read().await.is_some() {
                "ready".into()
            } else {
                "unavailable".into()
            },
        );
        Health {
            lifecycle,
            phase_history: self
                .inner
                .phase_history
                .read()
                .await
                .iter()
                .map(|phase| phase.as_str().to_string())
                .collect(),
            phase: lifecycle.as_str().to_string(),
            instance: self.inner.config.instance_id.clone(),
            package: self.inner.config.package.clone(),
            version: self.inner.config.package_version.clone(),
            ready,
            readiness_reason,
            admission: if admission_open { "open" } else { "closed" }.into(),
            incarnation: self.inner.config.incarnation.clone(),
            config_revision,
            asset_revision: config_revision,
            capabilities,
            capacity,
            subsystems,
            degraded: self.degraded().await,
            metrics,
            uptime_ms: self.inner.started.elapsed().as_millis() as u64,
        }
    }

    async fn bootstrap(&self) -> Result<()> {
        self.inner
            .store
            .write(|state| {
                let mut namespaces: std::collections::BTreeSet<String> = self
                    .inner
                    .config
                    .tokens
                    .values()
                    .map(|p| p.namespace.clone())
                    .collect();
                namespaces.insert("local".into());
                namespaces.insert("default".into());
                let now = common::now_ms();
                for namespace in namespaces {
                    let key = profile_key(&namespace, "default");
                    if state.profiles.contains_key(&key) {
                        continue;
                    }
                    let revision = common::ProfileRevision {
                        revision: 1,
                        instruction: "You are a helpful Goldenbee agent.".into(),
                        model: self.inner.config.default_model.clone(),
                        capabilities: self
                            .inner
                            .config
                            .capabilities
                            .iter()
                            .map(|capability| capability.id.clone())
                            .collect(),
                        grants: vec!["task:submit".into()],
                        restrictions: Vec::new(),
                        composition: "single".into(),
                        members: Vec::new(),
                        skills: Vec::new(),
                        mcp: Vec::new(),
                        workflow: None,
                        limits: common::Limits::default(),
                        compaction: None,
                        secret_refs: Vec::new(),
                        reasoning: None,
                        created_at: now,
                    };
                    state.profiles.insert(
                        key,
                        common::Profile {
                            name: "default".into(),
                            active_revision: 1,
                            revisions: vec![revision],
                        },
                    );
                }
                Ok(())
            })
            .await
    }

    pub(crate) async fn session_lock(
        &self,
        namespace: &str,
        principal: &str,
        session_id: &str,
    ) -> Arc<Mutex<()>> {
        let key = format!("{namespace}\u{1f}{principal}\u{1f}{session_id}");
        let mut locks = self.inner.session_locks.lock().await;
        locks
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub(crate) fn approval_notify(&self) -> &Notify {
        &self.inner.approval_notify
    }

    /// Allocate the next stream sequence. Ephemeral events share the sequence
    /// with durable events without touching the state file.
    pub(crate) async fn next_seq(&self, task_id: &str) -> u64 {
        let mut seqs = self.inner.seqs.lock().await;
        if let Some(next) = seqs.get_mut(task_id) {
            let seq = *next;
            *next += 1;
            return seq;
        }
        let last = self
            .inner
            .store
            .read(|state| {
                state
                    .events
                    .get(task_id)
                    .and_then(|stream| stream.last().map(|event| event.seq))
                    .unwrap_or(0)
            })
            .await;
        let seq = last + 1;
        seqs.insert(task_id.to_string(), seq + 1);
        seq
    }

    pub(crate) async fn forget_seq(&self, task_id: &str) {
        self.inner.seqs.lock().await.remove(task_id);
    }

    pub(crate) fn live_sender(&self) -> &broadcast::Sender<common::StreamEvent> {
        &self.inner.live
    }

    pub(crate) fn active(&self) -> usize {
        self.inner.active.load(Ordering::Relaxed)
    }

    pub(crate) fn active_inc(&self) -> usize {
        self.inner.active.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn active_dec(&self) -> usize {
        self.inner
            .active
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_sub(1))
            })
            .unwrap_or_default()
    }

    pub(crate) fn queued_inc(&self) -> usize {
        self.inner.queued.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn queued_dec(&self) {
        let _ = self
            .inner
            .queued
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_sub(1))
            });
    }

    pub(crate) fn slots(&self) -> Arc<Semaphore> {
        Arc::clone(&self.inner.slots)
    }

    pub(crate) async fn executor(&self) -> Option<Arc<dyn Executor>> {
        self.inner.executor.read().await.clone()
    }

    /// Resolve the executor snapshot pinned on a task's asset revision.
    ///
    /// Work admitted before a reload keeps its original assets. When a process
    /// restarts with no historical snapshot, fall back to the current executor
    /// rather than refusing recoverable work.
    pub(crate) async fn executor_for(&self, revision: u64) -> Option<Arc<dyn Executor>> {
        if let Some(executor) = self.inner.executors.read().await.get(&revision).cloned() {
            return Some(executor);
        }
        self.executor().await
    }

    pub(crate) async fn register_cancel(&self, task_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.inner
            .cancels
            .lock()
            .await
            .insert(task_id.to_string(), token.clone());
        token
    }

    pub(crate) async fn unregister_cancel(&self, task_id: &str) {
        self.inner.cancels.lock().await.remove(task_id);
        self.inner.pauses.lock().await.remove(task_id);
    }

    pub(crate) async fn cancel_token(&self, task_id: &str) -> Option<CancellationToken> {
        self.inner.cancels.lock().await.get(task_id).cloned()
    }

    /// Task-owned process supervisor used by the opt-in process tools.
    pub fn processes(&self) -> &crate::process::ProcessSupervisor {
        &self.inner.processes
    }

    /// Terminate every process group owned by this Worker.
    ///
    /// Called from the Worker shutdown sequence after task cancellation so a
    /// background command cannot outlive the runtime that admitted it.
    pub async fn shutdown_processes(&self) {
        self.inner.processes.shutdown().await;
    }

    /// Register the per-attempt execution pause flag used to release and
    /// reacquire a slot across approval waits without reading the store.
    pub(crate) async fn register_pause(&self, task_id: &str) -> Arc<AtomicU8> {
        let flag = Arc::new(AtomicU8::new(PAUSE_RUNNING));
        self.inner
            .pauses
            .lock()
            .await
            .insert(task_id.to_string(), flag.clone());
        flag
    }

    pub(crate) async fn set_pause(&self, task_id: &str, value: u8) {
        if let Some(flag) = self.inner.pauses.lock().await.get(task_id).cloned() {
            flag.store(value, Ordering::Relaxed);
        }
    }

    /// Install compiled Worker hooks used at admission and lifecycle points.
    pub async fn set_hooks(&self, hooks: Option<Arc<dyn crate::worker::WorkerHooks>>) {
        *self.inner.hooks.write().await = hooks;
    }

    /// Install the Worker-declared runtime-local subagent policy.
    ///
    /// A runtime without a policy rejects every subagent operation instead of
    /// admitting work it cannot bound.
    pub async fn set_subagent_config(&self, config: Option<Arc<crate::subagents::SubagentConfig>>) {
        *self.inner.subagents.write().await = config;
    }

    /// Active Worker-declared subagent policy.
    pub async fn subagent_config(&self) -> Option<Arc<crate::subagents::SubagentConfig>> {
        self.inner.subagents.read().await.clone()
    }

    pub(crate) async fn hooks(&self) -> Option<Arc<dyn crate::worker::WorkerHooks>> {
        self.inner.hooks.read().await.clone()
    }

    /// Record one started session. Returns `false` when the identity was
    /// already started in this process, making repeated turns idempotent.
    pub(crate) async fn track_session(
        &self,
        key: String,
        context: crate::worker::SessionHookContext,
    ) -> bool {
        self.inner
            .hooked_sessions
            .lock()
            .await
            .insert(key, context)
            .is_none()
    }

    /// Drain started sessions in stable identity order for `session_end`.
    pub(crate) async fn drain_sessions(&self) -> Vec<crate::worker::SessionHookContext> {
        let mut sessions = self.inner.hooked_sessions.lock().await;
        std::mem::take(&mut *sessions).into_values().collect()
    }

    /// Replace the effective package settings exposed to hooks.
    pub async fn set_settings(&self, settings: BTreeMap<String, String>) {
        *self.inner.settings.write().await = settings;
    }

    /// Effective package settings.
    pub async fn settings(&self) -> BTreeMap<String, String> {
        self.inner.settings.read().await.clone()
    }

    /// Current resolved configuration/asset revision.
    pub fn config_revision(&self) -> u64 {
        self.inner.config_revision.load(Ordering::SeqCst)
    }

    /// Immutable snapshot of the effective permission policy.
    pub fn permission_snapshot(&self) -> crate::policy::PolicySnapshot {
        self.inner
            .config
            .permissions
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot()
    }

    /// Shared permission policy handle.
    pub fn permission_handle(&self) -> crate::policy::PolicyHandle {
        self.inner.config.permissions.clone()
    }

    /// Replace the permission policy for subsequent tool calls.
    ///
    /// The change is an operator action. It never rewrites decisions already
    /// recorded for earlier calls, and it does not retroactively authorize an
    /// action whose approval was denied. Each later call records the new policy
    /// revision and digest in its permission evidence.
    pub async fn set_permission_policy(
        &self,
        principal: &Principal,
        policy: crate::policy::PermissionPolicy,
    ) -> Result<crate::policy::PolicySnapshot> {
        principal.require(common::scopes::INSTANCE_ADMIN)?;
        policy
            .validate()
            .map_err(|message| Error::new("validation.permissions", message))?;
        let previous = self.permission_snapshot();
        let snapshot = self
            .inner
            .config
            .permissions
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .replace(policy);
        self.audit(
            &principal.id,
            "permission.policy",
            &format!("policy-{}", snapshot.revision),
            "applied",
            &format!(
                "previous_revision={} revision={} digest={} mode={} rule_count={}",
                previous.revision,
                snapshot.revision,
                snapshot.digest,
                snapshot.mode().as_str(),
                snapshot.summary()["rule_count"],
            ),
        )
        .await?;
        Ok(snapshot)
    }

    /// Set the current resolved configuration/asset revision.
    pub async fn set_config_revision(&self, revision: u64) {
        self.inner
            .config_revision
            .store(revision.max(1), Ordering::SeqCst);
    }

    /// Current lifecycle phase.
    pub async fn phase(&self) -> LifecyclePhase {
        *self.inner.phase.read().await
    }

    /// Set the lifecycle phase.
    pub async fn set_phase(&self, phase: LifecyclePhase) {
        let mut current = self.inner.phase.write().await;
        if *current == phase {
            return;
        }
        *current = phase;
        drop(current);
        let mut history = self.inner.phase_history.write().await;
        history.push(phase);
    }

    /// Whether new task admission is open.
    pub async fn admission_open(&self) -> bool {
        self.inner.admission_open.load(Ordering::SeqCst)
    }

    /// Hold the admission read gate for one acceptance transaction.
    pub(crate) async fn admission_guard(&self) -> tokio::sync::RwLockReadGuard<'_, bool> {
        self.inner.admission.read().await
    }

    /// Close admission without starting a drain. Used during startup so a
    /// direct runtime caller cannot be mistaken for a ready Worker.
    pub async fn close_admission(&self) {
        let mut admission = self.inner.admission.write().await;
        *admission = false;
        self.inner.admission_open.store(false, Ordering::SeqCst);
    }

    /// Open admission after startup hooks have succeeded.
    pub async fn open_admission(&self) {
        let mut admission = self.inner.admission.write().await;
        if self.inner.drain.is_cancelled() {
            return;
        }
        *admission = true;
        self.inner.admission_open.store(true, Ordering::SeqCst);
    }

    /// Atomically stop new admission, make readiness false, and terminate
    /// connected stream readers. Idempotent.
    ///
    /// The write lock on `admission` waits for in-flight admission
    /// transactions; once it returns, no task can be accepted after this
    /// point. Admission transactions hold the read lock across the durable
    /// acceptance write.
    pub async fn begin_drain(&self) -> DrainOutcome {
        if self.phase().await.is_terminal() {
            return DrainOutcome {
                already_draining: true,
                unfinished: self.unfinished_count().await,
                ..DrainOutcome::default()
            };
        }
        let mut admission = self.inner.admission.write().await;
        let already_draining = !*admission || self.inner.drain.is_cancelled();
        *admission = false;
        self.inner.admission_open.store(false, Ordering::SeqCst);
        self.inner.drain.cancel();
        self.set_phase(LifecyclePhase::Draining).await;
        drop(admission);
        DrainOutcome {
            already_draining,
            unfinished: self.unfinished_count().await,
            ..DrainOutcome::default()
        }
    }

    /// Token cancelled when drain begins. Stream readers close on it.
    pub fn drain_token(&self) -> CancellationToken {
        self.inner.drain.clone()
    }

    /// Non-terminal admitted work, including approval-paused tasks.
    pub async fn unfinished_count(&self) -> usize {
        self.inner
            .store
            .read(|state| {
                state
                    .tasks
                    .values()
                    .filter(|task| !task.state.is_terminal())
                    .count()
            })
            .await
    }

    /// Work that can still finish through execution. `recovering` and
    /// `unknown` tasks need reconciliation and do not extend a drain window.
    pub async fn drainable_count(&self) -> usize {
        self.inner
            .store
            .read(|state| {
                state
                    .tasks
                    .values()
                    .filter(|task| {
                        matches!(
                            task.state,
                            common::TaskState::Accepted
                                | common::TaskState::Queued
                                | common::TaskState::Running
                                | common::TaskState::WaitingApproval
                                | common::TaskState::Cancelling
                        )
                    })
                    .count()
            })
            .await
    }

    /// Wait for admitted work up to the configured drain timeout, then cancel
    /// remaining execution tokens. Work that was still active is left
    /// recoverable by task completion, never rewritten to success or failure.
    pub async fn await_drain(&self) -> DrainOutcome {
        if self.inner.drain_complete.load(Ordering::SeqCst) {
            return DrainOutcome {
                already_draining: true,
                timed_out: self.forced_stop(),
                forced: self.forced_stop(),
                unfinished: self.unfinished_count().await,
            };
        }
        let timeout = Duration::from_millis(self.inner.config.drain_timeout_ms.max(1));
        let deadline = Instant::now() + timeout;
        loop {
            let drainable = self.drainable_count().await;
            if drainable == 0 {
                self.inner.drain_complete.store(true, Ordering::SeqCst);
                return DrainOutcome {
                    unfinished: self.unfinished_count().await,
                    ..DrainOutcome::default()
                };
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.force_stop().await;
        let grace = Instant::now() + Duration::from_secs(2);
        while self.active() > 0 && Instant::now() < grace {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.inner.drain_complete.store(true, Ordering::SeqCst);
        DrainOutcome {
            timed_out: true,
            forced: true,
            unfinished: self.unfinished_count().await,
            ..DrainOutcome::default()
        }
    }

    /// Mark remaining execution as forced and cancel every registered attempt.
    pub async fn force_stop(&self) {
        self.inner.forced.store(true, Ordering::SeqCst);
        self.record_degraded("drain_timeout").await;
        self.cancel_all_tasks().await;
    }

    /// Cancel every registered task attempt without changing task state.
    pub async fn cancel_all_tasks(&self) {
        let tokens: Vec<CancellationToken> =
            self.inner.cancels.lock().await.values().cloned().collect();
        for token in tokens {
            token.cancel();
        }
    }

    /// Record one explicit degradation reason for the health projection.
    pub async fn record_degraded(&self, reason: &str) {
        let mut degraded = self.inner.degraded.write().await;
        if !degraded.iter().any(|existing| existing == reason) {
            degraded.push(reason.to_string());
        }
    }

    /// Explicit degradation reasons.
    pub async fn degraded(&self) -> Vec<String> {
        self.inner.degraded.read().await.clone()
    }

    /// Whether remaining work was force-stopped during this process lifetime.
    pub fn forced_stop(&self) -> bool {
        self.inner.forced.load(Ordering::SeqCst)
    }

    pub(crate) async fn before_task_hook(
        &self,
        ctx: &crate::worker::TaskHookContext,
    ) -> Result<()> {
        let hooks = self.inner.hooks.read().await.clone();
        match hooks {
            Some(hooks) => hooks.before_task(ctx).await.map_err(|error| {
                let mut redacted = ApiError::new(error.code, common::redact(&error.message));
                redacted.retryable = error.retryable;
                redacted.details = error
                    .details
                    .into_iter()
                    .map(|(key, value)| (key, common::redact(&value)))
                    .collect();
                redacted
            }),
            None => Ok(()),
        }
    }

    pub(crate) async fn after_task_hook(
        &self,
        ctx: &crate::worker::TaskHookContext,
        outcome: &ExecutionOutcome,
    ) -> Option<ApiError> {
        let hooks = self.inner.hooks.read().await.clone();
        match hooks {
            Some(hooks) => hooks.after_task(ctx, outcome).await.err().map(|error| {
                let mut redacted = ApiError::new(error.code, common::redact(&error.message));
                redacted.retryable = error.retryable;
                redacted.details = error
                    .details
                    .into_iter()
                    .map(|(key, value)| (key, common::redact(&value)))
                    .collect();
                redacted
            }),
            None => None,
        }
    }

    /// Use the cancellation token registered at admission, or create one.
    pub(crate) async fn cancel_for(&self, task_id: &str) -> CancellationToken {
        if let Some(token) = self.cancel_token(task_id).await {
            token
        } else {
            self.register_cancel(task_id).await
        }
    }

    /// Register a specific cancellation token for an admitted task.
    pub(crate) async fn register_cancel_token(&self, task_id: &str, token: CancellationToken) {
        self.inner
            .cancels
            .lock()
            .await
            .insert(task_id.to_string(), token);
    }

    /// Data directory used by artifact and registry adapters.
    pub fn data_dir(&self) -> PathBuf {
        self.inner.config.data_dir.clone()
    }

    /// Native artifact store shared by this Worker Instance.
    pub fn artifact_store(&self) -> Arc<crate::data::artifacts::NativeArtifactStore> {
        self.inner.artifacts.clone()
    }

    /// Current process incarnation.
    pub fn incarnation(&self) -> &str {
        &self.inner.config.incarnation
    }
}
