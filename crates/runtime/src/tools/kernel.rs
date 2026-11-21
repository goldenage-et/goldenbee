//! One bounded authorization-execute-observe pipeline for native tools.
//!
//! The kernel is a thin `Tool`/`Toolset` decorator. It does not implement file
//! or search behavior; it applies runtime-owned concerns around tools supplied
//! by ADK, a Worker Package, an MCP connection, or a profile-built agent:
//!
//! - capability gating before dispatch;
//! - permission-mode and ordered-rule authorization before dispatch, with a
//!   bounded ask that reuses the durable approval record;
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
//!
//! Permission asks block inside the tool call, bounded by the policy's
//! `approval_wait_ms`, then return a truthful `approval.pending` result. This
//! mirrors the pinned native confirmation semantics used by protected effects
//! while covering every tool source uniformly. It does not claim that a
//! non-graph task resumes from a pending approval.

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
    /// Whether the tool can reach a network destination.
    pub network: bool,
    /// Whether a successful or failed call is audited even without a mutation.
    pub audit: bool,
    /// Whether the tool can write inside the workspace even when the exact
    /// paths are not known before dispatch.
    pub writes: bool,
    /// Whether this tool owns a stricter exact-operation approval internally.
    ///
    /// The kernel still evaluates deny rules and records the policy decision,
    /// but an `ask` from the mode is delegated to the tool instead of opening a
    /// second approval. Protected effect tools use this.
    pub tool_owned_approval: bool,
    /// Arguments that name workspace paths this call touches.
    pub paths: Option<fn(&Value) -> Vec<String>>,
    /// Arguments that carry the command this call executes.
    pub command: Option<fn(&Value) -> Option<String>>,
    /// Arguments that carry the network host this call reaches.
    pub domain: Option<fn(&Value) -> Option<String>>,
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
            .field("network", &self.network)
            .field("audit", &self.audit)
            .field("writes", &self.writes)
            .field("tool_owned_approval", &self.tool_owned_approval)
            .field("has_paths", &self.paths.is_some())
            .field("has_command", &self.command.is_some())
            .field("has_domain", &self.domain.is_some())
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
            network: false,
            audit: false,
            writes: false,
            tool_owned_approval: false,
            paths: None,
            command: None,
            domain: None,
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

    /// Declare that the tool can reach a network destination.
    pub fn network(mut self) -> Self {
        self.network = true;
        self
    }

    /// Record an audit entry for every call, not only for mutations.
    pub fn audit(mut self) -> Self {
        self.audit = true;
        self
    }

    /// Declare that the tool can write inside the workspace.
    pub fn writes(mut self) -> Self {
        self.writes = true;
        self
    }

    /// Delegate an `ask` decision to this tool's own exact approval.
    pub fn tool_owned_approval(mut self) -> Self {
        self.tool_owned_approval = true;
        self
    }

    /// Read workspace paths from the call arguments.
    pub fn paths(mut self, paths: fn(&Value) -> Vec<String>) -> Self {
        self.paths = Some(paths);
        self
    }

    /// Read the command this call executes from its arguments.
    pub fn command(mut self, command: fn(&Value) -> Option<String>) -> Self {
        self.command = Some(command);
        self
    }

    /// Read the network host this call reaches from its arguments.
    pub fn domain(mut self, domain: fn(&Value) -> Option<String>) -> Self {
        self.domain = Some(domain);
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
    /// Initial permission policy. Runtime updates replace its contents through
    /// [`ToolKernel::permission_handle`].
    pub permissions: crate::policy::PermissionPolicy,
    /// Runtime-owned sandbox policy: profile, network, paths, command
    /// ceiling, proxy, and process limits. Only the operator configuration
    /// sets it; `narrow_sandbox` is the only way to change it afterwards.
    pub sandbox: crate::sandbox::SandboxRules,
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
    permissions: crate::policy::PolicyHandle,
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
                permissions: crate::policy::default_handle(),
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
        config
            .permissions
            .validate()
            .map_err(adk_rust::AdkError::config)?;
        config.sandbox.validate().map_err(|message| {
            adk_rust::AdkError::config(format!("sandbox policy is invalid: {message}"))
        })?;
        *self
            .inner
            .permissions
            .write()
            .unwrap_or_else(|error| error.into_inner()) =
            crate::policy::PolicyState::new(config.permissions.clone());
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

    /// Shared permission policy handle.
    ///
    /// Worker startup hands the same handle to the runtime, so a validated
    /// operator change is visible to subsequent tool calls.
    pub fn permission_handle(&self) -> crate::policy::PolicyHandle {
        self.inner.permissions.clone()
    }

    /// Immutable policy snapshot for one call.
    pub fn permission_snapshot(&self) -> crate::policy::PolicySnapshot {
        self.inner
            .permissions
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot()
    }

    /// Effective sandbox rules for this process, when configured.
    pub fn sandbox_rules(&self) -> Option<crate::sandbox::SandboxRules> {
        self.state()
            .and_then(|state| state.as_ref().map(|state| state.config.sandbox.clone()))
    }

    /// Apply a narrower sandbox overlay.
    ///
    /// A Runtime Profile, plugin, command, or subagent uses this to restrict
    /// work further. The result is the intersection with the operator policy,
    /// so this API cannot widen a profile, re-enable network, add a protected
    /// grant, add a writable root, or add a command prefix.
    pub fn narrow_sandbox(
        &self,
        overlay: &crate::sandbox::SandboxRules,
    ) -> std::result::Result<crate::sandbox::SandboxRules, adk_rust::AdkError> {
        overlay.validate().map_err(|message| {
            adk_rust::AdkError::config(format!("sandbox overlay is invalid: {message}"))
        })?;
        let mut state = self
            .inner
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let Some(state) = state.as_mut() else {
            return Err(adk_rust::AdkError::config(
                "the Worker tool kernel has not been configured",
            ));
        };
        state.config.sandbox = state.config.sandbox.narrowed_by(overlay);
        Ok(state.config.sandbox.clone())
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

    /// The admitted task that owns the current native invocation, when the call
    /// runs inside one. Built-in tools use this to attach task-scoped events
    /// without re-deriving native identity.
    pub(crate) async fn task_for(&self, ctx: &dyn ToolContext) -> Option<common::Task> {
        active_task(self, ctx).await
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

    fn denied_permission(&self) -> Value {
        json!({
            "decision": "deny",
            "mode": "package",
            "rule": "capability_disabled",
            "capability": self.descriptor.capability,
        })
    }

    fn subagent_denied_permission(
        &self,
        snapshot: &crate::policy::PolicySnapshot,
        policy: &common::SubagentPolicy,
    ) -> Value {
        json!({
            "decision": "deny",
            "evaluated": "deny",
            "mode": policy
                .permission_mode
                .as_deref()
                .unwrap_or_else(|| snapshot.mode().as_str()),
            "rule": "subagent_tool_allowlist",
            "reason": "subagent_tool_not_allowed",
            "capability": self.descriptor.capability,
            "policy": snapshot.summary(),
            "approval": null,
        })
    }

    fn sandbox_profile(&self, effective: &EffectiveConfig) -> &'static str {
        effective.config.sandbox.profile.as_str()
    }

    fn action_effect(&self) -> crate::policy::ActionEffect {
        match self.descriptor.effect {
            ToolEffect::None => crate::policy::ActionEffect::None,
            ToolEffect::FileWrite => crate::policy::ActionEffect::FileWrite,
            ToolEffect::FileDelete => crate::policy::ActionEffect::FileDelete,
        }
    }

    fn policy_request(
        &self,
        effective: &EffectiveConfig,
        args: &Value,
        relative: &[String],
        task: Option<&common::Task>,
    ) -> crate::policy::PermissionRequest {
        crate::policy::PermissionRequest {
            tool: self.inner.name().to_string(),
            capability: self.descriptor.capability.clone(),
            effect: self.action_effect(),
            high_risk: matches!(self.descriptor.risk, ToolRisk::High),
            arguments_digest: common::canonical_digest(&crate::tools::effect::canonical_arguments(
                args,
            )),
            paths: relative.to_vec(),
            command: self
                .descriptor
                .command
                .and_then(|extract| extract(args))
                .map(|command| common::redact(&command)),
            domain: self.descriptor.domain.and_then(|extract| extract(args)),
            principal: task
                .map(|task| task.caller.clone())
                .unwrap_or_else(|| "local".into()),
            profile_revision: task.map(|task| task.profile_revision).unwrap_or(0),
            sandbox_profile: self.sandbox_profile(effective).to_string(),
        }
    }

    fn hook_context(
        &self,
        effective: &EffectiveConfig,
        ctx: &dyn ToolContext,
        task: Option<&common::Task>,
        request: &crate::policy::PermissionRequest,
        relative: &[String],
        permission: &Value,
    ) -> crate::worker::ToolHookContext {
        let decision = permission
            .get("decision")
            .and_then(Value::as_str)
            .unwrap_or("allow");
        crate::worker::ToolHookContext {
            task_id: task.map(|task| task.id.clone()),
            namespace: task
                .map(|task| task.namespace.clone())
                .unwrap_or_else(|| "local".into()),
            caller: task
                .map(|task| task.caller.clone())
                .unwrap_or_else(|| "local".into()),
            agent: ctx.agent_name().to_string(),
            session_id: ctx.session_id().to_string(),
            invocation: ctx.invocation_id().to_string(),
            call_id: ctx.function_call_id().to_string(),
            tool: self.inner.name().to_string(),
            capability: self.descriptor.capability.clone(),
            risk: self.descriptor.risk.as_str().to_string(),
            effect: self.descriptor.effect.as_str().to_string(),
            arguments_digest: request.arguments_digest.clone(),
            paths: relative.to_vec(),
            command: request.command.clone(),
            domain: request.domain.clone(),
            sandbox: self.sandbox_profile(effective).to_string(),
            permission: decision.to_string(),
            profile: task.map(|task| task.profile.clone()).unwrap_or_default(),
            profile_revision: task.map(|task| task.profile_revision).unwrap_or(0),
        }
    }

    fn permission_value(
        &self,
        evaluated: &crate::policy::PolicyDecision,
        snapshot: &crate::policy::PolicySnapshot,
        decision: &str,
        reason: &str,
        approval: Option<&common::Approval>,
    ) -> Value {
        json!({
            "decision": decision,
            "evaluated": evaluated.decision.as_str(),
            "mode": evaluated.mode.as_str(),
            "rule": evaluated.rule,
            "reason": reason,
            "capability": self.descriptor.capability,
            "policy": snapshot.summary(),
            "approval": approval.map(|approval| json!({
                "id": approval.id,
                "status": approval.status,
            })),
        })
    }

    fn sandbox(&self, effective: &EffectiveConfig) -> Value {
        let rules = &effective.config.sandbox;
        json!({
            "profile": rules.profile.as_str(),
            "workspace": effective
                .config
                .workspace
                .as_deref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("workspace"),
            "network": {
                "enabled": rules.network.enabled,
                "tool": self.descriptor.network,
            },
            "policy": rules.summary(),
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

    fn sandbox_paths(
        &self,
        effective: &EffectiveConfig,
        args: &Value,
    ) -> std::result::Result<Vec<String>, crate::sandbox::SandboxViolation> {
        let declared = self
            .descriptor
            .paths
            .map(|paths| paths(args))
            .unwrap_or_default();
        if declared.is_empty() {
            return Ok(Vec::new());
        }
        let Some(workspace) = effective.config.workspace.as_deref() else {
            // A package tool may own its own workspace (for example a
            // provisioned git worktree). Its descriptor still supplies
            // workspace-relative paths, so apply the lexical policy and let
            // the tool's own containment resolve the root.
            return declared
                .iter()
                .map(|path| {
                    normalize_relative(path).ok_or_else(|| {
                        crate::sandbox::SandboxViolation::new(
                            "sandbox.path_denied",
                            "the path escapes the tool workspace root",
                        )
                    })
                })
                .collect();
        };
        declared
            .iter()
            .map(|path| {
                resolve_tool_path(workspace, path).ok_or_else(|| {
                    crate::sandbox::SandboxViolation::new(
                        "sandbox.path_denied",
                        "the path escapes the workspace root or a symlink",
                    )
                })
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_sandbox(
        &self,
        effective: &EffectiveConfig,
        ctx: &dyn ToolContext,
        task: Option<&common::Task>,
        relative: &[String],
        command: Option<&str>,
        domain: Option<&str>,
        decision: &crate::sandbox::SandboxDecision,
    ) {
        let (Some(task), Some(runtime)) = (task, self.kernel.inner.runtime.get()) else {
            return;
        };
        let rules = &effective.config.sandbox;
        let (outcome, code, message) = match decision {
            crate::sandbox::SandboxDecision::Allow { .. } => ("allow", None, None),
            crate::sandbox::SandboxDecision::Deny(violation) => (
                "deny",
                Some(violation.code.clone()),
                Some(violation.message.clone()),
            ),
        };
        let command_digest = command
            .map(|command| common::canonical_digest(&command))
            .unwrap_or_default();
        let payload = json!({
            "tool": self.inner.name(),
            "call_id": ctx.function_call_id(),
            "invocation": ctx.invocation_id(),
            "agent": ctx.agent_name(),
            "decision": outcome,
            "code": code,
            "message": message.as_deref().map(common::redact),
            "profile": rules.profile.as_str(),
            "network": {
                "enabled": rules.network.enabled,
                "allowed": domain.map(|host| rules.network.allows_host(host)),
            },
            "paths": relative,
            "command_digest": command_digest,
            "domain": domain,
            "exceptions": decision.exceptions(),
            "policy": rules.summary(),
            "redaction": {
                "command": "digest",
                "paths": "workspace-relative",
            },
        });
        let _ = runtime
            .append_event(&task.id, "sandbox.decision", payload, true)
            .await;
        let exceptions = decision.exceptions();
        if outcome == "deny" || !exceptions.is_empty() {
            let detail = format!(
                "profile={} decision={} code={} reason={} paths={} command_digest={} domain={} exceptions={}",
                rules.profile.as_str(),
                outcome,
                code.as_deref().unwrap_or(""),
                message.as_deref().unwrap_or(""),
                relative.join(","),
                command_digest,
                domain.unwrap_or(""),
                exceptions
                    .iter()
                    .map(|exception| exception.kind.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            let audit_outcome = if outcome == "deny" {
                "deny"
            } else {
                "exception"
            };
            let _ = runtime
                .audit_with_trace(
                    &task.caller,
                    "sandbox.decision",
                    &task.id,
                    audit_outcome,
                    &detail,
                    &task.trace_id,
                )
                .await;
        }
    }

    async fn authorize(
        &self,
        evaluated: &crate::policy::PolicyDecision,
        snapshot: &crate::policy::PolicySnapshot,
        request: &crate::policy::PermissionRequest,
        task: Option<&common::Task>,
    ) -> Authorization {
        match evaluated.decision {
            crate::policy::Decision::Allow => Authorization::Allowed {
                decision: "allow",
                reason: evaluated.reason.clone(),
                approval: None,
            },
            crate::policy::Decision::Deny => Authorization::Denied {
                decision: "deny",
                reason: evaluated.reason.clone(),
                approval: None,
            },
            crate::policy::Decision::Ask
                if self.descriptor.tool_owned_approval && evaluated.rule.is_none() =>
            {
                // Protected effect tools run their own exact approval. The
                // mode's ask is delegated so the runtime does not open a second
                // approval or weaken the tool's wait. An explicit ask rule is
                // still enforced here.
                Authorization::Allowed {
                    decision: "allow",
                    reason: "tool_owned_approval".into(),
                    approval: None,
                }
            }
            crate::policy::Decision::Ask => self.ask(evaluated, snapshot, request, task).await,
        }
    }

    async fn ask(
        &self,
        evaluated: &crate::policy::PolicyDecision,
        snapshot: &crate::policy::PolicySnapshot,
        request: &crate::policy::PermissionRequest,
        task: Option<&common::Task>,
    ) -> Authorization {
        let Some(task) = task else {
            // Without a durable task there is nowhere to record a decision.
            // Fail closed rather than silently running the action.
            return Authorization::Denied {
                decision: "deny",
                reason: "approval_requires_task".into(),
                approval: None,
            };
        };
        let Some(runtime) = self.kernel.inner.runtime.get() else {
            return Authorization::Denied {
                decision: "deny",
                reason: "approval_runtime_unavailable".into(),
                approval: None,
            };
        };
        let digest = request.action_digest();
        let operation = self.inner.name();
        let existing = runtime
            .approval_for_operation(&task.id, operation, &digest)
            .await;
        let approval = match existing {
            Some(approval) => approval,
            None => {
                let target = approval_target(request);
                match runtime
                    .request_approval_with_action(
                        &task.id,
                        operation,
                        &target,
                        &digest,
                        self.descriptor.risk.as_str(),
                        snapshot.approval_ttl_ms().max(1) as i64,
                        Some(request.approval_action()),
                        Some(evaluated.mode.as_str().to_string()),
                        evaluated.rule.clone(),
                    )
                    .await
                {
                    Ok(approval) => approval,
                    Err(error) => {
                        let _ = runtime
                            .append_event(
                                &task.id,
                                "approval.unavailable",
                                json!({
                                    "tool": operation,
                                    "code": error.code,
                                    "message": common::redact(&error.message),
                                }),
                                true,
                            )
                            .await;
                        runtime.set_pause(&task.id, crate::PAUSE_RUNNING).await;
                        return Authorization::Denied {
                            decision: "deny",
                            reason: "approval_unavailable".into(),
                            approval: None,
                        };
                    }
                }
            }
        };
        let outcome = settle_approval(runtime, approval, snapshot).await;
        runtime.set_pause(&task.id, crate::PAUSE_RUNNING).await;
        match outcome {
            AskOutcome::Allowed(approval) => Authorization::Allowed {
                decision: "allow",
                reason: "approval_approved".into(),
                approval: Some(approval),
            },
            AskOutcome::Denied { reason, approval } => Authorization::Denied {
                decision: "deny",
                reason,
                approval,
            },
            AskOutcome::Pending(approval) => Authorization::Pending {
                decision: "ask",
                reason: "approval_pending".into(),
                approval,
            },
        }
    }
}

enum Authorization {
    Allowed {
        decision: &'static str,
        reason: String,
        approval: Option<common::Approval>,
    },
    Denied {
        decision: &'static str,
        reason: String,
        approval: Option<common::Approval>,
    },
    Pending {
        decision: &'static str,
        reason: String,
        approval: common::Approval,
    },
}

impl Authorization {
    fn decision(&self) -> &'static str {
        match self {
            Self::Allowed { decision, .. }
            | Self::Denied { decision, .. }
            | Self::Pending { decision, .. } => decision,
        }
    }

    fn reason(&self) -> &str {
        match self {
            Self::Allowed { reason, .. }
            | Self::Denied { reason, .. }
            | Self::Pending { reason, .. } => reason,
        }
    }

    fn approval(&self) -> Option<&common::Approval> {
        match self {
            Self::Allowed { approval, .. } => approval.as_ref(),
            Self::Denied { approval, .. } => approval.as_ref(),
            Self::Pending { approval, .. } => Some(approval),
        }
    }
}

enum AskOutcome {
    Allowed(common::Approval),
    Denied {
        reason: String,
        approval: Option<common::Approval>,
    },
    Pending(common::Approval),
}

async fn settle_approval(
    runtime: &Runtime,
    approval: common::Approval,
    snapshot: &crate::policy::PolicySnapshot,
) -> AskOutcome {
    let wait_ms = snapshot.approval_wait_ms();
    let mut current = approval;
    loop {
        match current.status.as_str() {
            "approved" if common::now_ms() < current.expires_at => {
                return AskOutcome::Allowed(current)
            }
            "approved" => {
                return AskOutcome::Denied {
                    reason: "approval_expired".into(),
                    approval: Some(current),
                }
            }
            "rejected" => {
                return AskOutcome::Denied {
                    reason: "approval_rejected".into(),
                    approval: Some(current),
                }
            }
            "revoked" => {
                return AskOutcome::Denied {
                    reason: "approval_revoked".into(),
                    approval: Some(current),
                }
            }
            "expired" => {
                return AskOutcome::Denied {
                    reason: "approval_expired".into(),
                    approval: Some(current),
                }
            }
            _ => {}
        }
        if wait_ms == 0 {
            return AskOutcome::Pending(current);
        }
        let remaining = (current.expires_at - common::now_ms()).max(1) as u64;
        let wait = remaining.min(wait_ms);
        match runtime
            .wait_for_approval(&current.id, Duration::from_millis(wait))
            .await
        {
            Ok(decided) => current = decided,
            Err(error) if error.code == "approval.expired" => {
                return AskOutcome::Denied {
                    reason: "approval_expired".into(),
                    approval: Some(current),
                }
            }
            Err(_) => return AskOutcome::Pending(current),
        }
    }
}

fn approval_target(request: &crate::policy::PermissionRequest) -> String {
    let mut target = format!("tool={}", request.tool);
    if !request.paths.is_empty() {
        target.push_str(&format!(" paths={}", request.paths.join(",")));
    }
    if let Some(command) = &request.command {
        target.push_str(&format!(" command={}", common::redact(command)));
    }
    if let Some(domain) = &request.domain {
        target.push_str(&format!(" domain={domain}"));
    }
    target.push_str(&format!(" sandbox={}", request.sandbox_profile));
    if target.len() > 1024 {
        target.truncate(1021);
        target.push_str("...");
    }
    target
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
                    duration: None,
                    queue_wait: None,
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
        // One snapshot per call: a mode or rule change applies only to later
        // calls and both the decision and the recorded evidence use this view.
        let snapshot = self.kernel.permission_snapshot();
        let command = self.descriptor.command.and_then(|extract| extract(&args));
        let domain = self.descriptor.domain.and_then(|extract| extract(&args));
        // The sandbox boundary runs before authorization: a profile that
        // cannot execute an action must not open an approval or reach a Worker
        // hook that could claim it was allowed.
        let (relative, sandbox_decision) = match self.sandbox_paths(&effective, &args) {
            Ok(relative) => {
                let request = crate::sandbox::SandboxRequest {
                    action: if self.descriptor.effect.is_mutation() || self.descriptor.writes {
                        crate::sandbox::SandboxAction::Write
                    } else {
                        crate::sandbox::SandboxAction::Read
                    },
                    paths: &relative,
                    command: command.as_deref(),
                    domain: domain.as_deref(),
                    network_tool: self.descriptor.network,
                };
                let decision = effective.config.sandbox.evaluate(&request);
                (relative, decision)
            }
            Err(violation) => (Vec::new(), crate::sandbox::SandboxDecision::Deny(violation)),
        };
        self.record_sandbox(
            &effective,
            ctx.as_ref(),
            task.as_ref(),
            &relative,
            command.as_deref(),
            domain.as_deref(),
            &sandbox_decision,
        )
        .await;
        if let crate::sandbox::SandboxDecision::Deny(violation) = &sandbox_decision {
            let observation = ToolObservation {
                ok: false,
                error_code: Some(violation.code.clone()),
                duration: None,
                queue_wait: None,
                truncated: false,
                output_bytes: 0,
                result_digest: String::new(),
                post_digest: String::new(),
                diff: Value::Null,
                effect: self.descriptor.effect.is_mutation().then(|| {
                    json!({
                        "kind": self.descriptor.effect.as_str(),
                        "paths": relative,
                    })
                }),
                permission: json!({
                    "decision": "deny",
                    "mode": "sandbox",
                    "sandbox": effective.config.sandbox.summary(),
                }),
            };
            self.observe(
                &effective,
                ctx.as_ref(),
                &args,
                task.as_ref(),
                observation.clone(),
            )
            .await;
            // A denied mutation is evidence of a failed effect, never a
            // committed one.
            self.effect_evidence(
                &effective,
                ctx.as_ref(),
                task.as_ref(),
                &relative,
                &observation,
            )
            .await;
            return Ok(error_value(&violation.code, &violation.message));
        }
        // A subagent's frozen tool allowlist and permission ceiling are part of
        // the same single enforcement point. No child can widen either by
        // naming a tool the definition did not authorize.
        let subagent_policy = task
            .as_ref()
            .and_then(|task| task.subagent.as_ref())
            .cloned();
        if let Some(policy) = &subagent_policy {
            if !policy.allows_tool(self.inner.name()) {
                let permission = self.subagent_denied_permission(&snapshot, policy);
                self.observe(
                    &effective,
                    ctx.as_ref(),
                    &args,
                    task.as_ref(),
                    ToolObservation {
                        ok: false,
                        error_code: Some("permission.denied".to_string()),
                        duration: None,
                        queue_wait: None,
                        truncated: false,
                        output_bytes: 0,
                        result_digest: String::new(),
                        post_digest: String::new(),
                        diff: Value::Null,
                        effect: None,
                        permission,
                    },
                )
                .await;
                return Ok(error_value(
                    "permission.denied",
                    format!(
                        "tool '{}' is outside this subagent's frozen tool allowlist",
                        self.inner.name()
                    ),
                ));
            }
        }
        let request = self.policy_request(&effective, &args, &relative, task.as_ref());
        let evaluated = match subagent_policy
            .as_ref()
            .and_then(crate::subagents::mode_ceiling)
        {
            Some(mode) => snapshot.evaluate_with_mode(mode, &request),
            None => snapshot.evaluate(&request),
        };
        let authorization = self
            .authorize(&evaluated, &snapshot, &request, task.as_ref())
            .await;
        let permission = self.permission_value(
            &evaluated,
            &snapshot,
            authorization.decision(),
            authorization.reason(),
            authorization.approval(),
        );
        // A protected effect tool owns its exact approval. Its own
        // `approval.requested` event is the decision evidence, so the kernel
        // does not add a second write before that request. Deny and explicit
        // ask rules still record the permission decision here.
        if authorization.reason() != "tool_owned_approval" {
            self.record_permission(
                &effective,
                ctx.as_ref(),
                task.as_ref(),
                &request,
                &evaluated,
                &snapshot,
                &authorization,
            )
            .await;
        }
        if !matches!(authorization, Authorization::Allowed { .. }) {
            let (code, message) = match &authorization {
                Authorization::Pending { approval, .. } => (
                    "approval.pending",
                    format!(
                        "tool '{}' is waiting for approval '{}'",
                        self.inner.name(),
                        approval.id
                    ),
                ),
                _ => (
                    "permission.denied",
                    format!(
                        "tool '{}' was denied by permission policy ({})",
                        self.inner.name(),
                        authorization.reason()
                    ),
                ),
            };
            self.observe(
                &effective,
                ctx.as_ref(),
                &args,
                task.as_ref(),
                ToolObservation {
                    ok: false,
                    error_code: Some(code.to_string()),
                    duration: None,
                    queue_wait: None,
                    truncated: false,
                    output_bytes: 0,
                    result_digest: String::new(),
                    post_digest: String::new(),
                    diff: Value::Null,
                    effect: None,
                    permission,
                },
            )
            .await;
            return Ok(error_value(code, message));
        }

        // Hooks run after authorization and sandbox decisions. A hook may only
        // deny further; a failure or timeout never grants the call.
        let hook_ctx = self.hook_context(
            &effective,
            ctx.as_ref(),
            task.as_ref(),
            &request,
            &relative,
            &permission,
        );
        if let Some(runtime) = self.kernel.inner.runtime.get() {
            if let crate::worker::HookDecision::Deny { reason } =
                runtime.pre_tool_hook(&hook_ctx).await
            {
                if let Some(task) = task.as_ref() {
                    let _ = runtime
                        .append_event(
                            &task.id,
                            "hook.denied",
                            json!({
                                "event": "pre_tool",
                                "tool": self.inner.name(),
                                "call_id": ctx.function_call_id(),
                                "reason": common::redact(&reason),
                            }),
                            true,
                        )
                        .await;
                }
                return Ok(error_value(
                    "hook.denied",
                    format!(
                        "tool '{}' was denied by a pre-tool hook: {reason}",
                        self.inner.name()
                    ),
                ));
            }
        }

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
            &permission,
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
                        duration: Some(started.elapsed()),
                        queue_wait: Some(queue_wait),
                        truncated: false,
                        output_bytes: 0,
                        result_digest: String::new(),
                        post_digest: String::new(),
                        diff: Value::Null,
                        effect: Some(json!({
                            "kind": self.descriptor.effect.as_str(),
                            "paths": relative,
                        })),
                        permission: permission.clone(),
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
                            duration: Some(duration),
                            queue_wait: Some(queue_wait),
                            truncated: false,
                            output_bytes: 0,
                            result_digest: String::new(),
                            post_digest: String::new(),
                            diff: Value::Null,
                            effect: self.descriptor.effect.is_mutation().then_some(json!({
                                "kind": self.descriptor.effect.as_str(),
                                "paths": relative,
                            })),
                            permission: permission.clone(),
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
            duration: Some(duration),
            queue_wait: Some(queue_wait),
            truncated,
            output_bytes,
            result_digest: result_digest.clone(),
            post_digest,
            diff,
            effect,
            permission: permission.clone(),
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
        self.record_diff(
            &effective,
            ctx.as_ref(),
            task.as_ref(),
            &relative,
            &observation,
        )
        .await;
        if let Some(runtime) = self.kernel.inner.runtime.get() {
            runtime
                .post_tool_hook(
                    &hook_ctx,
                    &crate::worker::ToolHookOutcome {
                        ok: observation.ok,
                        error_code: observation.error_code.clone(),
                        duration_ms: observation.duration.map_or(0, |it| it.as_millis() as u64),
                        result_digest: observation.result_digest.clone(),
                        output_bytes: observation.output_bytes,
                        truncated: observation.truncated,
                    },
                )
                .await;
        }
        Ok(bounded)
    }
}

#[derive(Clone)]
struct ToolObservation {
    ok: bool,
    error_code: Option<String>,
    duration: Option<Duration>,
    queue_wait: Option<Duration>,
    truncated: bool,
    output_bytes: usize,
    result_digest: String,
    post_digest: String,
    diff: Value,
    effect: Option<Value>,
    permission: Value,
}

impl KernelTool {
    #[allow(clippy::too_many_arguments)]
    async fn record_permission(
        &self,
        effective: &EffectiveConfig,
        ctx: &dyn ToolContext,
        task: Option<&common::Task>,
        request: &crate::policy::PermissionRequest,
        evaluated: &crate::policy::PolicyDecision,
        snapshot: &crate::policy::PolicySnapshot,
        authorization: &Authorization,
    ) {
        let (Some(task), Some(runtime)) = (task, self.kernel.inner.runtime.get()) else {
            return;
        };
        let approval = authorization.approval();
        let payload = json!({
            "tool": self.inner.name(),
            "call_id": ctx.function_call_id(),
            "invocation": ctx.invocation_id(),
            "agent": ctx.agent_name(),
            "decision": authorization.decision(),
            "evaluated": evaluated.decision.as_str(),
            "mode": evaluated.mode.as_str(),
            "rule": evaluated.rule,
            "reason": authorization.reason(),
            "capability": self.descriptor.capability,
            "risk": self.descriptor.risk.as_str(),
            "effect": self.descriptor.effect.as_str(),
            "paths": request.paths,
            "command": request.command,
            "domain": request.domain,
            "arguments_digest": request.arguments_digest,
            "action_digest": request.action_digest(),
            "sandbox": self.sandbox(effective),
            "policy": snapshot.summary(),
            "approval": approval.map(|approval| json!({
                "id": approval.id,
                "status": approval.status,
                "expires_at": approval.expires_at,
            })),
            "redaction": Self::redaction(),
        });
        let _ = runtime
            .append_event(&task.id, "permission.decision", payload, true)
            .await;
        if let Authorization::Pending { approval, .. } = authorization {
            let _ = runtime
                .append_event(
                    &task.id,
                    "approval.pending",
                    json!({
                        "tool": self.inner.name(),
                        "call_id": ctx.function_call_id(),
                        "approval_id": approval.id,
                        "operation": approval.operation,
                        "target": approval.target,
                        "digest": approval.digest,
                        "expires_at": approval.expires_at,
                        "action": approval.action,
                        "message": "the tool call returned without a decision; continuing the turn does not grant the action",
                    }),
                    true,
                )
                .await;
        }
        let command_digest = request
            .command
            .as_deref()
            .map(|command| common::canonical_digest(&command))
            .unwrap_or_default();
        let detail = format!(
            "tool={} mode={} rule={} decision={} reason={} paths={} command_digest={} domain={} policy_revision={} policy_digest={} approval={}",
            self.inner.name(),
            evaluated.mode.as_str(),
            evaluated.rule.as_deref().unwrap_or("mode"),
            authorization.decision(),
            authorization.reason(),
            request.paths.join(","),
            command_digest,
            request.domain.as_deref().unwrap_or(""),
            snapshot.revision,
            snapshot.digest,
            approval.map(|approval| approval.id.as_str()).unwrap_or(""),
        );
        let _ = runtime
            .audit_with_trace(
                &task.caller,
                "permission.decision",
                &task.id,
                authorization.decision(),
                &detail,
                &task.trace_id,
            )
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn observe_start(
        &self,
        effective: &EffectiveConfig,
        ctx: &dyn ToolContext,
        args: &Value,
        task: Option<&common::Task>,
        relative: &[String],
        queue_wait: Duration,
        permission: &Value,
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
            "permission": permission,
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
        let mut payload = json!({
            "tool": self.inner.name(),
            "call_id": ctx.function_call_id(),
            "invocation": ctx.invocation_id(),
            "agent": ctx.agent_name(),
            "capability": self.descriptor.capability,
            "risk": self.descriptor.risk.as_str(),
            "effect": self.descriptor.effect.as_str(),
            "ok": observation.ok,
            "error_code": observation.error_code,
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
        // A denied or rejected call was not executed, so it has no measured
        // duration or queue wait. The fields are omitted instead of reported
        // as zero.
        if let Some(duration) = observation.duration {
            payload["duration_ms"] = json!(duration.as_millis() as u64);
        }
        if let Some(wait) = observation.queue_wait {
            payload["queue_wait_ms"] = json!(wait.as_millis() as u64);
        }
        let _ = runtime
            .append_event(&task.id, "tool.completed", payload, true)
            .await;
        if let Some(duration) = observation.duration {
            let attributes = BTreeMap::from([
                ("tool".into(), self.inner.name().to_string()),
                ("capability".into(), self.descriptor.capability.clone()),
                ("effect".into(), self.descriptor.effect.as_str().to_string()),
                ("ok".into(), observation.ok.to_string()),
                ("duration_ms".into(), duration.as_millis().to_string()),
            ]);
            let _ = runtime
                .trace(
                    &task.trace_id,
                    &task.id,
                    "tool.execute",
                    duration.as_millis() as u64,
                    attributes,
                )
                .await;
        }
        if self.descriptor.audit && !self.descriptor.effect.is_mutation() {
            let outcome = if observation.ok { "ok" } else { "failed" };
            let detail = format!(
                "tool={} capability={} ok={} error={} duration_ms={}",
                self.inner.name(),
                self.descriptor.capability,
                observation.ok,
                observation.error_code.as_deref().unwrap_or(""),
                observation
                    .duration
                    .map(|duration| duration.as_millis().to_string())
                    .unwrap_or_default(),
            );
            let _ = runtime
                .audit_with_trace(
                    &task.caller,
                    "tool.call",
                    &task.id,
                    outcome,
                    &detail,
                    &task.trace_id,
                )
                .await;
        }
    }

    /// Record the ordered diff produced by a mutating tool call.
    ///
    /// `tool.completed` already carries the bounded diff summary for one call.
    /// This durable event gives Clients a diff stream position without parsing
    /// every tool payload, and it never includes file content or host paths.
    async fn record_diff(
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
        let files = observation
            .diff
            .get("files")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if files.is_empty() {
            return;
        }
        let Some(task) = task else { return };
        let Some(runtime) = self.kernel.inner.runtime.get() else {
            return;
        };
        let mut payload = json!({
            "tool": self.inner.name(),
            "call_id": ctx.function_call_id(),
            "invocation": ctx.invocation_id(),
            "agent": ctx.agent_name(),
            "paths": relative,
            "files": files,
            "ok": observation.ok,
            "redaction": {
                "paths": "workspace-relative",
                "content": "omitted",
            },
        });
        for key in ["additions", "deletions", "exact"] {
            if let Some(value) = observation.diff.get(key) {
                payload[key] = value.clone();
            }
        }
        let _ = runtime
            .append_event(&task.id, "diff.recorded", payload, true)
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
        .task_for_call(ctx.session_id(), &namespace, &caller)
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

/// Resolve one tool-declared path to a workspace-relative path, rejecting
/// lexical escape and symlinks that leave the workspace.
fn resolve_tool_path(workspace: &Path, raw: &str) -> Option<String> {
    let path = Path::new(raw);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    let normalized = normalize_lexical(&joined)?;
    let relative = normalized.strip_prefix(workspace).ok()?;
    // Canonicalize the nearest existing ancestor. A new file or directory
    // inherits the containment of its parent, and a symlinked prefix that
    // leaves the workspace is rejected.
    let mut ancestor = normalized.as_path();
    loop {
        match ancestor.canonicalize() {
            Ok(canonical) => {
                if !canonical.starts_with(workspace) {
                    return None;
                }
                break;
            }
            Err(_) => ancestor = ancestor.parent()?,
        }
    }
    Some(relative.to_string_lossy().into_owned())
}

/// Normalize a workspace-relative path without a known root, rejecting an
/// absolute path or a `..` that escapes the relative root.
fn normalize_relative(raw: &str) -> Option<String> {
    let path = Path::new(raw);
    if path.is_absolute() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

fn normalize_lexical(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
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
        configured_with(
            enabled,
            max_parallel,
            workspace,
            crate::policy::PermissionPolicy::default(),
        )
    }

    fn configured_with(
        enabled: BTreeSet<String>,
        max_parallel: usize,
        workspace: &Path,
        permissions: crate::policy::PermissionPolicy,
    ) -> ToolKernel {
        let kernel = ToolKernel::new();
        kernel
            .configure(ToolKernelConfig {
                workspace: Some(workspace.to_path_buf()),
                timeout: Duration::from_secs(5),
                max_output_bytes: 1024,
                max_parallel,
                enabled,
                permissions,
                sandbox: crate::sandbox::SandboxRules::default(),
            })
            .expect("kernel configures");
        kernel
    }

    fn configured_sandbox(workspace: &Path, sandbox: crate::sandbox::SandboxRules) -> ToolKernel {
        let kernel = ToolKernel::new();
        kernel
            .configure(ToolKernelConfig {
                workspace: Some(workspace.to_path_buf()),
                timeout: Duration::from_secs(5),
                max_output_bytes: 1024,
                max_parallel: 2,
                enabled: BTreeSet::new(),
                permissions: crate::policy::PermissionPolicy {
                    mode: crate::policy::PermissionMode::FullAuto,
                    ..crate::policy::PermissionPolicy::default()
                },
                sandbox,
            })
            .expect("kernel configures");
        kernel
    }

    fn permission_policy(
        rules: Vec<crate::policy::PermissionRule>,
    ) -> crate::policy::PermissionPolicy {
        crate::policy::PermissionPolicy {
            rules,
            ..crate::policy::PermissionPolicy::default()
        }
    }

    fn permission_rule(
        id: &str,
        decision: crate::policy::RuleDecision,
    ) -> crate::policy::PermissionRule {
        crate::policy::PermissionRule {
            id: id.into(),
            decision,
            tools: Vec::new(),
            command_prefix: Vec::new(),
            paths: Vec::new(),
            domains: Vec::new(),
            priority: 0,
        }
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
    async fn deny_rule_blocks_dispatch_before_the_tool_runs() {
        let dir = tempfile::tempdir().unwrap();
        let mut deny = permission_rule("deny-writer", crate::policy::RuleDecision::Deny);
        deny.tools = vec!["writer".into()];
        let kernel = configured_with(
            BTreeSet::new(),
            2,
            dir.path(),
            permission_policy(vec![deny]),
        );
        let (tool, _, _, calls) = probe("writer");
        let wrapped = kernel.wrap(
            tool,
            ToolDescriptor::new("workspace:write")
                .risk(ToolRisk::High)
                .effect(ToolEffect::FileWrite)
                .paths(path_of),
        );
        let ctx = Arc::new(adk_tool::SimpleToolContext::new("test")) as Arc<dyn ToolContext>;
        let result = wrapped
            .execute(ctx, json!({"path": "notes/a.txt"}))
            .await
            .expect("typed denial");
        assert_eq!(result["error"], "permission.denied");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn command_rule_matches_the_observed_command_and_unknown_commands_ask() {
        let dir = tempfile::tempdir().unwrap();
        let mut allow = permission_rule("allow-status", crate::policy::RuleDecision::Allow);
        allow.tools = vec!["exec_command".into()];
        allow.command_prefix = vec!["git status".into()];
        let kernel = configured_with(
            BTreeSet::new(),
            2,
            dir.path(),
            permission_policy(vec![allow]),
        );
        let (tool, _, _, calls) = probe("exec_command");
        let wrapped = kernel.wrap(
            tool,
            ToolDescriptor::new("exec:command")
                .risk(ToolRisk::High)
                .command(|args| {
                    args.get("command")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }),
        );
        let ctx = Arc::new(adk_tool::SimpleToolContext::new("test")) as Arc<dyn ToolContext>;
        wrapped
            .execute(ctx.clone(), json!({"command": "git status --short"}))
            .await
            .expect("allowed call");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        // A different command falls back to the mode's ask. There is no task in
        // this unit test, so the ask must fail closed instead of running.
        let result = wrapped
            .execute(ctx, json!({"command": "git push"}))
            .await
            .expect("typed denial");
        assert_eq!(result["error"], "permission.denied");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn domain_rule_matches_the_observed_host_not_a_claim() {
        let dir = tempfile::tempdir().unwrap();
        let mut allow = permission_rule("allow-api", crate::policy::RuleDecision::Allow);
        allow.domains = vec!["api.example.com".into()];
        let kernel = ToolKernel::new();
        kernel
            .configure(ToolKernelConfig {
                workspace: Some(dir.path().to_path_buf()),
                timeout: Duration::from_secs(5),
                max_output_bytes: 1024,
                max_parallel: 2,
                enabled: BTreeSet::new(),
                permissions: permission_policy(vec![allow]),
                // Permission rules are the subject of this test, so the
                // sandbox must permit the network destination first.
                sandbox: crate::sandbox::SandboxRules {
                    network: crate::sandbox::NetworkRules {
                        enabled: true,
                        ..crate::sandbox::NetworkRules::default()
                    },
                    ..crate::sandbox::SandboxRules::default()
                },
            })
            .expect("kernel configures");
        let (tool, _, _, calls) = probe("web_fetch");
        let wrapped = kernel.wrap(
            tool,
            ToolDescriptor::new("network:read")
                .risk(ToolRisk::Medium)
                .domain(|args| {
                    args.get("url")
                        .and_then(Value::as_str)
                        .and_then(|url| url.split("://").nth(1))
                        .map(|rest| rest.split('/').next().unwrap_or_default().to_string())
                }),
        );
        let ctx = Arc::new(adk_tool::SimpleToolContext::new("test")) as Arc<dyn ToolContext>;
        wrapped
            .execute(ctx.clone(), json!({"url": "https://api.example.com/v1"}))
            .await
            .expect("allowed call");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let result = wrapped
            .execute(ctx, json!({"url": "https://evil.example.com/v1"}))
            .await
            .expect("typed denial");
        assert_eq!(result["error"], "permission.denied");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn sandbox_denial_precedes_dispatch_and_permission_evaluation() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = crate::sandbox::SandboxRules {
            profile: crate::sandbox::SandboxProfile::ReadOnly,
            ..crate::sandbox::SandboxRules::default()
        };
        let kernel = configured_sandbox(dir.path(), sandbox);
        let (tool, _, _, calls) = probe("writer");
        let wrapped = kernel.wrap(
            tool,
            ToolDescriptor::new("workspace:write")
                .risk(ToolRisk::Medium)
                .effect(ToolEffect::FileWrite)
                .paths(path_of),
        );
        let ctx = Arc::new(adk_tool::SimpleToolContext::new("test")) as Arc<dyn ToolContext>;
        // The mode is full-auto, so only the sandbox profile can deny this.
        let result = wrapped
            .execute(ctx, json!({"path": "notes/a.txt"}))
            .await
            .expect("typed denial");
        assert_eq!(result["error"], "sandbox.read_only");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn sandbox_denial_covers_path_escape_before_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = configured_sandbox(dir.path(), crate::sandbox::SandboxRules::default());
        let (tool, _, _, calls) = probe("writer");
        let wrapped = kernel.wrap(tool, ToolDescriptor::new("workspace:write").paths(path_of));
        let ctx = Arc::new(adk_tool::SimpleToolContext::new("test")) as Arc<dyn ToolContext>;
        let result = wrapped
            .execute(ctx, json!({"path": "../host-secret"}))
            .await
            .expect("typed denial");
        assert_eq!(result["error"], "sandbox.path_denied");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn narrowing_the_sandbox_can_never_widen_it() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = configured_sandbox(
            dir.path(),
            crate::sandbox::SandboxRules {
                profile: crate::sandbox::SandboxProfile::ReadOnly,
                ..crate::sandbox::SandboxRules::default()
            },
        );
        let widened = kernel
            .narrow_sandbox(&crate::sandbox::SandboxRules {
                profile: crate::sandbox::SandboxProfile::FullAccess,
                network: crate::sandbox::NetworkRules {
                    enabled: true,
                    allow_hosts: None,
                    proxy: None,
                    proxy_bypass: Vec::new(),
                },
                protected_paths: Vec::new(),
                protected_grants: vec![".git".into()],
                writable_roots: None,
                command_allow: None,
                command_max_bytes: 4096,
                limits: crate::sandbox::SandboxLimits::default(),
            })
            .expect("overlay accepted");
        assert_eq!(widened.profile, crate::sandbox::SandboxProfile::ReadOnly);
        assert!(!widened.network.enabled);
        assert!(widened.protected_grants.is_empty());
        assert_eq!(widened.command_max_bytes, 4096);
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
