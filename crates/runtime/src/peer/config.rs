//! Static peer authorization and the verified peer record.
//!
//! A peer exists only because an operator listed it in the Worker
//! configuration. Presence is the authorization decision; endpoint, identity,
//! transport security, and outbound network policy are checked again before
//! the first connection. No Gateway discovery is required.

use crate::ApiError;
use common::Limits;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use url::Url;

/// One explicitly authorized remote Worker Instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerConfig {
    /// Expected remote Worker Instance identity. The resolved peer must report
    /// this exact instance id on its public liveness route.
    pub identity: String,
    /// Approved base endpoint. No path, query, credentials, or fragment.
    pub endpoint: String,
    /// Capabilities this operator authorizes on the peer. The effective set is
    /// the intersection with the peer's advertised A2A skills.
    pub capabilities: Vec<String>,
    /// Lifetime of a child delegation issued for this peer.
    #[serde(default = "default_token_ttl_ms")]
    pub token_ttl_ms: u64,
    /// Maximum delegation depth a child may carry.
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// Maximum concurrent non-terminal child delegations under one parent.
    #[serde(default = "default_max_fanout")]
    pub max_fanout: u32,
    /// Peer-wide resource ceiling. A child request is narrowed to this.
    #[serde(default)]
    pub limits: Limits,
}

fn default_token_ttl_ms() -> u64 {
    30_000
}

fn default_max_depth() -> u32 {
    1
}

fn default_max_fanout() -> u32 {
    1
}

impl PeerConfig {
    /// Validate operator configuration without contacting the peer.
    pub fn validate(&self, id: &str, allow_remote: bool) -> Result<(), ApiError> {
        if !valid_id(id, 64) {
            return Err(ApiError::new(
                "peer.config_invalid",
                format!("peer id '{id}' must be 1-64 characters of [A-Za-z0-9_-]"),
            ));
        }
        if self.identity.trim().is_empty() || self.identity.len() > 128 {
            return Err(ApiError::new(
                "peer.config_invalid",
                format!("peer '{id}' identity must be 1-128 characters"),
            ));
        }
        if self.capabilities.is_empty() {
            return Err(ApiError::new(
                "peer.config_invalid",
                format!("peer '{id}' must authorize at least one capability"),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for capability in &self.capabilities {
            if !valid_id(capability, 128) {
                return Err(ApiError::new(
                    "peer.config_invalid",
                    format!("peer '{id}' capability '{capability}' is invalid"),
                ));
            }
            if !seen.insert(capability.as_str()) {
                return Err(ApiError::new(
                    "peer.config_invalid",
                    format!("peer '{id}' lists capability '{capability}' twice"),
                ));
            }
        }
        if self.token_ttl_ms == 0 || self.token_ttl_ms > 300_000 {
            return Err(ApiError::new(
                "peer.config_invalid",
                format!("peer '{id}' token_ttl_ms must be 1-300000"),
            ));
        }
        if self.max_depth == 0 || self.max_depth > 16 {
            return Err(ApiError::new(
                "peer.config_invalid",
                format!("peer '{id}' max_depth must be 1-16"),
            ));
        }
        if self.max_fanout == 0 || self.max_fanout > 64 {
            return Err(ApiError::new(
                "peer.config_invalid",
                format!("peer '{id}' max_fanout must be 1-64"),
            ));
        }
        let url = parse_endpoint(&self.endpoint).map_err(|error| {
            ApiError::new(
                "peer.config_invalid",
                format!("peer '{id}' endpoint is invalid: {}", error.message),
            )
        })?;
        let loopback = is_loopback(&url);
        if !loopback && !allow_remote {
            return Err(ApiError::new(
                "peer.remote_not_approved",
                format!("peer '{id}' is not loopback and remote operation is not approved"),
            ));
        }
        if !loopback && url.scheme() != "https" {
            return Err(ApiError::new(
                "peer.tls_required",
                format!("peer '{id}' outside loopback requires an https endpoint"),
            ));
        }
        if self.limits.max_depth > 0 && self.limits.max_depth > self.max_depth {
            return Err(ApiError::new(
                "peer.config_invalid",
                format!("peer '{id}' limits.max_depth exceeds max_depth"),
            ));
        }
        if self.limits.max_fanout > 0 && self.limits.max_fanout > self.max_fanout {
            return Err(ApiError::new(
                "peer.config_invalid",
                format!("peer '{id}' limits.max_fanout exceeds max_fanout"),
            ));
        }
        Ok(())
    }

    /// Parsed endpoint base with trailing slashes removed.
    pub fn base_url(&self) -> Result<String, ApiError> {
        let url = parse_endpoint(&self.endpoint)?;
        Ok(url.as_str().trim_end_matches('/').to_string())
    }

    /// Whether the configured endpoint uses TLS.
    pub fn tls(&self) -> Result<bool, ApiError> {
        Ok(parse_endpoint(&self.endpoint)?.scheme() == "https")
    }
}

/// Durable record of the last verified peer identity and effective capability
/// set. It never carries a credential or child delegation token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRecord {
    /// Operator-assigned peer id.
    pub id: String,
    /// Approved instance identity from configuration.
    pub identity: String,
    /// Configured base endpoint.
    pub endpoint: String,
    /// Last observed remote instance identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Installed package name from the verified agent card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Installed package version from the verified agent card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_version: Option<String>,
    /// Capabilities advertised by the peer card.
    #[serde(default)]
    pub advertised_capabilities: Vec<String>,
    /// Effective capabilities: authorized by the operator and advertised by the
    /// peer. Only this set may be requested.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Whether the verified transport uses TLS.
    pub tls: bool,
    /// Verified when the last resolution completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<i64>,
    /// `configured` or `verified`.
    pub status: String,
}

impl PeerRecord {
    /// Static projection before the peer has been contacted.
    pub fn configured(id: &str, config: &PeerConfig) -> Self {
        Self {
            id: id.to_string(),
            identity: config.identity.clone(),
            endpoint: config.endpoint.trim_end_matches('/').to_string(),
            instance: None,
            package: None,
            package_version: None,
            advertised_capabilities: Vec::new(),
            capabilities: Vec::new(),
            tls: config.tls().unwrap_or(false),
            verified_at: None,
            status: "configured".into(),
        }
    }
}

/// Parse and reject unsafe endpoint shapes before any request is built.
pub fn parse_endpoint(endpoint: &str) -> Result<Url, ApiError> {
    let url = Url::parse(endpoint).map_err(|error| {
        ApiError::new(
            "peer.endpoint_invalid",
            format!("invalid endpoint: {error}"),
        )
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ApiError::new(
            "peer.endpoint_invalid",
            "peer endpoint must use http or https",
        ));
    }
    if url.host_str().is_none() {
        return Err(ApiError::new(
            "peer.endpoint_invalid",
            "peer endpoint must include a host",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::new(
            "peer.endpoint_invalid",
            "peer endpoint must not carry credentials",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ApiError::new(
            "peer.endpoint_invalid",
            "peer endpoint must not carry a query or fragment",
        ));
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err(ApiError::new(
            "peer.endpoint_invalid",
            "peer endpoint must be a base origin without a path",
        ));
    }
    Ok(url)
}

/// Loopback detection used by peer and Worker endpoint policy.
pub fn is_loopback(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}

fn valid_id(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(endpoint: &str) -> PeerConfig {
        PeerConfig {
            identity: "worker-b".into(),
            endpoint: endpoint.into(),
            capabilities: vec!["general".into()],
            token_ttl_ms: 30_000,
            max_depth: 1,
            max_fanout: 1,
            limits: Limits::default(),
        }
    }

    #[test]
    fn loopback_http_is_allowed_and_remote_http_is_rejected() {
        peer("http://127.0.0.1:9000")
            .validate("b", false)
            .expect("loopback http");
        let error = peer("http://example.com")
            .validate("b", false)
            .expect_err("remote http without approval");
        assert_eq!(error.code, "peer.remote_not_approved");
        let error = peer("http://example.com")
            .validate("b", true)
            .expect_err("remote http without tls");
        assert_eq!(error.code, "peer.tls_required");
        peer("https://example.com")
            .validate("b", true)
            .expect("remote https");
    }

    #[test]
    fn credentials_and_paths_are_rejected() {
        for endpoint in [
            "http://user:secret@127.0.0.1:9000",
            "http://127.0.0.1:9000/a2a",
            "http://127.0.0.1:9000/?token=1",
        ] {
            let error = peer(endpoint).validate("b", true).expect_err(endpoint);
            assert_eq!(error.code, "peer.config_invalid", "{endpoint}");
        }
    }

    #[test]
    fn duplicate_capabilities_and_inverted_limits_are_rejected() {
        let mut config = peer("http://127.0.0.1:9000");
        config.capabilities.push("general".into());
        assert_eq!(
            config.validate("b", false).unwrap_err().code,
            "peer.config_invalid"
        );
        let mut config = peer("http://127.0.0.1:9000");
        config.max_depth = 1;
        config.limits.max_depth = 4;
        assert_eq!(
            config.validate("b", false).unwrap_err().code,
            "peer.config_invalid"
        );
    }
}
