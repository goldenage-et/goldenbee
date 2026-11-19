//! Native ADK graph registration and definition-bound checkpoint persistence.
//!
//! A package registers compiled [`GraphAgent`](adk_rust::graph::GraphAgent)
//! definitions. The shared runtime supplies one native
//! [`SqliteCheckpointer`](adk_rust::graph::SqliteCheckpointer) per Worker
//! Instance and wraps it so every checkpoint carries the exact package,
//! instance, graph, version, and topology that produced it. The wrapper is a
//! compatibility check on the pinned `Checkpointer` extension point; it is not
//! another graph engine or a second checkpoint store.

use crate::execution::ResumeInspection;
use crate::{ApiError, Result};
use adk_rust::graph::checkpoint::RetentionPolicy;
use adk_rust::graph::{Checkpoint, Checkpointer, GraphAgent, GraphError, SqliteCheckpointer};
use adk_rust::Agent;
use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Native SQLite checkpoint file owned by one Worker Instance.
pub const GRAPH_STORE_FILE: &str = "native-graph.sqlite3";

/// Event metadata key marking a native graph interrupt.
pub use adk_rust::graph::interrupt::INTERRUPT_METADATA_KEY;

/// Checkpoint metadata key carrying the definition digest.
pub const GRAPH_DIGEST_KEY: &str = "goldenbee.graph_digest";
/// Checkpoint metadata key carrying the graph id.
pub const GRAPH_ID_KEY: &str = "goldenbee.graph_id";
/// Checkpoint metadata key carrying the graph definition version.
pub const GRAPH_VERSION_KEY: &str = "goldenbee.graph_version";
/// Checkpoint metadata key carrying the owning package name.
pub const GRAPH_PACKAGE_KEY: &str = "goldenbee.package";
/// Checkpoint metadata key carrying the owning Worker Instance id.
pub const GRAPH_INSTANCE_KEY: &str = "goldenbee.instance";

/// Native services supplied to one package graph factory.
#[derive(Clone)]
pub struct GraphServices {
    /// Worker Package name.
    pub package: String,
    /// Worker Package version.
    pub package_version: String,
    /// Stable Worker Instance identity.
    pub instance: String,
    /// Instance data directory.
    pub data_dir: PathBuf,
    /// Operator-supplied package settings resolved from the configuration.
    pub settings: BTreeMap<String, String>,
    /// Definition-bound native checkpointer. Pass it to the graph builder.
    pub checkpointer: Arc<DefinitionCheckpointer>,
    /// Worker-registered native tools, including protected effect adapters.
    ///
    /// Graph definitions that embed an `AgentNode` bind the same tool instance
    /// the package registered, so operation receipts and approval policy have
    /// one owner.
    pub tools: std::collections::BTreeMap<String, Arc<dyn adk_rust::Tool>>,
}

/// Shared-runtime factory that builds one native graph definition.
pub type GraphFactory =
    Arc<dyn Fn(&GraphServices) -> std::result::Result<GraphAgent, GraphError> + Send + Sync>;

/// One compiled graph definition registered by package code.
#[derive(Clone)]
pub struct GraphRegistration {
    /// Capability id used as the task agent name.
    pub id: String,
    /// Immutable definition version supplied by the package.
    pub version: u64,
    /// Human-readable description exposed in the checked manifest.
    pub description: String,
    build: GraphFactory,
}

impl GraphRegistration {
    /// Register a native graph factory.
    pub fn new<F>(id: impl Into<String>, version: u64, build: F) -> Self
    where
        F: Fn(&GraphServices) -> std::result::Result<GraphAgent, GraphError>
            + Send
            + Sync
            + 'static,
    {
        let id = id.into();
        Self {
            description: format!("native ADK graph workflow '{id}'"),
            id,
            version,
            build: Arc::new(build),
        }
    }

    /// Override the manifest description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

/// A built graph bound to one instance-scoped definition identity.
#[derive(Clone)]
pub struct GraphRuntime {
    /// Native graph agent.
    pub agent: Arc<dyn Agent>,
    /// Definition-bound native checkpointer.
    pub checkpointer: Arc<DefinitionCheckpointer>,
    /// Registered capability id.
    pub id: String,
    /// Registered definition version.
    pub version: u64,
    /// Definition digest computed from package, graph, version, and topology.
    pub digest: String,
    /// Manifest description.
    pub description: String,
}

impl GraphRuntime {
    /// Definition-bound checkpoint owner for lifecycle operations.
    pub fn checkpointer(&self) -> Arc<DefinitionCheckpointer> {
        self.checkpointer.clone()
    }

    /// Inspect the native checkpoint for a task thread without executing it.
    pub async fn inspect_resume(&self, thread_id: &str) -> ResumeInspection {
        match self.checkpointer.load(thread_id).await {
            Ok(None) => ResumeInspection::Missing,
            Ok(Some(checkpoint)) => {
                let state = match serde_json::to_value(&checkpoint.state) {
                    Ok(state) => state,
                    Err(error) => {
                        return ResumeInspection::Corrupt {
                            detail: format!("graph checkpoint state is not serializable: {error}"),
                        }
                    }
                };
                let output = checkpoint
                    .state
                    .get("output")
                    .or_else(|| checkpoint.state.get("result"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let resume = crate::execution::GraphResume {
                    thread_id: checkpoint.thread_id.clone(),
                    pending_nodes: checkpoint.pending_nodes.clone(),
                    step: checkpoint.step,
                    state,
                    output,
                };
                if checkpoint.pending_nodes.is_empty() {
                    ResumeInspection::Complete(resume)
                } else {
                    ResumeInspection::Paused(resume)
                }
            }
            Err(GraphError::CheckpointError(detail))
                if detail.starts_with("graph.definition_incompatible") =>
            {
                ResumeInspection::Incompatible { detail }
            }
            Err(error) => ResumeInspection::Corrupt {
                detail: error.to_string(),
            },
        }
    }
}

struct DefinitionIdentity {
    digest: String,
    metadata: BTreeMap<String, Value>,
}

/// A native checkpointer that binds every checkpoint to one graph definition.
///
/// Reads reject a checkpoint with no identity or a different digest, so a
/// changed graph cannot silently load another definition's state. The newest
/// checkpoint is retained by the native backend; this wrapper does not own
/// checkpoint state.
pub struct DefinitionCheckpointer {
    inner: Arc<dyn Checkpointer>,
    identity: RwLock<Option<DefinitionIdentity>>,
}

impl DefinitionCheckpointer {
    /// Wrap a native checkpointer whose definition identity is not yet bound.
    pub fn new(inner: Arc<dyn Checkpointer>) -> Self {
        Self {
            inner,
            identity: RwLock::new(None),
        }
    }

    /// Bind the exact definition identity written to every checkpoint.
    pub fn bind(&self, package: &str, instance: &str, graph: &str, version: u64, digest: &str) {
        let mut metadata = BTreeMap::new();
        metadata.insert(GRAPH_DIGEST_KEY.into(), json!(digest));
        metadata.insert(GRAPH_ID_KEY.into(), json!(graph));
        metadata.insert(GRAPH_VERSION_KEY.into(), json!(version));
        metadata.insert(GRAPH_PACKAGE_KEY.into(), json!(package));
        metadata.insert(GRAPH_INSTANCE_KEY.into(), json!(instance));
        if let Ok(mut identity) = self.identity.write() {
            *identity = Some(DefinitionIdentity {
                digest: digest.to_string(),
                metadata,
            });
        }
    }

    /// Current definition digest, when bound.
    pub fn digest(&self) -> Option<String> {
        self.identity
            .read()
            .ok()
            .and_then(|identity| identity.as_ref().map(|identity| identity.digest.clone()))
    }

    /// Remove every native checkpoint for one thread after caller-authorized
    /// retention decides the thread is no longer recoverable.
    pub async fn delete_thread(&self, thread_id: &str) -> crate::Result<usize> {
        let before = self
            .inner
            .list(thread_id)
            .await
            .map_err(|error| crate::Error::new("graph.checkpoint", error.to_string()))?;
        if before.is_empty() {
            return Ok(0);
        }
        self.inner
            .delete(thread_id)
            .await
            .map_err(|error| crate::Error::new("graph.checkpoint", error.to_string()))?;
        Ok(before.len())
    }

    /// Read the newest native checkpoint for one thread without executing it.
    pub async fn latest(&self, thread_id: &str) -> crate::Result<Option<Checkpoint>> {
        self.inner
            .load(thread_id)
            .await
            .map_err(|error| crate::Error::new("graph.checkpoint", error.to_string()))
    }

    fn check(&self, checkpoint: &Checkpoint) -> std::result::Result<(), GraphError> {
        let Ok(identity) = self.identity.read() else {
            return Err(GraphError::CheckpointError(
                "graph.definition_unbound: checkpointer identity is unavailable".into(),
            ));
        };
        let Some(identity) = identity.as_ref() else {
            return Err(GraphError::CheckpointError(
                "graph.definition_unbound: checkpointer has no bound definition".into(),
            ));
        };
        let stored = checkpoint
            .metadata
            .get(GRAPH_DIGEST_KEY)
            .and_then(Value::as_str);
        if stored != Some(identity.digest.as_str()) {
            return Err(GraphError::CheckpointError(format!(
                "graph.definition_incompatible: checkpoint belongs to a different graph definition (stored={}, current={})",
                stored.unwrap_or("missing"),
                identity.digest
            )));
        }
        Ok(())
    }

    /// Native checkpointer beneath the definition binding.
    pub fn inner(&self) -> Arc<dyn Checkpointer> {
        self.inner.clone()
    }
}

#[async_trait]
impl Checkpointer for DefinitionCheckpointer {
    async fn save(&self, checkpoint: &Checkpoint) -> std::result::Result<String, GraphError> {
        let mut checkpoint = checkpoint.clone();
        {
            let Ok(identity) = self.identity.read() else {
                return Err(GraphError::CheckpointError(
                    "graph.definition_unbound: checkpointer identity is unavailable".into(),
                ));
            };
            let Some(identity) = identity.as_ref() else {
                return Err(GraphError::CheckpointError(
                    "graph.definition_unbound: checkpointer has no bound definition".into(),
                ));
            };
            for (key, value) in &identity.metadata {
                checkpoint.metadata.insert(key.clone(), value.clone());
            }
        }
        self.inner.save(&checkpoint).await
    }

    async fn load(&self, thread_id: &str) -> std::result::Result<Option<Checkpoint>, GraphError> {
        let checkpoint = self.inner.load(thread_id).await?;
        if let Some(checkpoint) = &checkpoint {
            self.check(checkpoint)?;
        }
        Ok(checkpoint)
    }

    async fn load_by_id(
        &self,
        checkpoint_id: &str,
    ) -> std::result::Result<Option<Checkpoint>, GraphError> {
        let checkpoint = self.inner.load_by_id(checkpoint_id).await?;
        if let Some(checkpoint) = &checkpoint {
            self.check(checkpoint)?;
        }
        Ok(checkpoint)
    }

    async fn list(&self, thread_id: &str) -> std::result::Result<Vec<Checkpoint>, GraphError> {
        let checkpoints = self.inner.list(thread_id).await?;
        for checkpoint in &checkpoints {
            self.check(checkpoint)?;
        }
        Ok(checkpoints)
    }

    async fn delete(&self, thread_id: &str) -> std::result::Result<(), GraphError> {
        self.inner.delete(thread_id).await
    }

    async fn prune(
        &self,
        thread_id: &str,
        policy: &RetentionPolicy,
    ) -> std::result::Result<usize, GraphError> {
        self.inner.prune(thread_id, policy).await
    }
}

/// Digest of the exact package, instance, graph, version, and native topology.
pub fn definition_digest(
    package: &str,
    package_version: &str,
    graph: &str,
    version: u64,
    agent: &dyn Agent,
) -> String {
    let topology = agent
        .topology()
        .and_then(|topology| serde_json::to_value(topology).ok())
        .unwrap_or(Value::Null);
    let material = json!({
        "package": package,
        "package_version": package_version,
        "graph": graph,
        "version": version,
        "topology": topology,
    });
    let bytes = serde_json::to_vec(&material).unwrap_or_default();
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Build every registered graph against one instance-scoped native store.
pub async fn build_graphs(
    package: &str,
    package_version: &str,
    instance: &str,
    data_dir: &Path,
    settings: &BTreeMap<String, String>,
    tools: &BTreeMap<String, Arc<dyn adk_rust::Tool>>,
    registrations: &BTreeMap<String, GraphRegistration>,
) -> Result<BTreeMap<String, GraphRuntime>> {
    std::fs::create_dir_all(data_dir).map_err(|error| {
        ApiError::new(
            "graph.storage_unavailable",
            format!("cannot create graph data directory: {error}"),
        )
    })?;
    let path = data_dir.join(GRAPH_STORE_FILE);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let native: Arc<dyn Checkpointer> =
        Arc::new(SqliteCheckpointer::new(&url).await.map_err(|error| {
            ApiError::new(
                "graph.storage_unavailable",
                format!("native graph checkpoint store cannot open: {error}"),
            )
        })?);
    let mut runtimes = BTreeMap::new();
    for registration in registrations.values() {
        let checkpointer = Arc::new(DefinitionCheckpointer::new(native.clone()));
        let services = GraphServices {
            package: package.to_string(),
            package_version: package_version.to_string(),
            instance: instance.to_string(),
            data_dir: data_dir.to_path_buf(),
            settings: settings.clone(),
            checkpointer: checkpointer.clone(),
            tools: tools.clone(),
        };
        let agent = (registration.build)(&services).map_err(|error| {
            ApiError::new(
                "graph.build_failed",
                format!(
                    "native graph '{}' could not be built: {error}",
                    registration.id
                ),
            )
        })?;
        let digest = definition_digest(
            package,
            package_version,
            &registration.id,
            registration.version,
            &agent,
        );
        checkpointer.bind(
            package,
            instance,
            &registration.id,
            registration.version,
            &digest,
        );
        let description = if registration.description.trim().is_empty() {
            agent.description().to_string()
        } else {
            registration.description.clone()
        };
        runtimes.insert(
            registration.id.clone(),
            GraphRuntime {
                agent: Arc::new(agent),
                checkpointer,
                id: registration.id.clone(),
                version: registration.version,
                digest,
                description,
            },
        );
    }
    Ok(runtimes)
}
