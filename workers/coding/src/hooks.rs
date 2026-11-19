//! Worker lifecycle and task hooks for the Coding package.

use crate::package::CodingPackage;
use crate::services::CodingServices;
use common::ApiError;
use runtime::worker::{TaskHookContext, WorkerHooks, WorkerInstance};
use runtime::ExecutionOutcome;
use std::sync::Arc;

/// Compiled package hooks at the real lifecycle and admission points.
pub struct PackageHooks {
    package: Arc<CodingPackage>,
}

impl PackageHooks {
    /// Bind hooks to one package instance.
    pub fn new(package: Arc<CodingPackage>) -> Self {
        Self { package }
    }

    fn services(&self) -> Result<Arc<CodingServices>, ApiError> {
        self.package.current()
    }
}

#[async_trait::async_trait]
impl WorkerHooks for PackageHooks {
    async fn startup(&self, instance: &WorkerInstance) -> Result<(), ApiError> {
        let services = self.services()?;
        services.ensure_worktree()?;
        self.package.attach_runtime(instance.runtime());
        eprintln!(
            "hook:startup package=coding instance={} base={}",
            services.config().instance,
            services.base_commit()
        );
        Ok(())
    }

    async fn before_task(&self, ctx: &TaskHookContext) -> Result<(), ApiError> {
        let services = self.services()?;
        services.acquire_write(&ctx.task_id)?;
        if let Err(error) = services
            .ensure_worktree()
            .and_then(|_| services.begin_task())
        {
            services.release_write(&ctx.task_id);
            return Err(error);
        }
        eprintln!(
            "hook:before_task task={} agent={} worktree={}",
            ctx.task_id,
            ctx.agent,
            services.config().instance
        );
        Ok(())
    }

    async fn after_task(
        &self,
        ctx: &TaskHookContext,
        outcome: &ExecutionOutcome,
    ) -> Result<(), ApiError> {
        let evidence = match self.services() {
            Ok(services) => crate::evidence::collect(&self.package, ctx, outcome)
                .await
                .map(|_| services),
            Err(error) => Err(error),
        };
        let services = match evidence {
            Ok(services) => services,
            Err(error) => {
                if let Ok(services) = self.services() {
                    services.release_write(&ctx.task_id);
                    services.clear_baseline();
                    if outcome.state == Some(common::TaskState::Cancelled) {
                        let _ = crate::evidence::cleanup_cancelled(&services);
                    }
                }
                return Err(error);
            }
        };
        services.release_write(&ctx.task_id);
        services.clear_baseline();
        if outcome.state == Some(common::TaskState::Cancelled) {
            if let Some(problem) = crate::evidence::cleanup_cancelled(&services) {
                return Err(ApiError::new("coding.cleanup_failed", problem));
            }
        }
        Ok(())
    }

    async fn shutdown(&self, _instance: &WorkerInstance) -> Result<(), ApiError> {
        if let Ok(services) = self.services() {
            services.shutdown();
        }
        eprintln!("hook:shutdown package=coding");
        Ok(())
    }
}
