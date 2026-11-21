//! Worker hooks and their lifecycle, tool, prompt, compaction, session, and
//! subagent contexts.
//!
//! Two distinct categories remain separate: compiled package hooks
//! ([`WorkerHooks`]) and native ADK callbacks/plugins. Hooks observe stable
//! runtime events in registration order. Every handler is bounded by one
//! runtime-selected deadline; a failing or timed-out handler is reported as a
//! typed `hook.failed` or `hook.timeout` event and cannot grant authority.

use crate::worker::WorkerInstance;
use crate::worker::WorkerResult;
use crate::ExecutionOutcome;
use async_trait::async_trait;
use futures::future::BoxFuture;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Context for one admitted task at a Worker hook point.
#[derive(Debug, Clone)]
pub struct TaskHookContext {
    /// Durable external task handle this hook point belongs to.
    pub task_id: String,
    /// Owning namespace.
    pub namespace: String,
    /// Authenticated principal.
    pub caller: String,
    /// Frozen agent id.
    pub agent: String,
    /// Session id.
    pub session_id: String,
    /// Frozen profile name.
    pub profile: String,
    /// Frozen profile revision.
    pub profile_revision: u64,
    /// Frozen resolved asset revision.
    pub asset_revision: u64,
    /// Original task input.
    pub input: String,
    /// Effective package settings.
    pub settings: BTreeMap<String, String>,
    /// Cooperative cancellation for this invocation.
    pub cancel: CancellationToken,
}

/// Context for a pre/post tool hook.
///
/// These fields are runtime-observed. Arguments are represented by their
/// canonical digest; raw values are never forwarded to an external hook.
#[derive(Debug, Clone)]
pub struct ToolHookContext {
    /// Durable task handle, absent outside an admitted task.
    pub task_id: Option<String>,
    /// Owning namespace.
    pub namespace: String,
    /// Authenticated principal.
    pub caller: String,
    /// Agent that requested the call.
    pub agent: String,
    /// Session id.
    pub session_id: String,
    /// Native invocation id.
    pub invocation: String,
    /// Native function call id.
    pub call_id: String,
    /// Wrapped tool name.
    pub tool: String,
    /// Capability declared by the descriptor.
    pub capability: String,
    /// Risk classification.
    pub risk: String,
    /// Known effect class.
    pub effect: String,
    /// Canonical digest of the exact call arguments.
    pub arguments_digest: String,
    /// Workspace-relative paths the call touches.
    pub paths: Vec<String>,
    /// Redacted command text, when the tool executes one.
    pub command: Option<String>,
    /// Network host, when the tool reaches one.
    pub domain: Option<String>,
    /// Sandbox profile in force.
    pub sandbox: String,
    /// Authorization decision already reached by the policy engine.
    pub permission: String,
    /// Frozen profile name.
    pub profile: String,
    /// Frozen profile revision.
    pub profile_revision: u64,
}

/// Observed result of a tool call, for post-tool hooks.
#[derive(Debug, Clone)]
pub struct ToolHookOutcome {
    /// Whether the tool returned without an error code.
    pub ok: bool,
    /// Stable error code, when the call failed.
    pub error_code: Option<String>,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Canonical digest of the bounded result.
    pub result_digest: String,
    /// Serialized result size after bounding.
    pub output_bytes: usize,
    /// Whether the result was truncated at the runtime ceiling.
    pub truncated: bool,
}

/// Context for a terminal-turn (`stop`) hook.
#[derive(Debug, Clone)]
pub struct StopHookContext {
    /// Task identity and frozen execution context.
    pub task: TaskHookContext,
    /// Committed terminal state.
    pub state: String,
    /// Stable error code, when the attempt failed.
    pub error_code: Option<String>,
    /// Truthful uncertainty, when the outcome is not definite.
    pub uncertainty: Option<String>,
}

/// Context for a subagent-stop hook.
#[derive(Debug, Clone)]
pub struct SubagentStopHookContext {
    /// Parent task that owns the child.
    pub parent_task_id: String,
    /// Completed child task.
    pub child_task_id: String,
    /// Owning namespace.
    pub namespace: String,
    /// Authenticated principal.
    pub caller: String,
    /// Child agent id.
    pub agent: String,
    /// Child session id.
    pub session_id: String,
    /// Committed child terminal state.
    pub state: String,
}

/// Context for a pre/post compaction hook.
#[derive(Debug, Clone)]
pub struct CompactHookContext {
    /// Task identity and frozen execution context.
    pub task: TaskHookContext,
    /// Native compaction strategy.
    pub strategy: String,
    /// Estimated token budget that triggered compaction.
    pub context_budget: u64,
    /// Number of most-recent native events preserved verbatim.
    pub preserve_recent: usize,
    /// One-based attempt number.
    pub attempt: usize,
    /// Native event count before compaction.
    pub events_before: usize,
}

/// Observed outcome of native compaction.
#[derive(Debug, Clone, Copy)]
pub struct CompactHookOutcome {
    /// Native event count after compaction.
    pub events_after: usize,
}

/// Context for a session start/end hook.
///
/// Session lifetime is owned by the canonical native session store. A session
/// starts when the first admitted turn opens it in this process and ends when
/// the Worker stops or the identity is removed.
#[derive(Debug, Clone)]
pub struct SessionHookContext {
    /// Task that opened the session, absent for a shutdown-time end hook.
    pub task_id: Option<String>,
    /// Owning namespace.
    pub namespace: String,
    /// Authenticated principal.
    pub caller: String,
    /// Agent bound to the turn that opened the session.
    pub agent: String,
    /// Session id.
    pub session_id: String,
    /// Frozen profile name.
    pub profile: String,
    /// Frozen profile revision.
    pub profile_revision: u64,
    /// Frozen resolved asset revision.
    pub asset_revision: u64,
}

/// A hook that may only continue or deny. It can never grant authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookDecision {
    /// Continue with the decision the runtime already reached.
    Continue,
    /// Restrict the action further.
    Deny {
        /// Redacted, operator-facing reason.
        reason: String,
    },
}

impl HookDecision {
    /// A denial carrying one redacted reason.
    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
        }
    }

    /// Whether the hook denied the action.
    pub fn is_denied(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }

    /// Redacted denial reason.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Continue => None,
            Self::Deny { reason } => Some(reason),
        }
    }
}

/// Runtime limits applied to each hook handler.
#[derive(Debug, Clone, Copy)]
pub struct HookPolicy {
    /// Maximum time one handler may take.
    pub timeout: Duration,
    /// Maximum bytes retained from one external hook's output.
    pub max_output_bytes: usize,
}

impl Default for HookPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(5_000),
            max_output_bytes: 65_536,
        }
    }
}

/// Typed hook failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookFailureKind {
    /// The handler returned an error.
    Failed,
    /// The handler exceeded its deadline.
    Timeout,
}

impl HookFailureKind {
    /// Stable event kind.
    pub fn event(self) -> &'static str {
        match self {
            Self::Failed => "hook.failed",
            Self::Timeout => "hook.timeout",
        }
    }

    /// Stable wire label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Timeout => "timeout",
        }
    }
}

/// One typed hook failure, consumed by the runtime evidence sink.
#[derive(Debug, Clone)]
pub struct HookFailure {
    /// Stable handler label.
    pub hook: String,
    /// Hook event name, for example `pre_tool`.
    pub event: &'static str,
    /// Failure class.
    pub kind: HookFailureKind,
    /// Task the failure belongs to, when task-scoped.
    pub task_id: Option<String>,
    /// Redacted failure message.
    pub message: String,
    /// Deadline applied to the handler.
    pub timeout_ms: u64,
}

#[async_trait]
pub trait HookReporter: Send + Sync {
    /// Record one handler failure without altering the committed outcome.
    async fn report(&self, failure: HookFailure);
}

/// Compiled package hooks connected to real runtime lifecycle and admission
/// points.
///
/// A hook may observe and restrict. It is called after admission, permission,
/// sandbox, and approval decisions, and cannot grant a capability, widen a
/// profile, or rewrite a committed effect.
#[async_trait]
pub trait WorkerHooks: Send + Sync {
    /// Stable label used in failure evidence.
    fn label(&self) -> &str {
        "worker-hooks"
    }

    /// Called once after configuration, assets, storage, and native execution
    /// are ready, before any listener accepts traffic.
    async fn startup(&self, _instance: &WorkerInstance) -> WorkerResult<()> {
        Ok(())
    }

    /// Called after durable admission and before native execution. An error
    /// rejects execution with a stable, redacted error.
    async fn before_task(&self, _ctx: &TaskHookContext) -> WorkerResult<()> {
        Ok(())
    }

    /// Called when a caller submits a prompt, before `before_task`. A denial
    /// rejects the task; a handler failure or timeout is reported and ignored.
    async fn prompt_submit(&self, _ctx: &TaskHookContext) -> WorkerResult<HookDecision> {
        Ok(HookDecision::Continue)
    }

    /// Called after authorization and sandbox decisions, immediately before a
    /// tool is dispatched. A denial blocks the call; it can never allow one
    /// the policy already denied.
    async fn pre_tool(&self, _ctx: &ToolHookContext) -> WorkerResult<HookDecision> {
        Ok(HookDecision::Continue)
    }

    /// Called after a tool result is bounded. A handler cannot change the
    /// result, the committed effect, or the authorization decision.
    async fn post_tool(
        &self,
        _ctx: &ToolHookContext,
        _outcome: &ToolHookOutcome,
    ) -> WorkerResult<()> {
        Ok(())
    }

    /// Called when a turn reaches a committed terminal outcome.
    async fn stop(&self, _ctx: &StopHookContext) -> WorkerResult<()> {
        Ok(())
    }

    /// Called when a runtime-local child subagent reaches a terminal outcome.
    async fn subagent_stop(&self, _ctx: &SubagentStopHookContext) -> WorkerResult<()> {
        Ok(())
    }

    /// Called before native context compaction rewrites the request history.
    async fn pre_compact(&self, _ctx: &CompactHookContext) -> WorkerResult<()> {
        Ok(())
    }

    /// Called after native context compaction produced the next event list.
    async fn post_compact(
        &self,
        _ctx: &CompactHookContext,
        _outcome: &CompactHookOutcome,
    ) -> WorkerResult<()> {
        Ok(())
    }

    /// Called once when a session identity is first opened in this process.
    async fn session_start(&self, _ctx: &SessionHookContext) -> WorkerResult<()> {
        Ok(())
    }

    /// Called when a started session ends, before package shutdown hooks run.
    async fn session_end(&self, _ctx: &SessionHookContext) -> WorkerResult<()> {
        Ok(())
    }

    /// Called after the native outcome is committed. An error is reported as
    /// redacted evidence and cannot change the committed effect.
    async fn after_task(
        &self,
        _ctx: &TaskHookContext,
        _outcome: &ExecutionOutcome,
    ) -> WorkerResult<()> {
        Ok(())
    }

    /// Called after admission stops and admitted work drains, before native
    /// plugins close.
    async fn shutdown(&self, _instance: &WorkerInstance) -> WorkerResult<()> {
        Ok(())
    }
}

/// Ordered compilation of the hooks a package registered separately.
pub(super) struct CompositeHooks {
    hooks: Vec<Arc<dyn WorkerHooks>>,
    policy: HookPolicy,
    reporter: Option<Arc<dyn HookReporter>>,
}

impl CompositeHooks {
    pub(super) fn new(
        hooks: Vec<Arc<dyn WorkerHooks>>,
        policy: HookPolicy,
        reporter: Option<Arc<dyn HookReporter>>,
    ) -> Self {
        Self {
            hooks,
            policy,
            reporter,
        }
    }

    /// Run one handler under the runtime deadline and record a typed failure.
    async fn invoke<T, F>(
        &self,
        event: &'static str,
        task_id: Option<&str>,
        hook: &Arc<dyn WorkerHooks>,
        call: F,
    ) -> WorkerResult<T>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a dyn WorkerHooks) -> BoxFuture<'a, WorkerResult<T>>,
    {
        match tokio::time::timeout(self.policy.timeout, call(hook.as_ref())).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => {
                let message = common::redact(&error.message);
                self.report(HookFailure {
                    hook: hook.label().to_string(),
                    event,
                    kind: HookFailureKind::Failed,
                    task_id: task_id.map(str::to_string),
                    message: format!("{}: {}", error.code, message),
                    timeout_ms: self.policy.timeout.as_millis() as u64,
                })
                .await;
                Err(error)
            }
            Err(_) => {
                let timeout_ms = self.policy.timeout.as_millis() as u64;
                self.report(HookFailure {
                    hook: hook.label().to_string(),
                    event,
                    kind: HookFailureKind::Timeout,
                    task_id: task_id.map(str::to_string),
                    message: format!("hook exceeded {timeout_ms} ms"),
                    timeout_ms,
                })
                .await;
                Err(common::ApiError::new(
                    "hook.timeout",
                    format!("hook '{event}' exceeded {timeout_ms} ms"),
                ))
            }
        }
    }

    async fn report(&self, failure: HookFailure) {
        if let Some(reporter) = &self.reporter {
            reporter.report(failure).await;
        }
    }

    /// Run a restrictive decision hook. Failure or timeout continues with the
    /// decision the runtime already reached; it never grants.
    async fn decide<F>(&self, event: &'static str, task_id: Option<&str>, call: F) -> HookDecision
    where
        F: for<'a> Fn(&'a dyn WorkerHooks) -> BoxFuture<'a, WorkerResult<HookDecision>>,
    {
        for hook in &self.hooks {
            match self.invoke(event, task_id, hook, |inner| call(inner)).await {
                Ok(HookDecision::Continue) => {}
                Ok(denied @ HookDecision::Deny { .. }) => return denied,
                Err(_) => {}
            }
        }
        HookDecision::Continue
    }
}

#[async_trait]
impl WorkerHooks for CompositeHooks {
    async fn startup(&self, instance: &WorkerInstance) -> WorkerResult<()> {
        let instance = instance.clone();
        for hook in &self.hooks {
            let owned = instance.clone();
            self.invoke("startup", None, hook, move |inner| {
                Box::pin(async move { inner.startup(&owned).await })
            })
            .await?;
        }
        Ok(())
    }

    async fn before_task(&self, ctx: &TaskHookContext) -> WorkerResult<()> {
        let owned = ctx.clone();
        for hook in &self.hooks {
            let owned = owned.clone();
            self.invoke("before_task", Some(&ctx.task_id), hook, move |inner| {
                Box::pin(async move { inner.before_task(&owned).await })
            })
            .await?;
        }
        Ok(())
    }

    async fn prompt_submit(&self, ctx: &TaskHookContext) -> WorkerResult<HookDecision> {
        let owned = ctx.clone();
        Ok(self
            .decide("prompt_submit", Some(&ctx.task_id), move |inner| {
                let owned = owned.clone();
                Box::pin(async move { inner.prompt_submit(&owned).await })
            })
            .await)
    }

    async fn pre_tool(&self, ctx: &ToolHookContext) -> WorkerResult<HookDecision> {
        let owned = ctx.clone();
        Ok(self
            .decide("pre_tool", ctx.task_id.as_deref(), move |inner| {
                let owned = owned.clone();
                Box::pin(async move { inner.pre_tool(&owned).await })
            })
            .await)
    }

    async fn post_tool(
        &self,
        ctx: &ToolHookContext,
        outcome: &ToolHookOutcome,
    ) -> WorkerResult<()> {
        let owned = ctx.clone();
        let observed = outcome.clone();
        for hook in &self.hooks {
            let owned = owned.clone();
            let observed = observed.clone();
            let _ = self
                .invoke("post_tool", ctx.task_id.as_deref(), hook, move |inner| {
                    Box::pin(async move { inner.post_tool(&owned, &observed).await })
                })
                .await;
        }
        Ok(())
    }

    async fn stop(&self, ctx: &StopHookContext) -> WorkerResult<()> {
        let owned = ctx.clone();
        for hook in &self.hooks {
            let owned = owned.clone();
            let _ = self
                .invoke("stop", Some(&ctx.task.task_id), hook, move |inner| {
                    Box::pin(async move { inner.stop(&owned).await })
                })
                .await;
        }
        Ok(())
    }

    async fn subagent_stop(&self, ctx: &SubagentStopHookContext) -> WorkerResult<()> {
        let owned = ctx.clone();
        for hook in &self.hooks {
            let owned = owned.clone();
            let _ = self
                .invoke(
                    "subagent_stop",
                    Some(&ctx.parent_task_id),
                    hook,
                    move |inner| Box::pin(async move { inner.subagent_stop(&owned).await }),
                )
                .await;
        }
        Ok(())
    }

    async fn pre_compact(&self, ctx: &CompactHookContext) -> WorkerResult<()> {
        let owned = ctx.clone();
        for hook in &self.hooks {
            let owned = owned.clone();
            let _ = self
                .invoke("pre_compact", Some(&ctx.task.task_id), hook, move |inner| {
                    Box::pin(async move { inner.pre_compact(&owned).await })
                })
                .await;
        }
        Ok(())
    }

    async fn post_compact(
        &self,
        ctx: &CompactHookContext,
        outcome: &CompactHookOutcome,
    ) -> WorkerResult<()> {
        let owned = ctx.clone();
        let observed = *outcome;
        for hook in &self.hooks {
            let owned = owned.clone();
            let _ = self
                .invoke(
                    "post_compact",
                    Some(&ctx.task.task_id),
                    hook,
                    move |inner| {
                        Box::pin(async move { inner.post_compact(&owned, &observed).await })
                    },
                )
                .await;
        }
        Ok(())
    }

    async fn session_start(&self, ctx: &SessionHookContext) -> WorkerResult<()> {
        let owned = ctx.clone();
        for hook in &self.hooks {
            let owned = owned.clone();
            let _ = self
                .invoke(
                    "session_start",
                    ctx.task_id.as_deref(),
                    hook,
                    move |inner| Box::pin(async move { inner.session_start(&owned).await }),
                )
                .await;
        }
        Ok(())
    }

    async fn session_end(&self, ctx: &SessionHookContext) -> WorkerResult<()> {
        let owned = ctx.clone();
        for hook in &self.hooks {
            let owned = owned.clone();
            let _ = self
                .invoke("session_end", ctx.task_id.as_deref(), hook, move |inner| {
                    Box::pin(async move { inner.session_end(&owned).await })
                })
                .await;
        }
        Ok(())
    }

    async fn after_task(
        &self,
        ctx: &TaskHookContext,
        outcome: &ExecutionOutcome,
    ) -> WorkerResult<()> {
        let owned = ctx.clone();
        let observed = outcome.clone();
        for hook in &self.hooks {
            let owned = owned.clone();
            let observed = observed.clone();
            self.invoke("after_task", Some(&ctx.task_id), hook, move |inner| {
                Box::pin(async move { inner.after_task(&owned, &observed).await })
            })
            .await?;
        }
        Ok(())
    }

    async fn shutdown(&self, instance: &WorkerInstance) -> WorkerResult<()> {
        let instance = instance.clone();
        for hook in &self.hooks {
            let owned = instance.clone();
            self.invoke("shutdown", None, hook, move |inner| {
                Box::pin(async move { inner.shutdown(&owned).await })
            })
            .await?;
        }
        Ok(())
    }
}
