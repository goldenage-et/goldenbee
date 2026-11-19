//! Goldenbee Worker release registry client.
//!
//! Discovery, publication, and content-addressed download are implemented
//! here. The registry service and its operator configuration are a separate
//! deployment.

mod client;
mod types;

pub use client::RegistryClient;
pub use types::{
    BuildEvidenceView, PublicQuery, PublicRelease, PublicRevocation, PublishReceipt,
    PublisherTrust, RegistryError, ReleaseBundle, VersionEntry, PUBLIC_DEFAULT_DOWNLOAD_BYTES,
    PUBLIC_DEFAULT_RESULTS, PUBLIC_MAX_RESULTS,
};
