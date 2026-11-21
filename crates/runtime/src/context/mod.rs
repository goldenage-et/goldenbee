//! Deterministic runtime context assembly.
//!
//! [`ContextBundle`] owns configured memory-file loading. [`manifest::value`]
//! projects the assembled inputs into the durable `context.manifest` event.
//! Context assembly does not create a conversation store: native session
//! events remain the only canonical history, and native ADK compaction owns
//! any reduction of the active model context.

pub mod config;
pub mod manifest;
pub mod memory;

pub use config::{ContextConfig, MemorySource};
pub use manifest::{value as manifest, ManifestInput};
pub use memory::{ContextBundle, MemoryDiagnostic, MemoryFile};
