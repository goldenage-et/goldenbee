//! Authorized static peers and native A2A delegation.

mod config;
mod delegation;
mod transport;

pub use config::{is_loopback, parse_endpoint, PeerConfig, PeerRecord};
pub use delegation::{DelegateRequest, DelegatedArtifact, PeerDelegation};
pub use transport::PeerTransport;
