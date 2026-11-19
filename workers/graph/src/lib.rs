//! Native graph workflow package.
//!
//! The package supplies one compiled `GraphAgent` definition. Shared Worker
//! runtime owns the instance-scoped SQLite checkpoint store, definition
//! binding, task dispatch, and restart resume; this package owns no workflow
//! engine and no checkpoint store of its own.

mod approval;
mod workflow;

pub use workflow::build as build_graph;

use runtime::native::PackageManifest;
use runtime::worker::Worker;
use std::sync::Arc;

/// The package's single Worker definition.
pub fn worker() -> Result<Worker, String> {
    Ok(Worker::new("graph", env!("CARGO_PKG_VERSION"))
        .required_permissions(["task:submit", "session:write"])
        .effect_tool(
            approval::TOOL,
            Arc::new(approval::LedgerTool::default()),
            Arc::new(approval::LedgerStatus::default()),
            approval::policy(),
        )
        .graph("branching", 1, workflow::build))
}

/// Installed capability inventory derived from the one Worker definition.
pub fn manifest() -> PackageManifest {
    worker()
        .expect("graph Worker definition is compiled from native graph pieces")
        .manifest()
}
