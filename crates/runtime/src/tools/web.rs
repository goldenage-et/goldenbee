//! Bounded web fetch and search under one destination policy.
//!
//! The runtime owns the transport so every destination, redirect, and response
//! size is checked by the same code. Provider-native search tools are not used:
//! they execute outside this policy and cannot report a truthful destination
//! decision to the task event stream.

use super::kernel::{ToolDescriptor, ToolEffect, ToolRisk};
use adk_rust::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

/// Destination and size policy for the web tools.
///
/// An empty allowlist denies every destination. A Policy can narrow what the
/// package declared but is always explicit; there is no implicit public web.
#[derive(Debug, Clone)]
pub struct WebPolicy {
    allowed_domains: Vec<String>,
    allow_http: bool,
    allow_private_network: bool,
    allow_redirects: bool,
    max_redirects: usize,
    max_bytes: usize,
    timeout: Duration,
    search_url: Option<String>,
}

impl Default for WebPolicy {
    fn default() -> Self {
        Self {
            allowed_domains: Vec::new(),
            allow_http: false,
            allow_private_network: false,
            allow_redirects: false,
            max_redirects: 0,
            max_bytes: 1024 * 1024,
            timeout: Duration::from_secs(15),
            search_url: None,
        }
    }
}

impl WebPolicy {
    /// Fail-closed policy: no destination, HTTPS only, bounded response.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow one exact host. An entry beginning with `.` or `*.` also allows
    /// subdomains of the remaining suffix.
    pub fn allow_domain(mut self, domain: impl Into<String>) -> Self {
        let domain = domain.into().trim().to_ascii_lowercase();
        if !domain.is_empty() {
            self.allowed_domains.push(domain);
        }
        self
    }

    /// Permit plain HTTP for allowed destinations. Off by default.
    pub fn allow_http(mut self, allow: bool) -> Self {
        self.allow_http = allow;
        self
    }

    /// Permit destinations that resolve to loopback or private addresses.
    ///
    /// This exists for an operator-controlled local search service. It is off
    /// by default because a model-supplied URL must not reach the host network.
    pub fn allow_private_network(mut self, allow: bool) -> Self {
        self.allow_private_network = allow;
        self
    }

    /// Follow redirects, up to `max_redirects`, re-checking every hop.
    pub fn allow_redirects(mut self, max_redirects: usize) -> Self {
        self.allow_redirects = max_redirects > 0;
        self.max_redirects = max_redirects;
        self
    }

    /// Response body ceiling in bytes.
    pub fn max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes.max(1);
        self
    }

    /// Per-request deadline.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Configure a search endpoint. The URL must contain `{query}`.
    pub fn search_url(mut self, url: impl Into<String>) -> Self {
        self.search_url = Some(url.into());
        self
    }

    /// Allowed destination hosts.
    pub fn domains(&self) -> &[String] {
        &self.allowed_domains
    }

    /// Configured search endpoint, when one exists.
    pub fn search_endpoint(&self) -> Option<&str> {
        self.search_url.as_deref()
    }

    /// Validate the policy before a Worker starts serving.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if let Some(url) = &self.search_url {
            if !url.contains("{query}") {
                return Err("web search URL must contain the '{query}' placeholder".into());
            }
            let parsed = Url::parse(&url.replace("{query}", "q"))
                .map_err(|error| format!("web search URL is invalid: {error}"))?;
            if parsed.scheme() != "https" && !(self.allow_http && parsed.scheme() == "http") {
                return Err("web search URL scheme is not permitted by this policy".into());
            }
        }
        Ok(())
    }

    fn permits_scheme(&self, scheme: &str) -> bool {
        scheme == "https" || (self.allow_http && scheme == "http")
    }

    fn permits_host(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.allowed_domains.iter().any(|entry| {
            if let Some(suffix) = entry.strip_prefix("*.").or_else(|| entry.strip_prefix('.')) {
                host == suffix || host.ends_with(&format!(".{suffix}"))
            } else {
                host == *entry
            }
        })
    }
}

struct WebTools {
    policy: Arc<WebPolicy>,
}

impl WebTools {
    fn descriptor(capability: &'static str) -> ToolDescriptor {
        ToolDescriptor::new(capability)
            .risk(ToolRisk::Medium)
            .effect(ToolEffect::None)
            .builtin()
            .typed_errors()
            .network()
            .audit()
    }
}

/// Fetch one bounded HTTP response.
pub struct WebFetchTool {
    inner: WebTools,
}

impl WebFetchTool {
    /// Build the fetch tool from an explicit policy.
    pub fn new(policy: Arc<WebPolicy>) -> Self {
        Self {
            inner: WebTools { policy },
        }
    }

    /// Runtime tool descriptor.
    pub fn descriptor() -> ToolDescriptor {
        WebTools::descriptor("web:fetch")
    }
}

/// Search through an operator-configured endpoint.
pub struct WebSearchTool {
    inner: WebTools,
}

impl WebSearchTool {
    /// Build the search tool from an explicit policy.
    pub fn new(policy: Arc<WebPolicy>) -> Self {
        Self {
            inner: WebTools { policy },
        }
    }

    /// Runtime tool descriptor.
    pub fn descriptor() -> ToolDescriptor {
        WebTools::descriptor("web:search")
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch one URL permitted by the Worker network policy. Returns the final \
         URL, status, content type, and bounded text body. Redirects and \
         private-network destinations are refused unless explicitly allowed."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Absolute http or https URL."
                }
            },
            "required": ["url"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(url) = args.get("url").and_then(Value::as_str) else {
            return Ok(web_error(
                "web.invalid_url",
                "missing required string argument 'url'",
            ));
        };
        match fetch(&self.inner.policy, url).await {
            Ok(fetched) => Ok(fetched.into_value()),
            Err(error) => Ok(error.into_value()),
        }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search through the Worker's configured search endpoint. Returns typed \
         results whose URLs are filtered by the same destination policy as \
         web_fetch."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum results to return (default 5, max 20)."
                }
            },
            "required": ["query"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(query) = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
        else {
            return Ok(web_error(
                "web.invalid_query",
                "missing required string argument 'query'",
            ));
        };
        if query.chars().count() > 1024 {
            return Ok(web_error(
                "web.invalid_query",
                "the query exceeds 1024 characters",
            ));
        }
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .clamp(1, 20) as usize;
        let Some(template) = self.inner.policy.search_url.as_deref() else {
            return Ok(web_error(
                "web.not_configured",
                "this Worker declares no search endpoint",
            ));
        };
        let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        let url = template.replace("{query}", &encoded);
        let fetched = match fetch(&self.inner.policy, &url).await {
            Ok(fetched) => fetched,
            Err(error) => return Ok(error.into_value()),
        };
        let parsed: Value = match serde_json::from_str(&fetched.body) {
            Ok(value) => value,
            Err(_) => {
                return Ok(web_error(
                    "web.search_invalid_response",
                    "the search endpoint did not return JSON results",
                ))
            }
        };
        let items = parsed
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| parsed.as_array().cloned());
        let Some(items) = items else {
            return Ok(web_error(
                "web.search_invalid_response",
                "the search endpoint did not return a results array",
            ));
        };
        let mut results = Vec::new();
        let mut filtered = 0usize;
        for item in items {
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(512)
                .collect::<String>();
            let url = item
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(2048)
                .collect::<String>();
            let snippet = item
                .get("snippet")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(2048)
                .collect::<String>();
            if destination_allowed(&self.inner.policy, &url).is_err() {
                filtered += 1;
                continue;
            }
            results.push(json!({
                "title": title,
                "url": url,
                "snippet": snippet,
            }));
            if results.len() >= limit {
                break;
            }
        }
        Ok(json!({
            "query": query,
            "endpoint": fetched.final_url,
            "results": results,
            "count": results.len(),
            "filtered": filtered,
            "truncated": fetched.truncated,
        }))
    }
}

struct Fetched {
    final_url: String,
    status: u16,
    content_type: String,
    body: String,
    bytes: usize,
    truncated: bool,
}

impl Fetched {
    fn into_value(self) -> Value {
        json!({
            "url": self.final_url,
            "final_url": self.final_url,
            "status": self.status,
            "content_type": self.content_type,
            "bytes": self.bytes,
            "truncated": self.truncated,
            "body": self.body,
        })
    }
}

struct WebError {
    code: &'static str,
    message: String,
}

impl WebError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn into_value(self) -> Value {
        web_error(self.code, self.message)
    }
}

fn web_error(code: &str, message: impl Into<String>) -> Value {
    let message: String = message.into();
    json!({
        "error": code,
        "message": common::redact(&message),
    })
}

async fn fetch(policy: &WebPolicy, raw: &str) -> std::result::Result<Fetched, WebError> {
    let mut url = Url::parse(raw)
        .map_err(|error| WebError::new("web.invalid_url", format!("invalid URL: {error}")))?;
    let mut redirects = 0usize;
    loop {
        if let Err(error) = destination_allowed(policy, url.as_str()) {
            return Err(if redirects > 0 {
                WebError::new("web.redirect_denied", error.message)
            } else {
                error
            });
        }
        let host = url
            .host_str()
            .ok_or_else(|| WebError::new("web.invalid_url", "URL has no host"))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| WebError::new("web.invalid_url", "URL has no port"))?;
        let addresses = resolve(policy, host, port).await?;
        let client = reqwest::Client::builder()
            .timeout(policy.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|error| {
                WebError::new("web.unavailable", format!("HTTP client failed: {error}"))
            })?;
        let response = client.get(url.clone()).send().await.map_err(|error| {
            if error.is_timeout() {
                WebError::new(
                    "web.timeout",
                    "the destination did not respond before the deadline",
                )
            } else {
                WebError::new("web.unavailable", format!("request failed: {error}"))
            }
        })?;
        let status = response.status();
        if status.is_redirection() {
            if !policy.allow_redirects || redirects >= policy.max_redirects {
                return Err(WebError::new(
                    "web.redirect_denied",
                    "the destination attempted a redirect this policy does not allow",
                ));
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    WebError::new("web.redirect_denied", "redirect had no Location header")
                })?;
            url = url.join(location).map_err(|error| {
                WebError::new(
                    "web.redirect_denied",
                    format!("invalid redirect target: {error}"),
                )
            })?;
            redirects += 1;
            continue;
        }
        if status.is_client_error() || status.is_server_error() {
            return Err(WebError::new(
                "web.status",
                format!("the destination returned HTTP {status}"),
            ));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        if !permitted_content_type(&content_type) {
            return Err(WebError::new(
                "web.unsupported_content",
                format!("content type '{content_type}' is not text"),
            ));
        }
        let mut response = response;
        let mut body = Vec::new();
        let mut truncated = false;
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                WebError::new(
                    "web.timeout",
                    "the destination did not finish before the deadline",
                )
            } else {
                WebError::new("web.unavailable", format!("response read failed: {error}"))
            }
        })? {
            let remaining = policy.max_bytes.saturating_sub(body.len());
            if chunk.len() > remaining {
                body.extend_from_slice(&chunk[..remaining]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }
        return Ok(Fetched {
            final_url: url.to_string(),
            status: status.as_u16(),
            content_type,
            body: String::from_utf8_lossy(&body).into_owned(),
            bytes: body.len(),
            truncated,
        });
    }
}

fn destination_allowed(policy: &WebPolicy, raw: &str) -> std::result::Result<(), WebError> {
    let url = Url::parse(raw)
        .map_err(|error| WebError::new("web.invalid_url", format!("invalid URL: {error}")))?;
    if url.username() != "" || url.password().is_some() {
        return Err(WebError::new(
            "web.invalid_url",
            "URL credentials are not permitted",
        ));
    }
    if !policy.permits_scheme(url.scheme()) {
        return Err(WebError::new(
            "web.destination_denied",
            format!("scheme '{}' is not permitted", url.scheme()),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| WebError::new("web.invalid_url", "URL has no host"))?;
    if !policy.permits_host(host) {
        return Err(WebError::new(
            "web.destination_denied",
            "the destination host is not in the allowlist",
        ));
    }
    Ok(())
}

async fn resolve(
    policy: &WebPolicy,
    host: &str,
    port: u16,
) -> std::result::Result<Vec<SocketAddr>, WebError> {
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| WebError::new("web.unavailable", format!("DNS lookup failed: {error}")))?
        .collect();
    if addresses.is_empty() {
        return Err(WebError::new(
            "web.unavailable",
            "DNS lookup returned no address",
        ));
    }
    if !policy.allow_private_network {
        if let Some(blocked) = addresses.iter().find(|address| !public_ip(address.ip())) {
            return Err(WebError::new(
                "web.private_network_denied",
                format!(
                    "the destination resolves to a non-public address ({})",
                    blocked.ip()
                ),
            ));
        }
    }
    Ok(addresses)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(address) => {
            !(address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_broadcast()
                || address.is_documentation()
                || address.is_unspecified()
                || address.is_multicast())
        }
        IpAddr::V6(address) => {
            !(address.is_loopback()
                || address.is_unspecified()
                || address.is_multicast()
                || address.is_unique_local()
                || address.is_unicast_link_local())
        }
    }
}

fn permitted_content_type(content_type: &str) -> bool {
    let media = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    media.starts_with("text/")
        || media == "application/json"
        || media == "application/xml"
        || media == "application/xhtml+xml"
        || media.ends_with("+json")
        || media.ends_with("+xml")
}
