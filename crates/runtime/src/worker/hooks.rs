//! Worker hooks and their lifecycle contexts.

use crate::worker::WorkerInstance;
use crate::worker::WorkerResult;
use crate::ExecutionOutcome;
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::Arc;
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

/// Compiled package hooks connected to real runtime lifecycle and admission
/// points.
#[async_trait]
pub trait WorkerHooks: Send + Sync {
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
pub(super) struct CompositeHooks(pub(super) Vec<Arc<dyn WorkerHooks>>);

#[async_trait]
impl WorkerHooks for CompositeHooks {
    async fn startup(&self, instance: &WorkerInstance) -> WorkerResult<()> {
        for hook in &self.0 {
            hook.startup(instance).await?;
        }
        Ok(())
    }

    async fn before_task(&self, ctx: &TaskHookContext) -> WorkerResult<()> {
        for hook in &self.0 {
            hook.before_task(ctx).await?;
        }
        Ok(())
    }

    async fn after_task(
        &self,
        ctx: &TaskHookContext,
        outcome: &ExecutionOutcome,
    ) -> WorkerResult<()> {
        for hook in &self.0 {
            hook.after_task(ctx, outcome).await?;
        }
        Ok(())
    }

    async fn shutdown(&self, instance: &WorkerInstance) -> WorkerResult<()> {
        for hook in &self.0 {
            hook.shutdown(instance).await?;
        }
        Ok(())
    }
}
