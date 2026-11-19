//! Query shapes accepted by Worker discovery and audit.

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub(super) struct DiscoveryQuery {
    #[serde(default)]
    pub(super) capability: Option<String>,
    #[serde(default)]
    pub(super) protocol: Option<String>,
    #[serde(default)]
    pub(super) locality: Option<String>,
    #[serde(default)]
    pub(super) health: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct AuditQuery {
    #[serde(default)]
    pub(super) worker: Option<String>,
}
