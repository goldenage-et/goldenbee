//! Coding Worker Package composition.
//!
//! One [`runtime::worker::Worker`] definition supplies package identity and
//! instance-scoped repository services. The writable developer and read-only
//! reviewer are native ADK `CodingAgent` instances built from the same package;
//! neither is a separate executable.

mod agent;
mod build;
mod callbacks;
mod evidence;
mod hooks;
mod package;
mod repo;
mod services;
mod tool;

pub use package::{manifest, worker, CodingPackage, ToolCounters};
pub use repo::CodingResult;
pub use services::{CodingConfig, CodingServices};
pub use tool::CodingCommandTool;

/// Bounded evidence schema version emitted by package hooks.
pub const EVIDENCE_SCHEMA: &str = "coding.evidence@1";
