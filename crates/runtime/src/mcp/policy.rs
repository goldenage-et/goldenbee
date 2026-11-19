//! Deployment-specific destination, TLS, redirect, and DNS policy for remote
//! MCP Streamable HTTP.
//!
//! The pinned `McpHttpClientBuilder` exposes only a timeout and headers. This
//! adapter builds the reqwest 0.13 client itself so the Worker can refuse
//! redirects, pin the addresses it vetted, and apply private-network policy.

use crate::mcp::config::McpServerConfig;
use common::ApiError;
use reqwest13::redirect::{Attempt, Policy};
use reqwest13::{Client, Url};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

/// Error returned to reqwest when a remote server attempts a redirect.
#[derive(Debug)]
pub struct RedirectDenied;

impl fmt::Display for RedirectDenied {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("mcp.redirect_denied: remote MCP redirect is not permitted")
    }
}

impl std::error::Error for RedirectDenied {}

/// A destination that passed host, scheme, DNS, and network classification.
#[derive(Debug, Clone)]
pub struct VettedDestination {
    /// Validated endpoint.
    pub url: Url,
    /// Addresses vetted at connect time, pinned into the HTTP client.
    pub addrs: Vec<SocketAddr>,
    /// Whether the endpoint uses TLS.
    pub tls: bool,
}

/// Validated policy snapshot for one configured server.
#[derive(Debug, Clone)]
pub struct DestinationPolicy {
    allowed_hosts: Vec<String>,
    allow_loopback: bool,
    allow_private_network: bool,
    allow_insecure_http: bool,
    timeout: Duration,
}

impl DestinationPolicy {
    /// Build a policy from validated operator configuration.
    pub fn new(config: &McpServerConfig) -> Self {
        Self {
            allowed_hosts: config.allowed_hosts.clone(),
            allow_loopback: config.allow_loopback,
            allow_private_network: config.allow_private_network,
            allow_insecure_http: config.allow_insecure_http,
            timeout: Duration::from_millis(config.timeout_ms.max(1)),
        }
    }

    /// Vet a configured endpoint. Performs DNS resolution but no HTTP request.
    pub async fn vet(&self, endpoint: &str) -> Result<VettedDestination, ApiError> {
        let url = Url::parse(endpoint)
            .map_err(|_| ApiError::new("mcp.endpoint_denied", "MCP endpoint is not a valid URL"))?;
        if url.username() != "" || url.password().is_some() {
            return Err(ApiError::new(
                "mcp.endpoint_denied",
                "MCP endpoint must not embed credentials",
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(ApiError::new(
                "mcp.endpoint_denied",
                "MCP endpoint must not carry a query or fragment",
            ));
        }
        let tls = match url.scheme() {
            "https" => true,
            "http" => false,
            _ => {
                return Err(ApiError::new(
                    "mcp.endpoint_denied",
                    "MCP endpoint scheme is not permitted",
                ))
            }
        };
        let host = url
            .host_str()
            .ok_or_else(|| ApiError::new("mcp.endpoint_denied", "MCP endpoint has no host"))?
            .to_ascii_lowercase();
        if !self
            .allowed_hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&host))
        {
            return Err(ApiError::new(
                "mcp.endpoint_denied",
                "MCP endpoint host is not in the instance allowlist",
            ));
        }
        let port = url
            .port_or_known_default()
            .ok_or_else(|| ApiError::new("mcp.endpoint_denied", "MCP endpoint has no port"))?;

        let addrs = if let Ok(ip) = host.parse::<IpAddr>() {
            vec![SocketAddr::new(ip, port)]
        } else {
            let resolved = tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|_| {
                    ApiError::new("mcp.dns_unresolved", "MCP endpoint host did not resolve")
                })?;
            let addrs: Vec<_> = resolved.collect();
            if addrs.is_empty() {
                return Err(ApiError::new(
                    "mcp.dns_unresolved",
                    "MCP endpoint host resolved to no addresses",
                ));
            }
            addrs
        };

        for address in &addrs {
            self.classify(address.ip())?;
        }
        let loopback = addrs.iter().all(|address| address.ip().is_loopback());
        if !tls {
            if !loopback {
                return Err(ApiError::new(
                    "mcp.tls_required",
                    "remote MCP endpoints must use TLS",
                ));
            }
            if !self.allow_loopback || !self.allow_insecure_http {
                return Err(ApiError::new(
                    "mcp.endpoint_denied",
                    "plain-HTTP MCP is allowed only for explicitly enabled loopback",
                ));
            }
        }
        Ok(VettedDestination { url, addrs, tls })
    }

    fn classify(&self, ip: IpAddr) -> Result<(), ApiError> {
        let ip = match ip {
            IpAddr::V6(address) => address
                .to_ipv4_mapped()
                .map(IpAddr::V4)
                .unwrap_or(IpAddr::V6(address)),
            other => other,
        };
        match ip {
            IpAddr::V4(address) => {
                classify_v4(address, self.allow_loopback, self.allow_private_network)
            }
            IpAddr::V6(address) => {
                classify_v6(address, self.allow_loopback, self.allow_private_network)
            }
        }
    }

    /// Build the policy-controlled HTTP client and pin the vetted addresses.
    pub fn client(&self, destination: &VettedDestination) -> Result<Client, ApiError> {
        let host = destination
            .url
            .host_str()
            .ok_or_else(|| ApiError::new("mcp.endpoint_denied", "MCP endpoint has no host"))?;
        let mut builder = Client::builder()
            .timeout(self.timeout)
            .connect_timeout(self.timeout)
            .redirect(Policy::custom(|attempt: Attempt<'_>| {
                attempt.error(RedirectDenied)
            }))
            .https_only(destination.tls);
        if host.parse::<IpAddr>().is_err() {
            builder = builder.resolve_to_addrs(host, &destination.addrs);
        }
        builder.build().map_err(|_| {
            ApiError::new(
                "mcp.transport_unavailable",
                "MCP HTTP client could not start",
            )
        })
    }
}

fn classify_v4(ip: Ipv4Addr, allow_loopback: bool, allow_private: bool) -> Result<(), ApiError> {
    if ip.is_loopback() {
        return if allow_loopback {
            Ok(())
        } else {
            Err(ApiError::new(
                "mcp.endpoint_denied",
                "loopback MCP endpoints are not enabled for this instance",
            ))
        };
    }
    let non_public = ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.octets()[0] == 0;
    if non_public && !allow_private {
        return Err(ApiError::new(
            "mcp.endpoint_denied",
            "private-network MCP endpoints are not enabled for this instance",
        ));
    }
    Ok(())
}

fn classify_v6(ip: Ipv6Addr, allow_loopback: bool, allow_private: bool) -> Result<(), ApiError> {
    if ip.is_loopback() {
        return if allow_loopback {
            Ok(())
        } else {
            Err(ApiError::new(
                "mcp.endpoint_denied",
                "loopback MCP endpoints are not enabled for this instance",
            ))
        };
    }
    let non_public = ip.is_unique_local()
        || ip.is_unicast_link_local()
        || ip.is_multicast()
        || ip.is_unspecified();
    if non_public && !allow_private {
        return Err(ApiError::new(
            "mcp.endpoint_denied",
            "private-network MCP endpoints are not enabled for this instance",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::config::McpServerConfig;

    fn destination_policy(
        endpoint: &str,
        loopback: bool,
        private: bool,
        insecure: bool,
    ) -> DestinationPolicy {
        let config = McpServerConfig {
            endpoint: Some(endpoint.into()),
            enabled: true,
            allowed_tools: vec!["read".into()],
            credential: Some("secret://mcp/test".into()),
            required_scopes: Vec::new(),
            allowed_hosts: vec![Url::parse(endpoint)
                .expect("url")
                .host_str()
                .expect("host")
                .to_string()],
            allow_loopback: loopback,
            allow_private_network: private,
            allow_insecure_http: insecure,
            timeout_ms: 1_000,
            max_schema_bytes: 2_048,
            ..McpServerConfig::default()
        };
        DestinationPolicy::new(&config)
    }

    #[tokio::test]
    async fn private_metadata_address_is_denied_without_network_io() {
        let policy = destination_policy("http://169.254.169.254/mcp", true, false, true);
        assert_eq!(
            policy
                .vet("http://169.254.169.254/mcp")
                .await
                .expect_err("metadata address")
                .code,
            "mcp.endpoint_denied"
        );
    }

    #[tokio::test]
    async fn loopback_http_requires_explicit_development_opt_in() {
        let policy = destination_policy("http://127.0.0.1:9999/mcp", false, false, false);
        assert_eq!(
            policy
                .vet("http://127.0.0.1:9999/mcp")
                .await
                .expect_err("loopback disabled")
                .code,
            "mcp.endpoint_denied"
        );

        let policy = destination_policy("http://127.0.0.1:9999/mcp", true, false, false);
        assert_eq!(
            policy
                .vet("http://127.0.0.1:9999/mcp")
                .await
                .expect_err("insecure http disabled")
                .code,
            "mcp.endpoint_denied"
        );

        let policy = destination_policy("http://127.0.0.1:9999/mcp", true, false, true);
        assert!(policy.vet("http://127.0.0.1:9999/mcp").await.is_ok());
    }

    #[tokio::test]
    async fn plain_http_to_a_non_loopback_host_requires_tls() {
        let policy = destination_policy("http://192.0.2.10/mcp", false, true, true);
        assert_eq!(
            policy
                .vet("http://192.0.2.10/mcp")
                .await
                .expect_err("tls required")
                .code,
            "mcp.tls_required"
        );
    }

    #[tokio::test]
    async fn host_outside_allowlist_is_denied() {
        let policy = destination_policy("https://allowed.example.test/mcp", false, false, false);
        assert_eq!(
            policy
                .vet("https://other.example.test/mcp")
                .await
                .expect_err("host not allowed")
                .code,
            "mcp.endpoint_denied"
        );
    }
}
