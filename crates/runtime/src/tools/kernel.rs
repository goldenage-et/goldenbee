//! One bounded authorization-execute-observe pipeline for native tools.
//!
//! The kernel is a thin `Tool`/`Toolset` decorator. It does not implement file
//! or search behavior; it applies runtime-owned concerns around tools supplied
//! by ADK, a Worker Package, an MCP connection, or a profile-built agent:
//!
//! - capability gating before dispatch;
//! - cooperative cancellation and a wall-clock timeout;
//! - one bounded output ceiling with truncation metadata;
//! - a per-path write lock so conflicting mutations cannot interleave;
//! - bounded parallel execution;
//! - durable `tool.started`, `tool.completed`, and `tool.effect` events;
//! - one redacted audit entry for each mutating call.
//!
//! The pipeline is deliberately shallow. Approval decisions, sandbox profiles,
//! and permission modes are separate tickets and attach to the same descriptor
//! and event fields.

use crate::execution::runner::decode_native_user_id;
use crate::Runtime;
use adk_rust::{ReadonlyContext, Result, Tool, ToolContext, Toolset};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, Semaphore};

/// Risk classification advertised by a runtime tool descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRisk {
    /// Reads bounded data and has no side effect.
    Low,
    /// Can mutate workspace content or call an external system.
    Medium,
    /// Can remove data, execute commands, or reach a remote destination.
    High,
}

impl ToolRisk {
    /// Stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Effect kind observed by the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    /// No known external side effect.
    None,
    /// Creates or overwrites workspace content.
    FileWrite,
    /// Removes workspace content.
    FileDelete,
}

impl ToolEffect {
    /// Stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::FileWrite => "file_write",
            Self::FileDelete => "file_delete",
        }
    }

    /// Whether this effect is a mutation that needs evidence and a path lock.
    pub fn is_mutation(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Runtime-owned metadata for one wrapped native tool.
///
/// The tool itself owns its name, description, and schema. The descriptor adds
/// the mechanism the runtime enforces before and after dispatch.
#[derive(Clone)]
pub struct ToolDescriptor {
    /// Permission exercised by the tool, for example `workspace:read`.
    pub capability: String,
    /// Risk classification.
    pub risk: ToolRisk,
    /// Known effect kind.
    pub effect: ToolEffect,
    /// Explicit per-call timeout.
    pub timeout: Option<Duration>,
    /// Whether `timeout` falling back to the configured default is permitted.
    pub default_timeout: bool,
    /// Per-call output ceiling. `None` uses the configured default.
    pub max_output_bytes: Option<usize>,
    /// Whether ADK tool errors become stable typed JSON results.
    pub typed_errors: bool,
    /// Whether this descriptor is a package-declared built-in capability.
    pub builtin: bool,
    /// Arguments that name workspace paths this call touches.
    pub paths: Option<fn(&Value) -> Vec<String>>,
}

impl std::fmt::Debug for ToolDescriptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolDescriptor")
            .field("capability", &self.capability)
            .field("risk", &self.risk)
            .field("effect", &self.effect)
            .field("timeout", &self.timeout)
            .field("max_output_bytes", &self.max_output_bytes)
            .field("typed_errors", &self.typed_errors)
            .field("builtin", &self.builtin)
            .field("has_paths", &self.paths.is_some())
            .finish()
    }
}

impl ToolDescriptor {
    /// Descriptor for a package or MCP tool with generic capability metadata.
    pub fn new(capability: impl Into<String>) -> Self {
        Self {
            capability: capability.into(),
            risk: ToolRisk::Medium,
            effect: ToolEffect::None,
            timeout: None,
            default_timeout: true,
            max_output_bytes: None,
            typed_errors: false,
            builtin: false,
            paths: None,
        }
    }

    /// Mark the effect kind.
    pub fn effect(mut self, effect: ToolEffect) -> Self {
        self.effect = effect;
        self
    }

    /// Mark the risk.
    pub fn risk(mut self, risk: ToolRisk) -> Self {
        self.risk = risk;
        self
    }

    /// Set an explicit deadline for this tool.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self.default_timeout = false;
        self
    }

    /// Do not apply the configured default deadline.
    ///
    /// Protected effects and long-running orchestration use this: dropping
    /// their future at a harness deadline would turn a possible commitment into
    /// an unknown outcome without a receipt.
    pub fn no_timeout(mut self) -> Self {
        self.default_timeout = false;
        self
    }

    /// Convert ADK tool errors into stable typed JSON.
    pub fn typed_errors(mut self) -> Self {
        self.typed_errors = true;
        self
    }

    /// Mark this descriptor as a package-declared built-in capability.
    pub fn builtin(mut self) -> Self {
        self.builtin = true;
        self
    }

    /// Read workspace paths from the call arguments.
    pub fn paths(mut self, paths: fn(&Value) -> Vec<String>) -> Self {
        self.paths = Some(paths);
        self
    }
}

/// Per-tool metadata factory used by a wrapped dynamic toolset.
pub type ToolDescriptorFactory = Arc<dyn Fn(&dyn Tool) -> ToolDescriptor + Send + Sync>;

/// Resolved runtime limits applied to wrapped tool calls.
#[derive(Debug, Clone)]
pub struct ToolKernelConfig {
    /// Workspace root used for path locks, diff evidence, and ADK devtools.
    /// `None` means this package declared no workspace capability.
    pub workspace: Option<PathBuf>,
    /// Default wall-clock timeout.
    pub timeout: Duration,
    /// Default serialized-result ceiling.
    pub max_output_bytes: usize,
    /// Maximum concurrently executing wrapped tools.
    pub max_parallel: usize,
    /// Package-declared built-in capabilities that are enabled.
    pub enabled: BTreeSet<String>,
}

struct KernelState {
    config: ToolKernelConfig,
    workspace: Option<adk_devtools::Workspace>,
    parallel: Arc<Semaphore>,
}

struct KernelInner {
    state: RwLock<Option<KernelState>>,
    runtime: Arc<OnceLock<Runtime>>,
    locks: Arc<PathLocks>,
}

impl std::fmt::Debug for KernelInner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolKernel")
            .field(
                "configured",
                &self
                    .state
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some(),
            )
            .finish()
    }
}

/// Cloneable handle to the process-wide tool pipeline.
#[derive(Clone, Debug)]
pub struct ToolKernel {
    inner: Arc<KernelInner>,
}

impl Default for ToolKernel {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolKernel {
    /// Create an unconfigured kernel. Tools may be wrapped before startup; every
    /// invocation fails closed until [`configure`](Self::configure) runs.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(KernelInner {
                state: RwLock::new(None),
                runtime: Arc::new(OnceLock::new()),
                locks: Arc::new(PathLocks::default()),
            }),
        }
    }

    /// Bind the durable runtime after state opens.
    pub(crate) fn bind_runtime(&self) -> Arc<OnceLock<Runtime>> {
        self.inner.runtime.clone()
    }

    /// Resolve the operator-selected workspace and limits.
    pub(crate) fn configure(&self, config: ToolKernelConfig) -> Result<()> {
        if config.timeout.is_zero() || config.max_output_bytes == 0 || config.max_parallel == 0 {
            return Err(adk_rust::AdkError::config(
                "tool limits must be greater than zero",
            ));
        }
        let (workspace, devtools) = match config.workspace.clone() {
            Some(path) => {
                std::fs::create_dir_all(&path).map_err(|error| {
                    adk_rust::AdkError::config(format!(
                        "tool workspace could not be created: {error}"
                    ))
                })?;
                let workspace = std::fs::canonicalize(&path).map_err(|error| {
                    adk_rust::AdkError::config(format!(
                        "tool workspace could not be resolved: {error}"
                    ))
                })?;
                let devtools = adk_devtools::Workspace::new(&workspace)
                    .allow_bash(false)
                    .max_output_bytes(config.max_output_bytes);
                (Some(workspace), Some(devtools))
            }
            None => (None, None),
        };
        let mut config = config;
        config.workspace = workspace;
        let parallel = Arc::new(Semaphore::new(config.max_parallel));
        *self
            .inner
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(KernelState {
            config,
            workspace: devtools,
            parallel,
        });
        Ok(())
    }

    /// Whether a package-declared built-in capability is enabled.
    pub fn is_enabled(&self, id: &str) -> bool {
        self.state()
            .and_then(|state| {
                state
                    .as_ref()
                    .map(|state| state.config.enabled.contains(id))
            })
            .unwrap_or(false)
    }

    /// The configured workspace root, when startup resolved one.
    pub fn workspace_root(&self) -> Option<PathBuf> {
        self.state().and_then(|state| {
            state
                .as_ref()
                .and_then(|state| state.config.workspace.clone())
        })
    }

    /// The shared ADK devtools workspace, when configured.
    pub(crate) fn devtools_workspace(&self) -> Option<adk_devtools::Workspace> {
        self.state()
            .and_then(|state| state.as_ref().and_then(|state| state.workspace.clone()))
    }

    /// Wrap one native tool in the runtime pipeline.
    pub fn wrap(&self, tool: Arc<dyn Tool>, descriptor: ToolDescriptor) -> Arc<dyn Tool> {
        Arc::new(KernelTool {
            inner: tool,
            descriptor,
            kernel: self.clone(),
        })
    }

    /// Wrap one native tool with generic descriptor metadata.
    ///
    /// Packages that know the exercised permission should pass an explicit
    /// [`ToolDescriptor`] through [`wrap`](Self::wrap) instead.
    pub fn wrap_tool(&self, tool: Arc<dyn Tool>) -> Arc<dyn Tool> {
        self.wrap(tool, ToolDescriptor::new("tool:call"))
    }

    /// Wrap every tool exposed by a dynamic toolset with generic metadata.
    pub fn wrap_toolset(&self, toolset: Arc<dyn Toolset>) -> Arc<dyn Toolset> {
        Arc::new(KernelToolset {
            inner: toolset,
            kernel: self.clone(),
            descriptor: None,
        })
    }

    /// Wrap every tool exposed by a dynamic toolset with per-tool metadata.
    ///
    /// Packages use this when a toolset owns tools with known effects (for
    /// example a developer toolset with `write_file` and `edit_file`) so those
    /// calls produce effect evidence and path locks like registered built-ins.
    pub fn wrap_toolset_with(
        &self,
        toolset: Arc<dyn Toolset>,
        descriptor: ToolDescriptorFactory,
    ) -> Arc<dyn Toolset> {
        Arc::new(KernelToolset {
            inner: toolset,
            kernel: self.clone(),
            descriptor: Some(descriptor),
        })
    }

    fn state(&self) -> Option<std::sync::RwLockReadGuard<'_, Option<KernelState>>> {
        // The guard is returned as `Some` only when the state is configured.
        // Callers must check the inner option.
        Some(
            self.inner
                .state
                .read()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }

    fn effective(&self) -> Option<EffectiveConfig> {
        let guard = self.state()?;
        guard.as_ref().map(|state| EffectiveConfig {
            config: state.config.clone(),
            parallel: state.parallel.clone(),
        })
    }
}

struct EffectiveConfig {
    config: ToolKernelConfig,
    parallel: Arc<Semaphore>,
}

struct KernelToolset {
    inner: Arc<dyn Toolset>,
    kernel: ToolKernel,
    descriptor: Option<ToolDescriptorFactory>,
}

#[async_trait]
impl Toolset for KernelToolset {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let tools = self.inner.tools(ctx).await?;
        Ok(tools
            .into_iter()
            .map(|tool| match &self.descriptor {
                Some(descriptor) => {
                    let metadata = descriptor(tool.as_ref());
                    self.kernel.wrap(tool, metadata)
                }
                None => self.kernel.wrap_tool(tool),
            })
            .collect())
    }

    async fn process_llm_request(
        &self,
        ctx: Arc<dyn ReadonlyContext>,
        request: &mut adk_rust::LlmRequest,
    ) -> Result<()> {
        self.inner.process_llm_request(ctx, request).await
    }
}

struct KernelTool {
    inner: Arc<dyn Tool>,
    descriptor: ToolDescriptor,
    kernel: ToolKernel,
}

impl KernelTool {
    fn timeout(&self, effective: &EffectiveConfig) -> Option<Duration> {
        self.descriptor.timeout.or_else(|| {
            self.descriptor
                .default_timeout
                .then_some(effective.config.timeout)
        })
    }

    fn output_limit(&self, effective: &EffectiveConfig) -> usize {
        self.descriptor
            .max_output_bytes
            .unwrap_or(effective.config.max_output_bytes)
    }

    fn permission(&self) -> Value {
        json!({
            "decision": "allow",
            "mode": "package",
            "rule": "capability",
            "capability": self.descriptor.capability,
        })
    }

    fn denied_permission(&self) -> Value {
        json!({
            "decision": "deny",
            "mode": "package",
            "rule": "capability_disabled",
            "capability": self.descriptor.capability,
        })
    }

    fn sandbox(&self, effective: &EffectiveConfig) -> Value {
        json!({
            "profile": if self.descriptor.effect.is_mutation() {
                "workspace-write"
            } else {
                "workspace-read"
            },
            "workspace": effective
                .config
                .workspace
                .as_deref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("workspace"),
            "network": false,
        })
    }

    fn redaction() -> Value {
        json!({
            "arguments": "digest",
            "result": "digest",
            "paths": "workspace-relative",
        })
    }

    fn relative_paths(&self, effective: &EffectiveConfig, args: &Value) -> Vec<String> {
        self.descriptor
            .paths
            .map(|paths| paths(args))
            .unwrap_or_default()
            .into_iter()
            .map(|path| {
                effective
                    .config
                    .workspace
                    .as_deref()
                    .map(|workspace| display_path(workspace, &path))
                    .unwrap_or(path)
            })
            .collect()
    }
}

#[async_trait]
impl Tool for KernelTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn is_builtin(&self) -> bool {
        self.inner.is_builtin()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    fn declaration(&self) -> Value {
        self.inner.declaration()
    }

    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }

    fn is_agent_delegation(&self) -> bool {
        self.inner.is_agent_delegation()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(effective) = self.kernel.effective() else {
            return Ok(error_value(
                "tool.not_configured",
                "the Worker tool kernel has not been configured",
            ));
        };
        if self.descriptor.builtin && !self.kernel.is_enabled(self.inner.name()) {
            let task = active_task(&self.kernel, ctx.as_ref()).await;
            self.observe(
                &effective,
                ctx.as_ref(),
                &args,
                task.as_ref(),
                ToolObservation {
                    ok: false,
                    error_code: Some("tool.not_enabled".to_string()),
                    duration: Duration::ZERO,
                    queue_wait: Duration::ZERO,
                    truncated: false,
                    output_bytes: 0,
                    result_digest: String::new(),
                    post_digest: String::new(),
                    diff: Value::Null,
                    effect: None,
                    permission: self.denied_permission(),
                },
            )
            .await;
            return Ok(error_value(
                "tool.not_enabled",
                format!(
                    "tool '{}' is not enabled by this Worker Package",
                    self.inner.name()
                ),
            ));
        }
        // A native Launcher turn has no Goldenbee task. The call still runs with
        // the same bounds; task-scoped events and audit are omitted because no
        // task outcome exists to commit them before.
        let task = active_task(&self.kernel, ctx.as_ref()).await;

        let queue_started = Instant::now();
        let permit_wait = self.timeout(&effective).unwrap_or(Duration::from_secs(300));
        let permit = tokio::select! {
            _ = wait_for_cancel(ctx.clone()) => {
                return Ok(error_value("tool.cancelled", "tool call was cancelled"));
            }
            _ = tokio::time::sleep(permit_wait) => {
                return Ok(error_value(
                    "tool.capacity",
                    "no execution permit became available before the tool deadline",
                ));
            }
            permit = effective.parallel.clone().acquire_owned() => permit,
        };
        let Ok(_permit) = permit else {
            return Ok(error_value(
                "tool.capacity",
                "tool execution is unavailable",
            ));
        };
        let queue_wait = queue_started.elapsed();

        let relative = self.relative_paths(&effective, &args);
        let paths = self.lock_paths(&effective, &args);
        let _path_guards = self.kernel.inner.locks.acquire(&paths).await;
        let before = if self.descriptor.effect.is_mutation() {
            self.pre_image(&effective, &args)
        } else {
            BTreeMap::new()
        };

        let started = Instant::now();
        self.observe_start(
            &effective,
            ctx.as_ref(),
            &args,
            task.as_ref(),
            &relative,
            queue_wait,
        )
        .await;

        // Snapshot the files this mutation is about to touch. A capture failure
        // is fail-closed: an unreversible edit must not be presented as a
        // checkpointed one.
        let checkpoint = match self
            .checkpoint_before(&effective, ctx.as_ref(), task.as_ref(), &relative)
            .await
        {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                let code = error.code.clone();
                self.observe(
                    &effective,
                    ctx.as_ref(),
                    &args,
                    task.as_ref(),
                    ToolObservation {
                        ok: false,
                        error_code: Some(code.clone()),
                        duration: started.elapsed(),
                        queue_wait,
                        truncated: false,
                        output_bytes: 0,
                        result_digest: String::new(),
                        post_digest: String::new(),
                        diff: Value::Null,
                        effect: Some(json!({
                            "kind": self.descriptor.effect.as_str(),
                            "paths": relative,
                        })),
                        permission: self.permission(),
                    },
                )
                .await;
                return Ok(error_value(&code, error.message));
            }
        };

        let deadline = self.timeout(&effective);
        let timeout = async {
            match deadline {
                Some(deadline) => tokio::time::sleep(deadline).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(timeout);
        let mut execution = Box::pin(self.inner.execute(ctx.clone(), args.clone()));
        enum Outcome {
            Complete(adk_rust::Result<Value>),
            Cancelled(Value),
            TimedOut(Value),
        }
        let outcome = tokio::select! {
            biased;
            _ = wait_for_cancel(ctx.clone()) => Outcome::Cancelled(error_value(
                "tool.cancelled",
                "tool call was cancelled",
            )),
            _ = &mut timeout => Outcome::TimedOut(error_value(
                "tool.timeout",
                format!("tool '{}' exceeded its time limit", self.inner.name()),
            )),
            result = &mut execution => Outcome::Complete(result),
        };
        let duration = started.elapsed();
        self.checkpoint_after(&effective, checkpoint.as_deref(), &relative)
            .await;
        let result = match outcome {
            Outcome::Cancelled(value) | Outcome::TimedOut(value) => value,
            Outcome::Complete(Ok(value)) => value,
            Outcome::Complete(Err(error)) => {
                if self.descriptor.typed_errors {
                    typed_error(&error)
                } else {
                    self.observe(
                        &effective,
                        ctx.as_ref(),
                        &args,
                        task.as_ref(),
                        ToolObservation {
                            ok: false,
                            error_code: Some(error.code.to_string()),
                            duration,
                            queue_wait,
                            truncated: false,
                            output_bytes: 0,
                            result_digest: String::new(),
                            post_digest: String::new(),
                            diff: Value::Null,
                            effect: self.descriptor.effect.is_mutation().then_some(json!({
                                "kind": self.descriptor.effect.as_str(),
                                "paths": relative,
                            })),
                            permission: self.permission(),
                        },
                    )
                    .await;
                    return Err(error);
                }
            }
        };

        let error_code = result
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string);
        let (bounded, truncated, output_bytes) =
            bounded_result(result, self.output_limit(&effective));
        let result_digest = common::canonical_digest(&bounded);
        let after = if self.descriptor.effect.is_mutation() {
            self.post_image(&effective, &args)
        } else {
            BTreeMap::new()
        };
        let diff = if self.descriptor.effect.is_mutation() {
            diff_payload(&before, &after)
        } else {
            Value::Null
        };
        let post_digest = if self.descriptor.effect.is_mutation() {
            common::canonical_digest(&after)
        } else {
            String::new()
        };
        let effect = self.descriptor.effect.is_mutation().then(|| {
            json!({
                "kind": self.descriptor.effect.as_str(),
                "paths": relative,
                "digest": result_digest,
                "post_digest": post_digest,
            })
        });
        let observation = ToolObservation {
            ok: error_code.is_none(),
            error_code: error_code.clone(),
            duration,
            queue_wait,
            truncated,
            output_bytes,
            result_digest: result_digest.clone(),
            post_digest,
            diff,
            effect,
            permission: self.permission(),
        };
        self.observe(
            &effective,
            ctx.as_ref(),
            &args,
            task.as_ref(),
            observation.clone(),
        )
        .await;
        self.effect_evidence(
            &effective,
            ctx.as_ref(),
            task.as_ref(),
            &relative,
            &observation,
        )
        .await;
        Ok(bounded)
    }
}

#[derive(Clone)]
struct ToolObservation {
    ok: bool,
    error_code: Option<String>,
    duration: Duration,
    queue_wait: Duration,
    truncated: bool,
    output_bytes: usize,
    result_digest: String,
    post_digest: String,
    diff: Value,
    effect: Option<Value>,
    permission: Value,
}

impl KernelTool {
    async fn observe_start(
        &self,
        effective: &EffectiveConfig,
        ctx: &dyn ToolContext,
        args: &Value,
        task: Option<&common::Task>,
        relative: &[String],
        queue_wait: Duration,
    ) {
        let Some(task) = task else { return };
        let Some(runtime) = self.kernel.inner.runtime.get() else {
            return;
        };
        let payload = json!({
            "tool": self.inner.name(),
            "call_id": ctx.function_call_id(),
            "invocation": ctx.invocation_id(),
            "agent": ctx.agent_name(),
            "capability": self.descriptor.capability,
            "risk": self.descriptor.risk.as_str(),
            "effect": self.descriptor.effect.as_str(),
            "permission": self.permission(),
            "sandbox": self.sandbox(effective),
            "redaction": Self::redaction(),
            "schema_digest": common::canonical_digest(&self.inner.declaration()),
            "arguments_digest": common::canonical_digest(args),
            "paths": relative,
            "timeout_ms": self
                .timeout(effective)
                .map(|timeout| timeout.as_millis() as u64)
                .unwrap_or(0),
            "max_output_bytes": self.output_limit(effective),
            "queue_wait_ms": queue_wait.as_millis() as u64,
        });
        let _ = runtime
            .append_event(&task.id, "tool.started", payload, true)
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn observe(
        &self,
        effective: &EffectiveConfig,
        ctx: &dyn ToolContext,
        args: &Value,
        task: Option<&common::Task>,
        observation: ToolObservation,
    ) {
        let Some(task) = task else { return };
        let Some(runtime) = self.kernel.inner.runtime.get() else {
            return;
        };
        let payload = json!({
            "tool": self.inner.name(),
            "call_id": ctx.function_call_id(),
            "invocation": ctx.invocation_id(),
            "agent": ctx.agent_name(),
            "capability": self.descriptor.capability,
            "risk": self.descriptor.risk.as_str(),
            "effect": self.descriptor.effect.as_str(),
            "ok": observation.ok,
            "error_code": observation.error_code,
            "duration_ms": observation.duration.as_millis() as u64,
            "queue_wait_ms": observation.queue_wait.as_millis() as u64,
            "truncated": observation.truncated,
            "output_bytes": observation.output_bytes,
            "result_digest": observation.result_digest,
            "post_digest": observation.post_digest,
            "diff": observation.diff,
            "effect_evidence": observation.effect,
            "arguments_digest": common::canonical_digest(args),
            "permission": observation.permission,
            "sandbox": self.sandbox(effective),
            "redaction": Self::redaction(),
        });
        let _ = runtime
            .append_event(&task.id, "tool.completed", payload, true)
            .await;
        let attributes = BTreeMap::from([
            ("tool".into(), self.inner.name().to_string()),
            ("capability".into(), self.descriptor.capability.clone()),
            ("effect".into(), self.descriptor.effect.as_str().to_string()),
            ("ok".into(), observation.ok.to_string()),
            (
                "duration_ms".into(),
                observation.duration.as_millis().to_string(),
            ),
        ]);
        let _ = runtime
            .trace(
                &task.trace_id,
                &task.id,
                "tool.execute",
                observation.duration.as_millis() as u64,
                attributes,
            )
            .await;
    }

    async fn effect_evidence(
        &self,
        _effective: &EffectiveConfig,
        ctx: &dyn ToolContext,
        task: Option<&common::Task>,
        relative: &[String],
        observation: &ToolObservation,
    ) {
        if !self.descriptor.effect.is_mutation() {
            return;
        }
        let (Some(task), Some(runtime)) = (task, self.kernel.inner.runtime.get()) else {
            return;
        };
        let payload = json!({
            "tool": self.inner.name(),
            "call_id": ctx.function_call_id(),
            "invocation": ctx.invocation_id(),
            "kind": self.descriptor.effect.as_str(),
            "paths": relative,
            "ok": observation.ok,
            "error_code": observation.error_code,
            "result_digest": observation.result_digest,
            "post_digest": observation.post_digest,
            "diff": observation.diff,
            "redaction": Self::redaction(),
        });
        let _ = runtime
            .append_event(&task.id, "tool.effect", payload, true)
            .await;
        let outcome = if observation.ok {
            "committed"
        } else {
            "failed"
        };
        let detail = format!(
            "tool={} effect={} paths={} digest={}",
            self.inner.name(),
            self.descriptor.effect.as_str(),
            relative.join(","),
            observation.post_digest
        );
        let _ = runtime
            .audit_with_trace(
                &task.caller,
                "tool.effect",
                &task.id,
                outcome,
                &detail,
                &task.trace_id,
            )
            .await;
    }

    async fn checkpoint_before(
        &self,
        effective: &EffectiveConfig,
        ctx: &dyn ToolContext,
        task: Option<&common::Task>,
        relative: &[String],
    ) -> crate::Result<Option<String>> {
        if !self.descriptor.effect.is_mutation() {
            return Ok(None);
        }
        let (Some(task), Some(workspace)) = (task, effective.config.workspace.as_deref()) else {
            return Ok(None);
        };
        let Some(runtime) = self.kernel.inner.runtime.get() else {
            return Ok(None);
        };
        runtime
            .checkpoint_before(task, ctx.invocation_id(), workspace, relative)
            .await
            .map(Some)
    }

    async fn checkpoint_after(
        &self,
        effective: &EffectiveConfig,
        checkpoint: Option<&str>,
        relative: &[String],
    ) {
        let (Some(checkpoint), Some(workspace)) =
            (checkpoint, effective.config.workspace.as_deref())
        else {
            return;
        };
        let Some(runtime) = self.kernel.inner.runtime.get() else {
            return;
        };
        // A post-capture failure leaves the manifest `partial` and is reported
        // through `checkpoint.committed`; it must not rewrite the tool result.
        let _ = runtime
            .checkpoint_after(checkpoint, workspace, relative)
            .await;
    }

    fn pre_image(&self, effective: &EffectiveConfig, args: &Value) -> BTreeMap<String, String> {
        self.images(effective, args)
    }

    fn post_image(&self, effective: &EffectiveConfig, args: &Value) -> BTreeMap<String, String> {
        self.images(effective, args)
    }

    fn images(&self, effective: &EffectiveConfig, args: &Value) -> BTreeMap<String, String> {
        let mut images = BTreeMap::new();
        let Some(workspace) = effective.config.workspace.as_deref() else {
            return images;
        };
        for path in self.relative_paths(effective, args) {
            let resolved = normalized_path(workspace, &path);
            if let Ok(text) = std::fs::read_to_string(&resolved) {
                if text.len() <= 1_048_576 {
                    images.insert(path, text);
                }
            }
        }
        images
    }

    fn lock_paths(&self, effective: &EffectiveConfig, args: &Value) -> Vec<PathBuf> {
        if self.descriptor.paths.is_none() {
            return Vec::new();
        }
        let Some(workspace) = effective.config.workspace.as_deref() else {
            return Vec::new();
        };
        self.descriptor
            .paths
            .map(|paths| paths(args))
            .unwrap_or_default()
            .into_iter()
            .map(|path| lock_path(workspace, &path))
            .collect()
    }
}

async fn active_task(kernel: &ToolKernel, ctx: &dyn ToolContext) -> Option<common::Task> {
    let runtime = kernel.inner.runtime.get()?;
    let (namespace, caller) = decode_native_user_id(ctx.user_id()).ok()?;
    runtime
        .effect_task(ctx.session_id(), &namespace, &caller)
        .await
}

async fn wait_for_cancel(ctx: Arc<dyn ToolContext>) {
    loop {
        if ctx.is_cancelled() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn error_value(code: &str, message: impl Into<String>) -> Value {
    let message: String = message.into();
    json!({
        "error": code,
        "message": common::redact(&message),
    })
}

fn typed_error(error: &adk_rust::AdkError) -> Value {
    let message = common::redact(&error.message);
    let lower = message.to_ascii_lowercase();
    let code = if lower.contains("escapes the workspace root")
        || lower.contains("outside the workspace")
    {
        "tool.path_denied"
    } else if lower.contains("read-only") {
        "tool.read_only"
    } else if lower.contains("must be read") {
        "tool.not_read"
    } else if lower.contains("no occurrence of the target string") {
        "tool.edit_no_match"
    } else if lower.contains("occurs") && lower.contains("times") {
        "tool.edit_ambiguous"
    } else if lower.contains("invalid") || lower.contains("missing required") {
        "tool.invalid_arguments"
    } else if lower.contains("not found")
        || lower.contains("no such file")
        || lower.contains("does not exist")
    {
        "tool.not_found"
    } else if lower.contains("timed out") {
        "tool.timeout"
    } else {
        "tool.failed"
    };
    error_value(code, message)
}

fn display_path(workspace: &Path, raw: &str) -> String {
    let path = Path::new(raw);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    joined
        .strip_prefix(workspace)
        .unwrap_or(&joined)
        .display()
        .to_string()
}

fn lock_path(workspace: &Path, raw: &str) -> PathBuf {
    let path = normalized_path(workspace, raw);
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        if let Ok(canonical) = parent.canonicalize() {
            return canonical.join(name);
        }
    }
    path
}

fn normalized_path(workspace: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn diff_payload(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Value {
    let mut paths: BTreeSet<&String> = before.keys().collect();
    paths.extend(after.keys());
    let mut files = Vec::new();
    let mut additions = 0usize;
    let mut deletions = 0usize;
    for path in paths {
        let old = before.get(path).map(String::as_str).unwrap_or_default();
        let new = after.get(path).map(String::as_str).unwrap_or_default();
        if old == new {
            continue;
        }
        let change = if old.is_empty() {
            "add"
        } else if new.is_empty() {
            "delete"
        } else {
            "modify"
        };
        let (added, removed) = line_changes(old, new);
        additions += added;
        deletions += removed;
        files.push(json!({
            "path": path,
            "change": change,
            "additions": added,
            "deletions": removed,
        }));
    }
    json!({
        "files": files,
        "additions": additions,
        "deletions": deletions,
        "exact": true,
    })
}

fn line_changes(old: &str, new: &str) -> (usize, usize) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let product = old_lines.len().saturating_mul(new_lines.len());
    if product > 4_000_000 {
        return (new_lines.len(), old_lines.len());
    }
    let mut previous = vec![0usize; new_lines.len() + 1];
    let mut current = vec![0usize; new_lines.len() + 1];
    for left in &old_lines {
        for (index, right) in new_lines.iter().enumerate() {
            current[index + 1] = if left == right {
                previous[index] + 1
            } else {
                previous[index + 1].max(current[index])
            };
        }
        std::mem::swap(&mut previous, &mut current);
        current.fill(0);
    }
    let common_lines = previous[new_lines.len()];
    (
        new_lines.len().saturating_sub(common_lines),
        old_lines.len().saturating_sub(common_lines),
    )
}

fn bounded_result(value: Value, max_bytes: usize) -> (Value, bool, usize) {
    let original_bytes = serde_json::to_vec(&value)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    if original_bytes <= max_bytes {
        return (value, false, original_bytes);
    }
    let mut bounded = value;
    for _ in 0..16 {
        let bytes = serde_json::to_vec(&bounded)
            .map(|bytes| bytes.len())
            .unwrap_or(0);
        if bytes <= max_bytes {
            break;
        }
        let excess = bytes.saturating_sub(max_bytes);
        if !shrink_longest_string(&mut bounded, excess) {
            break;
        }
    }
    let mut bytes = serde_json::to_vec(&bounded)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    if bytes > max_bytes {
        return (
            json!({
                "error": "tool.output_limit",
                "message": "the tool result exceeded its output limit",
                "max_bytes": max_bytes,
                "observed_bytes": original_bytes,
                "truncated": true,
            }),
            true,
            original_bytes,
        );
    }
    if let Some(object) = bounded.as_object_mut() {
        object.insert("truncated".into(), Value::Bool(true));
        object.insert("original_bytes".into(), json!(original_bytes));
        object.insert("max_bytes".into(), json!(max_bytes));
        bytes = serde_json::to_vec(&bounded)
            .map(|bytes| bytes.len())
            .unwrap_or(original_bytes);
    }
    (bounded, true, bytes)
}

fn shrink_longest_string(value: &mut Value, excess: usize) -> bool {
    let target = longest_string(value);
    let Some(target) = target else {
        return false;
    };
    let length = target.len();
    let keep = length.saturating_sub(excess.max(32));
    if keep >= length {
        return false;
    }
    let mut boundary = keep;
    while boundary > 0 && !target.is_char_boundary(boundary) {
        boundary -= 1;
    }
    target.truncate(boundary);
    true
}

fn longest_string(value: &mut Value) -> Option<&mut String> {
    match value {
        Value::String(text) => Some(text),
        Value::Array(values) => {
            let mut best: Option<&mut String> = None;
            for value in values {
                if let Some(candidate) = longest_string(value) {
                    if best
                        .as_ref()
                        .is_none_or(|current| candidate.len() > current.len())
                    {
                        best = Some(candidate);
                    }
                }
            }
            best
        }
        Value::Object(map) => {
            let mut best: Option<&mut String> = None;
            for value in map.values_mut() {
                if let Some(candidate) = longest_string(value) {
                    if best
                        .as_ref()
                        .is_none_or(|current| candidate.len() > current.len())
                    {
                        best = Some(candidate);
                    }
                }
            }
            best
        }
        _ => None,
    }
}

#[derive(Default)]
struct PathLocks {
    locks: std::sync::Mutex<BTreeMap<PathBuf, Arc<AsyncMutex<()>>>>,
}

impl PathLocks {
    async fn acquire(&self, paths: &[PathBuf]) -> Vec<OwnedMutexGuard<()>> {
        let mut ordered = paths.to_vec();
        ordered.sort();
        ordered.dedup();
        let mut guards = Vec::with_capacity(ordered.len());
        for path in ordered {
            let lock = {
                let mut locks = self.locks.lock().unwrap_or_else(|error| error.into_inner());
                locks
                    .entry(path)
                    .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                    .clone()
            };
            guards.push(lock.lock_owned().await);
        }
        guards
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_changes_counts_replacements() {
        assert_eq!(line_changes("a\nb\n", "a\nc\n"), (1, 1));
        assert_eq!(line_changes("", "a\nb\n"), (2, 0));
        assert_eq!(line_changes("a\n", ""), (0, 1));
    }

    struct ProbeTool {
        name: &'static str,
        active: Arc<std::sync::atomic::AtomicUsize>,
        peak: Arc<std::sync::atomic::AtomicUsize>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl Tool for ProbeTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "kernel probe"
        }

        fn parameters_schema(&self) -> Option<Value> {
            Some(json!({"type": "object", "properties": {"path": {"type": "string"}}}))
        }

        async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let active = self
                .active
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            self.peak
                .fetch_max(active, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(40)).await;
            self.active
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            Ok(json!({"ok": true}))
        }
    }

    fn configured(enabled: BTreeSet<String>, max_parallel: usize, workspace: &Path) -> ToolKernel {
        let kernel = ToolKernel::new();
        kernel
            .configure(ToolKernelConfig {
                workspace: Some(workspace.to_path_buf()),
                timeout: Duration::from_secs(5),
                max_output_bytes: 1024,
                max_parallel,
                enabled,
            })
            .expect("kernel configures");
        kernel
    }

    fn probe(
        name: &'static str,
    ) -> (
        Arc<dyn Tool>,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        (
            Arc::new(ProbeTool {
                name,
                active: active.clone(),
                peak: peak.clone(),
                calls: calls.clone(),
            }),
            active,
            peak,
            calls,
        )
    }

    fn path_of(args: &Value) -> Vec<String> {
        args.get("path")
            .and_then(Value::as_str)
            .map(|path| vec![path.to_string()])
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn disabled_builtin_is_denied_before_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = configured(BTreeSet::new(), 2, dir.path());
        let (tool, _, _, calls) = probe("read_file");
        let wrapped = kernel.wrap(tool, ToolDescriptor::new("workspace:read").builtin());
        let ctx = Arc::new(adk_tool::SimpleToolContext::new("test")) as Arc<dyn ToolContext>;
        let result = wrapped.execute(ctx, json!({})).await.expect("typed denial");
        assert_eq!(result["error"], "tool.not_enabled");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn same_path_mutations_do_not_interleave() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = configured(BTreeSet::new(), 4, dir.path());
        let (tool, _, peak, _) = probe("writer");
        let wrapped = kernel.wrap(tool, ToolDescriptor::new("workspace:write").paths(path_of));
        let ctx = Arc::new(adk_tool::SimpleToolContext::new("test")) as Arc<dyn ToolContext>;
        let (left, right) = tokio::join!(
            wrapped.execute(ctx.clone(), json!({"path": "same.txt"})),
            wrapped.execute(ctx, json!({"path": "same.txt"})),
        );
        left.expect("left completes");
        right.expect("right completes");
        assert_eq!(peak.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn parallel_calls_are_bounded_by_the_runtime_limit() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = configured(BTreeSet::new(), 1, dir.path());
        let (tool, _, peak, _) = probe("reader");
        let wrapped = kernel.wrap(tool, ToolDescriptor::new("workspace:read"));
        let ctx = Arc::new(adk_tool::SimpleToolContext::new("test")) as Arc<dyn ToolContext>;
        let (left, right) = tokio::join!(
            wrapped.execute(ctx.clone(), json!({"path": "a.txt"})),
            wrapped.execute(ctx, json!({"path": "b.txt"})),
        );
        left.expect("left completes");
        right.expect("right completes");
        assert_eq!(peak.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn bounded_result_truncates_the_longest_string_and_keeps_shape() {
        let (value, truncated, _) =
            bounded_result(json!({"content": "x".repeat(2048), "ok": true}), 200);
        assert!(truncated);
        assert_eq!(value["ok"], true);
        assert_eq!(value["truncated"], true);
        assert!(value["content"].as_str().unwrap_or_default().len() < 2048);
    }
}
