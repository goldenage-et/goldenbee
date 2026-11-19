//! Per-session egress proxy enforcing the browser destination policy for every
//! request, redirect, and subresource.
//!
//! WebDriver has no redirect or host policy. The browser is configured (through
//! its session capabilities) to send HTTP and HTTPS traffic to this loopback
//! proxy, so a redirect to an unlisted host is denied before the connection is
//! made. HTTPS destinations are validated on `CONNECT`; the tunnel then carries
//! the browser's own TLS verification.

use crate::browser::BrowserPolicy;
use common::ApiError;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use url::Url;

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 64;

/// Loopback HTTP proxy bound to one browser session.
pub struct EgressProxy {
    address: SocketAddr,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl EgressProxy {
    /// Start a proxy for one session.
    pub async fn start(policy: BrowserPolicy) -> Result<Self, ApiError> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|error| {
            ApiError::new(
                "browser.proxy_failed",
                format!("browser egress proxy could not bind: {error}"),
            )
        })?;
        let address = listener.local_addr().map_err(|error| {
            ApiError::new(
                "browser.proxy_failed",
                format!("browser egress proxy has no address: {error}"),
            )
        })?;
        let cancel = CancellationToken::new();
        let shutdown = cancel.clone();
        let task = tokio::spawn(async move {
            let connections = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
            loop {
                let accepted = tokio::select! {
                    _ = shutdown.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else {
                    continue;
                };
                let Ok(permit) = connections.clone().try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let policy = policy.clone();
                let shutdown = shutdown.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = handle_connection(stream, &policy, &shutdown).await;
                });
            }
        });
        Ok(Self {
            address,
            cancel,
            task,
        })
    }

    /// Proxy URL to inject into the browser session.
    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Proxy port for session preferences.
    pub fn port(&self) -> u16 {
        self.address.port()
    }

    /// Stop accepting connections and release the listener.
    pub fn shutdown(&self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    policy: &BrowserPolicy,
    shutdown: &CancellationToken,
) -> std::io::Result<()> {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 4096];
    loop {
        if buffer.len() > MAX_HEADER_BYTES {
            return deny(&mut stream, 431, "request headers too large").await;
        }
        let read = tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            read = stream.read(&mut chunk) => read?,
        };
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = header_end(&buffer) {
            let head = buffer[..end].to_vec();
            let body = buffer[end..].to_vec();
            return forward(stream, head, body, policy, shutdown).await;
        }
    }
}

async fn forward(
    mut stream: TcpStream,
    head: Vec<u8>,
    body: Vec<u8>,
    policy: &BrowserPolicy,
    shutdown: &CancellationToken,
) -> std::io::Result<()> {
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    if method.is_empty() || target.is_empty() {
        return deny(&mut stream, 400, "malformed proxy request").await;
    }
    let headers: Vec<&str> = lines.collect();

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = match split_authority(&target) {
            Some(value) => value,
            None => return deny(&mut stream, 400, "invalid CONNECT authority").await,
        };
        if let Err(error) = policy.check_host(&host, Some(port), "https") {
            return deny_policy(&mut stream, &error).await;
        }
        let address = match resolve_allowed(policy, &host, port, "https").await {
            Ok(address) => address,
            Err(error) => return deny_policy(&mut stream, &error).await,
        };
        let mut upstream = match TcpStream::connect(address).await {
            Ok(stream) => stream,
            Err(_) => return deny(&mut stream, 502, "destination unavailable").await,
        };
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        tokio::select! {
            _ = shutdown.cancelled() => Ok(()),
            _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream) => Ok(()),
        }
    } else {
        let url = match Url::parse(&target) {
            Ok(url) => url,
            Err(_) => return deny(&mut stream, 400, "proxy requires an absolute URL").await,
        };
        if let Err(error) = policy.check_parsed_url(&url) {
            return deny_policy(&mut stream, &error).await;
        }
        let host = url.host_str().unwrap_or_default().to_string();
        let port = url.port_or_known_default().unwrap_or(80);
        let address = match resolve_allowed(policy, &host, port, url.scheme()).await {
            Ok(address) => address,
            Err(error) => return deny_policy(&mut stream, &error).await,
        };
        let mut upstream = match TcpStream::connect(address).await {
            Ok(stream) => stream,
            Err(_) => return deny(&mut stream, 502, "destination unavailable").await,
        };
        let mut origin_form = url.path().to_string();
        if let Some(query) = url.query() {
            origin_form.push('?');
            origin_form.push_str(query);
        }
        let mut request = format!("{method} {origin_form} {version}\r\n");
        for header in headers {
            if header.is_empty() {
                continue;
            }
            let lower = header
                .split_once(':')
                .map(|(name, _)| name.trim().to_ascii_lowercase())
                .unwrap_or_default();
            if matches!(
                lower.as_str(),
                "proxy-connection"
                    | "connection"
                    | "keep-alive"
                    | "proxy-authorization"
                    | "te"
                    | "upgrade"
            ) {
                continue;
            }
            request.push_str(header);
            request.push_str("\r\n");
        }
        // Force one request per origin connection so every request goes back
        // through policy instead of being pipelined on an already-checked
        // connection.
        request.push_str("Connection: close\r\n\r\n");
        upstream.write_all(request.as_bytes()).await?;
        if !body.is_empty() {
            upstream.write_all(&body).await?;
        }
        tokio::select! {
            _ = shutdown.cancelled() => Ok(()),
            _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream) => Ok(()),
        }
    }
}

async fn resolve_allowed(
    policy: &BrowserPolicy,
    host: &str,
    port: u16,
    scheme: &str,
) -> Result<SocketAddr, ApiError> {
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| {
            ApiError::new(
                "browser.destination_denied",
                format!("browser destination '{host}' did not resolve: {error}"),
            )
        })?
        .collect();
    if addresses.is_empty() {
        return Err(ApiError::new(
            "browser.destination_denied",
            format!("browser destination '{host}' has no address"),
        ));
    }
    for address in &addresses {
        policy.check_resolved(host, port, scheme, &address.ip().to_string())?;
    }
    Ok(addresses[0])
}

async fn deny(stream: &mut TcpStream, status: u16, message: &str) -> std::io::Result<()> {
    let reason = match status {
        400 => "Bad Request",
        403 => "Forbidden",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        _ => "Forbidden",
    };
    let body = format!("browser policy: {message}");
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

async fn deny_policy(stream: &mut TcpStream, error: &ApiError) -> std::io::Result<()> {
    deny(stream, 403, &error.code).await
}

fn header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn split_authority(authority: &str) -> Option<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = rest[..end].to_ascii_lowercase();
        let port = rest[end + 1..].strip_prefix(':')?.parse().ok()?;
        return Some((host, port));
    }
    let (host, port) = authority.rsplit_once(':')?;
    Some((host.to_ascii_lowercase(), port.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ipv4_and_ipv6_authorities() {
        assert_eq!(
            split_authority("127.0.0.1:8080"),
            Some(("127.0.0.1".into(), 8080))
        );
        assert_eq!(split_authority("[::1]:443"), Some(("::1".into(), 443)));
        assert_eq!(split_authority("localhost"), None);
    }

    #[test]
    fn detects_header_end() {
        assert_eq!(header_end(b"GET / HTTP/1.1\r\n\r\n"), Some(18));
        assert_eq!(header_end(b"GET / HTTP/1.1\r\n"), None);
    }
}
