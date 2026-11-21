//! Core runtime state, configuration, persistence, and shared primitives.

pub(crate) mod audit;
pub mod config;
pub mod crypto;
pub(crate) mod diagnostics;
pub(crate) mod error;
pub(crate) mod fault;
pub(crate) mod hooks;
pub(crate) mod ids;
pub(crate) mod redaction;
pub(crate) mod runtime;
pub(crate) mod store;
