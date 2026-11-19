//! Browser destination, network, TLS, and redirect policy.
//!
//! The policy is evaluated outside the model prompt and outside the browser
//! tool descriptions. It runs before a navigation is handed to WebDriver and
//! again inside the session's egress proxy for every request the browser
//! makes, including redirects and subresources.

use common::ApiError;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use url::Url;

/// One explicitly authorized destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    /// Host name or IP literal, lower-cased.
    pub host: String,
    /// Optional port. `None` accepts the scheme's default port.
    pub port: Option<u16>,
}

/// Deterministic navigation policy for one Worker Instance.
#[derive(Debug, Clone)]
pub struct BrowserPolicy {
    destinations: Vec<Destination>,
    require_https: bool,
}

impl BrowserPolicy {
    /// A policy with no destinations, used only while the capability is
    /// disabled. Any navigation attempt still fails closed.
    pub(crate) fn none() -> Self {
        Self {
            destinations: Vec::new(),
            require_https: true,
        }
    }

    /// Build a policy from operator-configured `host[:port]` destinations.
    ///
    /// An empty or wildcard list is rejected: enabling the browser must be an
    /// explicit destination allowlist, not an unrestricted network capability.
    pub fn new<S: AsRef<str>>(destinations: &[S]) -> Result<Self, ApiError> {
        if destinations.is_empty() {
            return Err(ApiError::new(
                "browser.policy_empty",
                "browser.allowed_hosts must list at least one destination",
            ));
        }
        let mut parsed = Vec::new();
        for value in destinations {
            let value = value.as_ref().trim();
            if value.is_empty() || value == "*" {
                return Err(ApiError::new(
                    "browser.policy_invalid",
                    format!("browser destination '{value}' is not an explicit host"),
                ));
            }
            parsed.push(parse_destination(value)?);
        }
        Ok(Self {
            destinations: parsed,
            require_https: true,
        })
    }

    /// Require HTTPS for every destination outside loopback.
    pub fn require_https(mut self, required: bool) -> Self {
        self.require_https = required;
        self
    }

    /// Validate a model-supplied URL before it reaches WebDriver.
    pub fn check_url(&self, value: &str) -> Result<Url, ApiError> {
        let url = Url::parse(value).map_err(|error| {
            ApiError::new(
                "browser.url_invalid",
                format!("browser destination is not a valid URL: {error}"),
            )
        })?;
        self.check_parsed_url(&url)?;
        Ok(url)
    }

    /// Validate an already parsed URL.
    pub fn check_parsed_url(&self, url: &Url) -> Result<(), ApiError> {
        let scheme = url.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(ApiError::new(
                "browser.scheme_denied",
                format!("browser scheme '{scheme}' is not allowed"),
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| {
                ApiError::new(
                    "browser.destination_denied",
                    "browser URL has no destination host",
                )
            })?
            .to_ascii_lowercase();
        let port = url.port_or_known_default();
        self.check_host(&host, port, scheme)?;
        if self.require_https && scheme == "http" && !is_loopback_host(&host) {
            return Err(ApiError::new(
                "browser.tls_required",
                format!("browser destination '{host}' requires HTTPS"),
            ));
        }
        Ok(())
    }

    /// Validate a host/port/scheme tuple. Used by the redirect guard and the
    /// per-session egress proxy.
    pub fn check_host(&self, host: &str, port: Option<u16>, scheme: &str) -> Result<(), ApiError> {
        let host = normalize_host(host);
        if !self.matches(&host, port) {
            return Err(ApiError::new(
                "browser.destination_denied",
                format!("browser destination '{host}' is not allowed"),
            ));
        }
        if self.require_https && scheme == "http" && !is_loopback_host(&host) {
            return Err(ApiError::new(
                "browser.tls_required",
                format!("browser destination '{host}' requires HTTPS"),
            ));
        }
        Ok(())
    }

    /// Validate a resolved address. A public allowlisted host that resolves to
    /// a private, loopback, link-local, or unspecified address is refused
    /// unless the operator allowlisted that address literally (SSRF and DNS
    /// rebinding defence).
    pub fn check_resolved(
        &self,
        host: &str,
        port: u16,
        scheme: &str,
        address: &str,
    ) -> Result<(), ApiError> {
        let host = normalize_host(host);
        self.check_host(&host, Some(port), scheme)?;
        let ip = IpAddr::from_str(address).map_err(|error| {
            ApiError::new(
                "browser.destination_denied",
                format!("browser destination resolved to an invalid address: {error}"),
            )
        })?;
        let explicit_loopback = host.eq_ignore_ascii_case("localhost") && is_loopback(ip);
        if is_special_use(ip) && !is_explicit_literal(&host, ip) && !explicit_loopback {
            return Err(ApiError::new(
                "browser.private_address_denied",
                format!("browser destination '{host}' resolves to a private address"),
            ));
        }
        Ok(())
    }

    /// Whether a host/port pair is in the destination allowlist.
    pub fn matches(&self, host: &str, port: Option<u16>) -> bool {
        let host = normalize_host(host);
        self.destinations.iter().any(|destination| {
            destination.host == host && (destination.port.is_none() || destination.port == port)
        })
    }

    /// Configured destinations, for diagnostics and tests.
    pub fn destinations(&self) -> &[Destination] {
        &self.destinations
    }
}

fn parse_destination(value: &str) -> Result<Destination, ApiError> {
    if value.contains("://") {
        let url = Url::parse(value).map_err(|error| {
            ApiError::new(
                "browser.policy_invalid",
                format!("browser destination '{value}' is invalid: {error}"),
            )
        })?;
        let host = url
            .host_str()
            .ok_or_else(|| {
                ApiError::new(
                    "browser.policy_invalid",
                    format!("browser destination '{value}' has no host"),
                )
            })?
            .to_ascii_lowercase();
        return Ok(Destination {
            host,
            port: url.port(),
        });
    }
    let (host, port) = split_host_port(value)?;
    Ok(Destination { host, port })
}

fn split_host_port(value: &str) -> Result<(String, Option<u16>), ApiError> {
    if let Some(rest) = value.strip_prefix('[') {
        let end = rest.find(']').ok_or_else(|| {
            ApiError::new(
                "browser.policy_invalid",
                format!("browser destination '{value}' has an unterminated IPv6 literal"),
            )
        })?;
        let host = rest[..end].to_ascii_lowercase();
        let tail = &rest[end + 1..];
        let port = if let Some(port) = tail.strip_prefix(':') {
            Some(parse_port(value, port)?)
        } else if tail.is_empty() {
            None
        } else {
            return Err(ApiError::new(
                "browser.policy_invalid",
                format!("browser destination '{value}' is invalid"),
            ));
        };
        return Ok((host, port));
    }
    if let Some((host, port)) = value.rsplit_once(':') {
        // A bare IPv6 literal without brackets is ambiguous; require brackets.
        if host.contains(':') {
            return Ok((value.to_ascii_lowercase(), None));
        }
        return Ok((host.to_ascii_lowercase(), Some(parse_port(value, port)?)));
    }
    Ok((value.to_ascii_lowercase(), None))
}

fn parse_port(value: &str, port: &str) -> Result<u16, ApiError> {
    port.parse::<u16>().map_err(|_| {
        ApiError::new(
            "browser.policy_invalid",
            format!("browser destination '{value}' has an invalid port"),
        )
    })
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    IpAddr::from_str(&normalize_host(host))
        .map(is_loopback)
        .unwrap_or(false)
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    }
}

fn is_special_use(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_special_use_v4(ip),
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || is_unique_local_v6(ip)
                || is_link_local_v6(ip)
        }
    }
}

fn is_special_use_v4(ip: Ipv4Addr) -> bool {
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.octets()[0] == 0
        || (ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1]))
}

fn is_unique_local_v6(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

fn is_link_local_v6(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

fn is_explicit_literal(host: &str, ip: IpAddr) -> bool {
    IpAddr::from_str(host)
        .map(|configured| configured == ip)
        .unwrap_or(false)
}
