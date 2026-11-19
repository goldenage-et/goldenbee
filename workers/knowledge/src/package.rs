//! The package's single Worker definition and derived capability inventory.

use crate::callbacks::{knowledge_plugin, native_callbacks};
use crate::hooks::{KnowledgeLifecycle, PackageHooks};
use crate::service::KnowledgeCell;
use crate::tools::{KnowledgeAskTool, KnowledgeIngestTool, KnowledgeRemoveTool};
use adk_rust::Tool;
use runtime::native::PackageManifest;
use runtime::worker::Worker;
use std::sync::Arc;

/// The package's single Worker definition.
///
/// Agents, prompts, skills, source roots, and storage are operator-selected
/// configuration. The compiled registrations are the three real knowledge
/// tools, package hooks, callbacks, and the native policy plugin.
pub fn worker() -> Result<Worker, String> {
    let cell = KnowledgeCell::new();
    let callbacks = cell.clone();
    Ok(Worker::new("knowledge", env!("CARGO_PKG_VERSION"))
        .feature("rag")
        .required_permissions(["task:submit", "session:write", "artifact:write"])
        .tool_with_permissions(
            "knowledge_ingest",
            Arc::new(KnowledgeIngestTool::new(cell.clone())) as Arc<dyn Tool>,
            ["knowledge:ingest", "artifact:write"],
        )
        .tool_with_permissions(
            "knowledge_ask",
            Arc::new(KnowledgeAskTool::new(cell.clone())) as Arc<dyn Tool>,
            ["knowledge:read"],
        )
        .tool_with_permissions(
            "knowledge_remove",
            Arc::new(KnowledgeRemoveTool::new(cell.clone())) as Arc<dyn Tool>,
            ["knowledge:remove"],
        )
        .callbacks(move || native_callbacks(callbacks.clone()))
        .adk_plugin(knowledge_plugin())
        .lifecycle(Arc::new(KnowledgeLifecycle::new(cell.clone())))
        .hooks(Arc::new(PackageHooks::new(cell))))
}

/// Installed capability inventory derived from the one Worker definition.
pub fn manifest() -> PackageManifest {
    worker()
        .expect("knowledge Worker definition is compiled from credential-free native pieces")
        .manifest()
}
