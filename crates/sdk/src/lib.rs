//! Rust client library for the Goldenbee Gateway.
//!
//! Independent Client applications use this crate for connection behavior.
//! It never links native ADK execution, Worker runtime state, or Client
//! presentation. Typed operations are extended by later slices; this module
//! wires the client modules and owns transport, stable error decoding, and
//! bounded timeouts.

mod client;
mod outcome;
mod socket;

pub use client::{
    ArtifactDownload, Client, RetryPolicy, Watch, MAX_ARTIFACT_BYTES, MAX_STREAM_BUFFER,
};
pub use outcome::{cancellation_outcome, classify, retry_after, CancellationOutcome, ErrorKind};
pub use socket::{Socket, SocketSession, MAX_SOCKET_PENDING};

/// Wire protocol version implemented by this SDK.
pub const PROTOCOL_VERSION: &str = common::PROTOCOL_VERSION;
