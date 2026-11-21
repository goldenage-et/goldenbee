//! The Coding package's single Worker definition and instance services.

use crate::agent;
use crate::callbacks::audit_plugin;
use crate::hooks::PackageHooks;
use crate::services::CodingServices;
use crate::tool::CodingCommandTool;
use adk_rust::Tool;
use runtime::native::PackageManifest;
use runtime::worker::{Worker, WorkerSettings};
use runtime::Runtime;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Native plugin and hook counters. The plugin observes the real Runner tool
/// dispatch; hooks record what actually executed.
#[derive(Default)]
pub struct ToolCounters {
    calls: AtomicUsize,
    names: Mutex<Vec<String>>,
}

impl ToolCounters {
    /// Record one native tool invocation.
    pub fn record(&self, name: &str) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut names) = self.names.lock() {
            if names.len() < 64 {
                names.push(name.to_string());
            }
        }
    }

    /// Total observed tool calls.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Observed tool names in dispatch order, bounded.
    pub fn names(&self) -> Vec<String> {
        self.names
            .lock()
            .map(|names| names.clone())
            .unwrap_or_default()
    }
}

/// Package-owned state shared by factories, tools, and hooks.
pub struct CodingPackage {
    services: Mutex<Option<Arc<CodingServices>>>,
    runtime: Mutex<Option<Runtime>>,
    counters: Arc<ToolCounters>,
}

impl CodingPackage {
    /// Empty package state for one Worker definition.
    pub fn new() -> Self {
        Self {
            services: Mutex::new(None),
            runtime: Mutex::new(None),
            counters: Arc::new(ToolCounters::default()),
        }
    }

    /// Resolve the instance services once. A changed configuration requires a
    /// process restart rather than silently rebinding a live worktree.
    pub fn services(
        &self,
        settings: &WorkerSettings,
    ) -> Result<Arc<CodingServices>, common::ApiError> {
        let mut slot = self.services.lock().expect("services lock is not poisoned");
        if let Some(services) = slot.as_ref() {
            let requested = crate::services::CodingConfig::load(settings)?;
            if services.config().digest != requested.digest {
                return Err(common::ApiError::new(
                    "coding.restart_required",
                    "repository service settings changed; restart the instance",
                ));
            }
            return Ok(services.clone());
        }
        let services = Arc::new(CodingServices::open(settings)?);
        *slot = Some(services.clone());
        Ok(services)
    }

    /// Current services, when startup composed them.
    pub fn current(&self) -> Result<Arc<CodingServices>, common::ApiError> {
        self.services
            .lock()
            .expect("services lock is not poisoned")
            .clone()
            .ok_or_else(|| {
                common::ApiError::new(
                    "coding.workspace_unavailable",
                    "coding services were not composed for this instance",
                )
            })
    }

    /// Bind the active runtime for evidence commits.
    pub fn attach_runtime(&self, runtime: Runtime) {
        *self.runtime.lock().expect("runtime lock is not poisoned") = Some(runtime);
    }

    /// Active runtime used by the after-task evidence hook.
    pub fn runtime(&self) -> Result<Runtime, common::ApiError> {
        self.runtime
            .lock()
            .expect("runtime lock is not poisoned")
            .clone()
            .ok_or_else(|| {
                common::ApiError::new(
                    "coding.runtime_unavailable",
                    "coding runtime was not attached at startup",
                )
            })
    }

    /// Shared plugin counters.
    pub fn counters(&self) -> Arc<ToolCounters> {
        self.counters.clone()
    }
}

impl Default for CodingPackage {
    fn default() -> Self {
        Self::new()
    }
}

/// The package's single Worker definition.
pub fn worker() -> Result<Worker, String> {
    let package = Arc::new(CodingPackage::new());
    let developer_package = package.clone();
    let reviewer_package = package.clone();
    let hooks_package = package.clone();
    Ok(Worker::new("coding", env!("CARGO_PKG_VERSION"))
        .required_permissions([
            "task:submit",
            "session:write",
            "workspace:write",
            "exec:command",
            "artifact:write",
        ])
        .features(["coding", "devtools"])
        .agent_from(
            "developer",
            "Writable native CodingAgent in an isolated repository worktree",
            move |settings| agent::developer(&developer_package, settings),
        )
        .agent_from(
            "reviewer",
            "Read-only native CodingAgent that inspects without write or exec tools",
            move |settings| agent::reviewer(&reviewer_package, settings),
        )
        .tool(
            "exec_command",
            Arc::new(CodingCommandTool::new(package.clone())) as Arc<dyn Tool>,
        )
        .workspace_setting("workspace_root")
        .capability_permissions("developer", ["workspace:write", "exec:command"])
        .capability_permissions("reviewer", ["workspace:read"])
        .capability_permissions("exec_command", ["exec:command"])
        .adk_plugin(audit_plugin(package.counters()))
        .hooks(Arc::new(PackageHooks::new(hooks_package))))
}

/// Installed capability inventory derived from the one Worker definition.
pub fn manifest() -> PackageManifest {
    worker()
        .expect("coding Worker definition is compiled from credential-free native pieces")
        .manifest()
}
