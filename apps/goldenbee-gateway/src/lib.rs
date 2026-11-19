//! Goldenbee standalone Gateway adapter.
//!
//! `goldenbee-gateway` is a thin HTTP deployment over `gateway-core`. It owns
//! caller bearer-token configuration and authentication, Axum routing and
//! middleware, JSON and Server-Sent Event serialization, operator
//! configuration loading, and listener startup. Registry, routing, dispatch,
//! idempotency, lease, delegation, and Worker event semantics belong to the
//! core.
//!
//! Route ownership is split by authentication class and feature: `workers`
//! holds Worker discovery, control, and administration, `tasks` holds task
//! proxy routes, and `router` composes them. `server` owns preparation and the
//! listener lifecycle.

// Axum handlers return `Response` as the rejected variant, matching the Worker
// serving module and keeping the handler contract direct.
#![allow(clippy::result_large_err)]

pub mod config;

mod auth;
mod control;
mod fleet;
mod health;
mod http;
mod profiles;
mod providers;
mod router;
mod server;
mod socket;
mod state;
mod tasks;
mod teams;
mod workers;

pub use config::{CallerConfig, Config, SocketConfig};
pub use server::{serve, Gateway};

use common::ApiError;

/// Load and validate an operator configuration file.
pub fn load_config(path: impl AsRef<std::path::Path>) -> Result<Config, ApiError> {
    Config::load(path)
}
