//! Runtime-side hook dispatch and typed hook-failure evidence.
//!
//! Hooks run after admission, authorization, sandbox, and approval decisions.
//! This module only routes events and records failures; it never grants an
//! authority the runtime already denied.

use crate::worker::{
    CompactHookContext, CompactHookOutcome, HookDecision, HookFailure, HookReporter,
    SessionHookContext, StopHookContext, SubagentStopHookContext, TaskHookContext, ToolHookContext,
    ToolHookOutcome,
};
use crate::Runtime;
use async_trait::async_trait;

impl Runtime {
    /// Call `prompt_submit` hooks. Failure and timeout are reported and
    /// ignored; an explicit denial blocks the task.
    pub(crate) async fn prompt_submit_hook(&self, ctx: &TaskHookContext) -> HookDecision {
        match self.hooks().await {
            Some(hooks) => hooks
                .prompt_submit(ctx)
                .await
                .unwrap_or(HookDecision::Continue),
            None => HookDecision::Continue,
        }
    }

    /// Call `pre_tool` hooks after the policy engine allowed the call. The
    /// result can only restrict.
    pub(crate) async fn pre_tool_hook(&self, ctx: &ToolHookContext) -> HookDecision {
        match self.hooks().await {
            Some(hooks) => hooks.pre_tool(ctx).await.unwrap_or(HookDecision::Continue),
            None => HookDecision::Continue,
        }
    }

    /// Call `post_tool` hooks. The result is observation only.
    pub(crate) async fn post_tool_hook(&self, ctx: &ToolHookContext, outcome: &ToolHookOutcome) {
        if let Some(hooks) = self.hooks().await {
            let _ = hooks.post_tool(ctx, outcome).await;
        }
    }

    /// Call `stop` hooks after a terminal outcome is committed.
    pub(crate) async fn stop_hook(&self, ctx: &StopHookContext) {
        if let Some(hooks) = self.hooks().await {
            let _ = hooks.stop(ctx).await;
        }
    }

    /// Call `subagent_stop` hooks for one completed runtime-local child.
    ///
    /// Child admission is owned by the subagent runtime; this method is the
    /// single event entry point so a child cannot bypass ordering or evidence.
    pub async fn notify_subagent_stop(
        &self,
        parent_task_id: &str,
        child_task_id: &str,
        agent: &str,
        session_id: &str,
        state: &str,
    ) -> std::result::Result<(), common::ApiError> {
        let parent = self.task_unscoped(parent_task_id).await.ok_or_else(|| {
            common::ApiError::new(
                "task.not_found",
                "subagent-stop hook parent task does not exist",
            )
        })?;
        let _ = self
            .append_event(
                parent_task_id,
                "subagent.stop",
                serde_json::json!({
                    "child_task_id": child_task_id,
                    "agent": agent,
                    "session_id": session_id,
                    "state": state,
                }),
                true,
            )
            .await;
        if let Some(hooks) = self.hooks().await {
            let _ = hooks
                .subagent_stop(&SubagentStopHookContext {
                    parent_task_id: parent.id.clone(),
                    child_task_id: child_task_id.to_string(),
                    namespace: parent.namespace.clone(),
                    caller: parent.caller.clone(),
                    agent: agent.to_string(),
                    session_id: session_id.to_string(),
                    state: state.to_string(),
                })
                .await;
        }
        Ok(())
    }

    /// Call `pre_compact` hooks before native history is rewritten.
    pub(crate) async fn pre_compact_hook(&self, ctx: &CompactHookContext) {
        if let Some(hooks) = self.hooks().await {
            let _ = hooks.pre_compact(ctx).await;
        }
    }

    /// Call `post_compact` hooks after native history was rewritten.
    pub(crate) async fn post_compact_hook(
        &self,
        ctx: &CompactHookContext,
        outcome: &CompactHookOutcome,
    ) {
        if let Some(hooks) = self.hooks().await {
            let _ = hooks.post_compact(ctx, outcome).await;
        }
    }

    /// Call `session_start` hooks once per canonical session identity.
    pub(crate) async fn session_start_hook(&self, ctx: &SessionHookContext) {
        let Some(hooks) = self.hooks().await else {
            return;
        };
        let key = session_key(ctx);
        if self.track_session(key, ctx.clone()).await {
            let _ = hooks.session_start(ctx).await;
        }
    }

    /// Call `session_end` hooks for every session started in this process.
    pub(crate) async fn session_end_hooks(&self) {
        let sessions = self.drain_sessions().await;
        let Some(hooks) = self.hooks().await else {
            return;
        };
        for ctx in sessions {
            let _ = hooks.session_end(&ctx).await;
        }
    }
}

fn session_key(ctx: &SessionHookContext) -> String {
    format!(
        "{}\u{0}{}\u{0}{}",
        ctx.namespace, ctx.caller, ctx.session_id
    )
}

#[async_trait]
impl HookReporter for Runtime {
    async fn report(&self, failure: HookFailure) {
        if let Some(task_id) = &failure.task_id {
            let task = self.task_unscoped(task_id).await;
            let _ = self
                .append_event(
                    task_id,
                    failure.kind.event(),
                    serde_json::json!({
                        "hook": failure.hook,
                        "event": failure.event,
                        "kind": failure.kind.as_str(),
                        "message": failure.message,
                        "timeout_ms": failure.timeout_ms,
                    }),
                    true,
                )
                .await;
            if let Some(task) = task {
                let _ = self
                    .trace(
                        &task.trace_id,
                        task_id,
                        failure.kind.event(),
                        failure.timeout_ms,
                        std::collections::BTreeMap::from([
                            ("hook".into(), failure.hook.clone()),
                            ("event".into(), failure.event.to_string()),
                            ("kind".into(), failure.kind.as_str().to_string()),
                        ]),
                    )
                    .await;
                let detail = format!(
                    "hook={} event={} kind={} timeout_ms={} message={}",
                    failure.hook,
                    failure.event,
                    failure.kind.as_str(),
                    failure.timeout_ms,
                    common::redact(&failure.message),
                );
                let _ = self
                    .audit_with_trace(
                        &task.caller,
                        failure.kind.event(),
                        task_id,
                        "failed",
                        &detail,
                        &task.trace_id,
                    )
                    .await;
            }
            return;
        }
        // Lifecycle and session-end hooks have no task stream. Their failures
        // remain visible in the durable audit and the health projection.
        let detail = format!(
            "hook={} event={} kind={} timeout_ms={} message={}",
            failure.hook,
            failure.event,
            failure.kind.as_str(),
            failure.timeout_ms,
            common::redact(&failure.message),
        );
        let _ = self
            .audit(
                "runtime",
                failure.kind.event(),
                &failure.hook,
                "failed",
                &detail,
            )
            .await;
        self.record_degraded(failure.kind.event()).await;
    }
}
