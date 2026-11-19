//! Tool registration and runtime-owned builtin tool bindings.

pub mod effect;

pub use effect::EffectTool;

use crate::worker::Worker;
use adk_rust::{Tool, Toolset};
use std::sync::Arc;

impl Worker {
    /// Register an additional native tool referenced by loaded agents.
    pub fn tool(self, id: impl Into<String>, tool: Arc<dyn Tool>) -> Self {
        self.tool_with_permissions(id, tool, std::iter::empty::<&str>())
    }

    /// Register a protected side-effecting native tool.
    ///
    /// The adapter owns an operation identity before dispatch and a receipt
    /// after a confirmed response. `reconciler` is the Worker-owned status or
    /// idempotency interface used after a crash or ambiguous response.
    pub fn effect_tool(
        self,
        id: impl Into<String>,
        tool: Arc<dyn Tool>,
        reconciler: Arc<dyn crate::execution::operations::EffectReconciler>,
        policy: crate::execution::operations::EffectPolicy,
    ) -> Self {
        let mut worker = self;
        let native_name = tool.name().to_string();
        if let Some(approval) = policy.approval.clone() {
            worker
                .effect_approvals
                .insert(native_name.clone(), approval);
        }
        let wrapper = Arc::new(EffectTool::new(
            tool,
            reconciler.clone(),
            policy,
            worker.effect_runtime.clone(),
        )) as Arc<dyn Tool>;
        let id = id.into();
        worker.effect_reconcilers.insert(native_name, reconciler);
        worker.tool_with_permissions(id, wrapper, std::iter::empty::<&str>())
    }

    /// Register a native tool with the permissions it actually exercises.
    ///
    /// Signed release metadata exposes these declarations so the installer can
    /// gate a package before its executable is accepted.
    pub fn tool_with_permissions<I, S>(
        mut self,
        id: impl Into<String>,
        tool: Arc<dyn Tool>,
        permissions: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let id = id.into();
        if self.tools.insert(id.clone(), tool).is_some() {
            self.duplicate_ids.insert(id.clone());
        }
        self.tool_permissions
            .insert(id, permissions.into_iter().map(Into::into).collect());
        self
    }

    /// Register an additional native toolset referenced by loaded agents.
    pub fn toolset(mut self, id: impl Into<String>, toolset: Arc<dyn Toolset>) -> Self {
        let id = id.into();
        if self.toolsets.insert(id.clone(), toolset).is_some() {
            self.duplicate_ids.insert(id);
        }
        self
    }
}
