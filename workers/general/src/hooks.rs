//! Native ADK plugin and Worker lifecycle hooks for the General package.

use crate::callbacks::{append_text, text_of};
use adk_rust::plugin::{Plugin, PluginConfig};
use adk_rust::prelude::Content;
use common::ApiError;
use runtime::worker::{TaskHookContext, WorkerHooks, WorkerInstance};
use runtime::ExecutionOutcome;

/// One run-scope native ADK plugin. Only hooks reached by the pinned Runner
/// dispatch are used: `before_run`, `on_user_message`, `on_event`, `after_run`.
pub fn audit_plugin() -> Plugin {
    Plugin::new(PluginConfig {
        name: "general-audit".into(),
        before_run: Some(Box::new(|ctx| {
            Box::pin(async move {
                if text_of(ctx.user_content()).contains("plugin-short") {
                    return Ok(Some(
                        Content::new("model").with_text("plugin-short-circuit"),
                    ));
                }
                Ok(None)
            })
        })),
        on_user_message: Some(Box::new(|_ctx, mut content| {
            Box::pin(async move {
                if text_of(&content).contains("plugin:")
                    && !text_of(&content).contains("[plugin-user]")
                {
                    append_text(&mut content, " [plugin-user]");
                    return Ok(Some(content));
                }
                Ok(None)
            })
        })),
        on_event: Some(Box::new(|_ctx, mut event| {
            Box::pin(async move {
                if let Some(content) = event.llm_response.content.as_mut() {
                    if event.llm_response.turn_complete
                        && !event.llm_response.partial
                        && text_of(content).contains("[plugin-user]")
                        && !text_of(content).contains("[plugin-event]")
                    {
                        append_text(content, " [plugin-event]");
                        return Ok(Some(event));
                    }
                }
                Ok(None)
            })
        })),
        after_run: Some(Box::new(|_ctx| Box::pin(async {}))),
        close_fn: Some(Box::new(|| {
            Box::pin(async move {
                eprintln!("plugin:closed");
            })
        })),
        ..Default::default()
    })
}

/// Compiled package hooks at real lifecycle and admission points.
pub struct PackageHooks;

#[async_trait::async_trait]
impl WorkerHooks for PackageHooks {
    async fn startup(&self, instance: &WorkerInstance) -> Result<(), ApiError> {
        eprintln!(
            "hook:startup package={} instance={} revision={}",
            instance.name(),
            instance.runtime().config().instance_id,
            instance.asset_revision()
        );
        let settings = instance.runtime().settings().await;
        if let Some(delay) = settings
            .get("startup_delay_ms")
            .and_then(|value| value.parse::<u64>().ok())
        {
            tokio::time::sleep(std::time::Duration::from_millis(delay.min(30_000))).await;
        }
        if settings
            .get("startup_fail")
            .is_some_and(|value| value == "true")
        {
            return Err(ApiError::new(
                "policy.startup_failed",
                "startup hook rejected this configuration; canary sk-startup-secret",
            ));
        }
        Ok(())
    }

    async fn before_task(&self, ctx: &TaskHookContext) -> Result<(), ApiError> {
        eprintln!(
            "hook:before_task agent={} profile_revision={} asset_revision={}",
            ctx.agent, ctx.profile_revision, ctx.asset_revision
        );
        if let Some(needle) = ctx.settings.get("reject_contains") {
            if !needle.is_empty() && ctx.input.contains(needle) {
                return Err(ApiError::new(
                    "policy.before_task_rejected",
                    "before-task policy rejected input; canary sk-before-task-secret",
                ));
            }
        }
        Ok(())
    }

    async fn after_task(
        &self,
        ctx: &TaskHookContext,
        outcome: &ExecutionOutcome,
    ) -> Result<(), ApiError> {
        eprintln!(
            "hook:after_task agent={} state={:?}",
            ctx.agent, outcome.state
        );
        if ctx
            .settings
            .get("after_task_fail")
            .is_some_and(|value| value == "true")
        {
            return Err(ApiError::new(
                "policy.after_task_failed",
                "after-task evidence failure; canary sk-after-task-secret",
            ));
        }
        Ok(())
    }

    async fn shutdown(&self, instance: &WorkerInstance) -> Result<(), ApiError> {
        eprintln!("hook:shutdown");
        let settings = instance.runtime().settings().await;
        if let Some(path) = settings.get("shutdown_marker") {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(file, "shutdown");
            }
        }
        if let Some(delay) = settings
            .get("shutdown_delay_ms")
            .and_then(|value| value.parse::<u64>().ok())
        {
            tokio::time::sleep(std::time::Duration::from_millis(delay.min(30_000))).await;
        }
        if settings
            .get("shutdown_fail")
            .is_some_and(|value| value == "true")
        {
            return Err(ApiError::new(
                "policy.shutdown_failed",
                "shutdown hook failed; canary sk-shutdown-secret",
            ));
        }
        Ok(())
    }
}
