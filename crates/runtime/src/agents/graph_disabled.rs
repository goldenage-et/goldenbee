//! Graph-disabled compatibility surface.
//!
//! Packages that do not enable the `graph` runtime feature do not link the
//! native graph engine. The shared runtime still exposes the same registration
//! and catalog types so package composition stays uniform.

use crate::execution::ResumeInspection;
use crate::{ApiError, Result};
use adk_rust::Agent;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// Native SQLite checkpoint file name, reserved by the graph adapter.
pub const GRAPH_STORE_FILE: &str = "native-graph.sqlite3";

/// Event metadata key marking a native graph interrupt.
pub const INTERRUPT_METADATA_KEY: &str = "adk.graph.interrupt";

/// Native services supplied to one package graph factory.
#[derive(Clone)]
pub struct GraphServices;

/// One compiled graph definition registered by package code.
#[derive(Clone)]
pub struct GraphRegistration {
    /// Capability id used as the task agent name.
    pub id: String,
    /// Immutable definition version supplied by the package.
    pub version: u64,
    /// Human-readable description exposed in the checked manifest.
    pub description: String,
}

/// A built graph bound to one instance-scoped definition identity.
#[derive(Clone)]
pub struct GraphRuntime {
    /// Native graph agent.
    pub agent: Arc<dyn Agent>,
    /// Registered capability id.
    pub id: String,
    /// Registered definition version.
    pub version: u64,
    /// Definition digest.
    pub digest: String,
    /// Manifest description.
    pub description: String,
}

impl GraphRuntime {
    /// Definition-bound checkpoint owner for lifecycle operations.
    pub fn checkpointer(&self) -> Arc<DefinitionCheckpointer> {
        Arc::new(DefinitionCheckpointer)
    }

    /// Inspect the native checkpoint for a task thread without executing it.
    pub async fn inspect_resume(&self, _thread_id: &str) -> ResumeInspection {
        ResumeInspection::Unsupported
    }
}

/// Definition-bound native checkpointer, unavailable without the graph feature.
pub struct DefinitionCheckpointer;

impl DefinitionCheckpointer {
    /// No graph checkpoints exist in a build without the graph feature.
    pub async fn delete_thread(&self, _thread_id: &str) -> Result<usize> {
        Ok(0)
    }
}

/// Build every registered graph. No graph engine is linked in this build.
pub async fn build_graphs(
    _package: &str,
    _package_version: &str,
    _instance: &str,
    _data_dir: &Path,
    _settings: &BTreeMap<String, String>,
    _tools: &BTreeMap<String, Arc<dyn adk_rust::Tool>>,
    registrations: &BTreeMap<String, GraphRegistration>,
) -> Result<BTreeMap<String, GraphRuntime>> {
    if registrations.is_empty() {
        return Ok(BTreeMap::new());
    }
    Err(ApiError::new(
        "graph.feature_disabled",
        "this runtime build does not include the native graph feature",
    ))
}
