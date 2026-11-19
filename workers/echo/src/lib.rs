//! Echo Worker Package composition.
//!
//! One [`runtime::worker::Worker`] definition supplies package identity and one
//! compiled native tool. Agents, prompts, and skills remain operator-owned
//! configuration loaded by the shared runtime.

mod package;

pub use package::{manifest, worker};
