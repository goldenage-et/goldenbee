//! Package lifecycle hooks for the Knowledge Worker.
//!
//! Startup validates operator source roots and opens the durable job store
//! before the instance becomes ready. After-task hooks attach durable audit
//! provenance to the exact task through the shared `task_id` context. Shutdown
//! drops the derived native index.

use crate::service::{KnowledgeCell, KnowledgeService};
use async_trait::async_trait;
use common::ApiError;
use runtime::worker::{TaskHookContext, WorkerHooks, WorkerInstance};
use runtime::ExecutionOutcome;

/// Compiled package hooks at real lifecycle and admission points.
pub struct PackageHooks {
    cell: KnowledgeCell,
    runtime: std::sync::Mutex<Option<runtime::Runtime>>,
}

impl PackageHooks {
    /// Bind hooks to the shared package service cell.
    pub fn new(cell: KnowledgeCell) -> Self {
        Self {
            cell,
            runtime: std::sync::Mutex::new(None),
        }
    }
}

/// Package-owned lifecycle owner for the durable source catalogue, source
/// bytes, derived vector chunks, and citation jobs.
pub struct KnowledgeLifecycle {
    cell: KnowledgeCell,
}

impl KnowledgeLifecycle {
    /// Bind lifecycle calls to the same service cell used by the tools.
    pub fn new(cell: KnowledgeCell) -> Self {
        Self { cell }
    }
}

#[async_trait]
impl runtime::LifecycleOwner for KnowledgeLifecycle {
    async fn erase_principal(&self, namespace: &str, principal: &str) -> Result<usize, ApiError> {
        let mut guard = self.cell.lock().await;
        let service = guard.as_mut().ok_or_else(|| {
            ApiError::new(
                "knowledge.not_ready",
                "the knowledge source store is not installed in this Worker",
            )
        })?;
        service
            .erase_principal(namespace, principal)
            .await
            .map_err(|error| error.to_api_error())
    }

    async fn retention(&self, cutoff_ms: i64) -> Result<usize, ApiError> {
        let mut guard = self.cell.lock().await;
        let service = guard.as_mut().ok_or_else(|| {
            ApiError::new(
                "knowledge.not_ready",
                "the knowledge source store is not installed in this Worker",
            )
        })?;
        service
            .retention(cutoff_ms)
            .await
            .map_err(|error| error.to_api_error())
    }
}

#[async_trait]
impl WorkerHooks for PackageHooks {
    async fn startup(&self, instance: &WorkerInstance) -> Result<(), ApiError> {
        let context = instance.context();
        let settings = context.settings().await;
        let config_dir = instance
            .config_path()
            .parent()
            .ok_or_else(|| {
                ApiError::new(
                    "knowledge.config_invalid",
                    "configuration file has no parent directory",
                )
            })?
            .to_path_buf();
        let data_dir = context.data_dir().await;
        let namespace = instance.namespace().await;
        let memory = instance.runtime().memory().await;
        let service = KnowledgeService::open(&config_dir, &data_dir, &namespace, &settings, memory)
            .map_err(|error| error.to_api_error())?;
        let roots = service.config.source_roots.len();
        self.cell.install(service).await;
        if let Ok(mut guard) = self.runtime.lock() {
            *guard = Some(instance.runtime());
        }
        let _ = instance
            .runtime()
            .audit(
                "package",
                "knowledge.startup",
                instance.runtime().config().instance_id.as_str(),
                "ready",
                &format!("source_roots={roots}"),
            )
            .await;
        eprintln!(
            "hook:startup package={} instance={} source_roots={roots}",
            instance.name(),
            instance.runtime().config().instance_id
        );
        Ok(())
    }

    async fn after_task(
        &self,
        ctx: &TaskHookContext,
        outcome: &ExecutionOutcome,
    ) -> Result<(), ApiError> {
        eprintln!(
            "hook:after_task package=knowledge task={} agent={} state={:?}",
            ctx.task_id, ctx.agent, outcome.state
        );
        let runtime = self.runtime.lock().ok().and_then(|guard| guard.clone());
        if let Some(runtime) = runtime {
            runtime
                .audit(
                    &ctx.caller,
                    "knowledge.task",
                    &ctx.task_id,
                    "completed",
                    &format!("agent={} state={:?}", ctx.agent, outcome.state),
                )
                .await?;
        }
        Ok(())
    }

    async fn shutdown(&self, instance: &WorkerInstance) -> Result<(), ApiError> {
        self.cell.clear().await;
        if let Ok(mut guard) = self.runtime.lock() {
            *guard = None;
        }
        let _ = instance
            .runtime()
            .audit(
                "package",
                "knowledge.shutdown",
                instance.runtime().config().instance_id.as_str(),
                "stopped",
                "",
            )
            .await;
        eprintln!("hook:shutdown package=knowledge");
        Ok(())
    }
}
