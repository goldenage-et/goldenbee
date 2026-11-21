//! Headless one-shot and JSONL protocol.
//!
//! Owner: the runtime protocol projection. This module resolves local input
//! sources, admits through the canonical task path, and projects the canonical
//! task event stream into the shared `common::run` wire types. It owns no
//! executor, session store, or event bus.

mod frames;
mod input;
mod run;

pub use input::resolve_sources;
