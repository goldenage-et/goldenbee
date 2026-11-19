//! Knowledge Worker Package composition.
//!
//! One [`runtime::worker::Worker`] definition supplies package identity,
//! three native tools over the pinned `adk-rag` pipeline, package-owned hooks
//! and callbacks, and the real file source adapter. Agents, prompts, skills,
//! source roots, and storage come from the operator-selected configuration;
//! no installer path or current working directory is consulted.

mod callbacks;
mod embedding;
mod error;
mod hooks;
mod jobs;
mod package;
mod service;
mod source;
mod tools;

pub use callbacks::{knowledge_plugin, native_callbacks};
pub use error::{KnowledgeError, KnowledgeResult};
pub use hooks::{KnowledgeLifecycle, PackageHooks};
pub use jobs::{JobRecord, JobStatus, JobStore};
pub use package::{manifest, worker};
pub use service::{
    IngestOutcome, KnowledgeCell, KnowledgeConfig, KnowledgeService, QueryOutcome, SourceCitation,
};
pub use source::{FileSource, OpenedSource};
pub use tools::{KnowledgeAskTool, KnowledgeIngestTool, KnowledgeRemoveTool};
