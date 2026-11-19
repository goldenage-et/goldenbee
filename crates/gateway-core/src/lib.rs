//! Goldenbee Gateway control plane.
//!
//! `gateway-core` owns Worker approvals, registration and leases, caller-scoped
//! routing, dispatch intent and idempotency, delegation issuance, discovery,
//! Worker administration and audit, retained and live event forwarding, and the
//! durable registry and dispatch stores.
//!
//! It does not own Axum routers, HTTP extraction or middleware, Server-Sent
//! Event formatting, listener startup, caller bearer-token configuration,
//! users, login, payments, subscriptions, entitlements, business tenant policy,
//! Worker runtime state, or native ADK execution.

pub mod backup;
mod caller;
mod config;
mod fault;
mod gateway;
mod lifecycle;
mod registry;
pub mod store;
mod stream;
pub mod vault;

pub use caller::Caller;
pub use config::{CoreConfig, HostConfig, StoreConfig, VaultConfig, WorkerConfig};
pub use gateway::{ArtifactContent, ArtifactStream, Gateway};
pub use registry::Clock;
pub use stream::EventStream;
