//! Documented legacy SSE compatibility adapter for `2024-11-05` MCP clients.
//!
//! The pinned ADK 2.3.0 / rmcp 3.11 client exposes stdio and Streamable HTTP
//! only ([`McpToolset`](adk_tool::mcp::McpToolset) and
//! `StreamableHttpClientTransport`). This module adds the deprecated HTTP+SSE
//! transport as a narrow adapter so an operator can reach a legacy server
//! without a second MCP client: the resulting transport is handed to the same
//! native `McpToolset`, registry, credential policy, and allowlists.
//!
//! Protocol shape (the 2024-11-05 transport):
//!
//! 1. `GET <endpoint>` with `Accept: text/event-stream`.
//! 2. The server's first event is `event: endpoint` with a same-origin message
//!    URL.
//! 3. Client messages are `POST`ed to that URL; responses arrive on the GET
//!    stream.
//!
//! Security: the message URL must stay on the vetted endpoint origin, the
//! credential is sent only as a bearer header, no redirect is followed, and one
//! event is size bounded before it is parsed.

use adk_tool::mcp::rmcp::model::ServerJsonRpcMessage;
use adk_tool::mcp::rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use adk_tool::mcp::rmcp::transport::Transport;
use adk_tool::mcp::rmcp::RoleClient;
use common::ApiError;
use futures::{Stream, StreamExt};
use reqwest13::{Response, Url};
use std::future::Future;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// One parsed server-sent event.
struct SseEvent {
    kind: String,
    data: String,
}

/// Transport failure. Detail is redacted before it reaches an event or log.
#[derive(Debug)]
pub(super) struct SseError(String);

impl std::fmt::Display for SseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SseError {}

impl SseError {
    fn new(detail: impl std::fmt::Display) -> Self {
        Self(common::redact(&detail.to_string()))
    }
}

/// Legacy SSE client transport handed to the native MCP client.
pub(super) struct SseTransport {
    client: reqwest13::Client,
    message_url: Url,
    token: Option<String>,
    sender: mpsc::Sender<ServerJsonRpcMessage>,
    receiver: mpsc::Receiver<ServerJsonRpcMessage>,
    cancel: CancellationToken,
}

impl Drop for SseTransport {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Transport<RoleClient> for SseTransport {
    type Error = SseError;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let client = self.client.clone();
        let url = self.message_url.clone();
        let token = self.token.clone();
        let sender = self.sender.clone();
        async move {
            let body = serde_json::to_vec(&item).map_err(SseError::new)?;
            let mut request = client
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
            if let Some(token) = token {
                request = request.bearer_auth(token);
            }
            let response = request.send().await.map_err(SseError::new)?;
            let status = response.status();
            if !status.is_success() {
                return Err(SseError::new(format!(
                    "legacy MCP message endpoint returned HTTP {}",
                    status.as_u16()
                )));
            }
            // Some servers answer the POST inline instead of only over SSE.
            let bytes = read_bounded(response, 1024 * 1024).await?;
            if !bytes.is_empty() {
                if let Ok(message) = serde_json::from_slice::<ServerJsonRpcMessage>(&bytes) {
                    let _ = sender.send(message).await;
                }
            }
            Ok(())
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        self.receiver.recv().await
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.cancel.cancel();
        Ok(())
    }
}

/// Open the legacy SSE stream and return the initialized transport plus the
/// service cancellation token.
///
/// The caller owns destination vetting and credential resolution; this adapter
/// only enforces the same-origin message URL and one bounded event.
pub(super) async fn connect(
    endpoint: &str,
    destination: &crate::mcp::policy::VettedDestination,
    client: reqwest13::Client,
    token: Option<String>,
    timeout: Duration,
    max_event_bytes: usize,
) -> Result<(SseTransport, CancellationToken), ApiError> {
    let mut request = client
        .get(destination.url.clone())
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive");
    if let Some(token) = &token {
        request = request.bearer_auth(token);
    }
    let response = tokio::time::timeout(timeout, request.send())
        .await
        .map_err(|_| {
            ApiError::new(
                "mcp.transport_unavailable",
                "legacy MCP SSE handshake exceeded its deadline",
            )
        })?
        .map_err(|_| {
            ApiError::new(
                "mcp.transport_unavailable",
                "legacy MCP SSE connection failed",
            )
        })?;
    if !response.status().is_success() {
        return Err(ApiError::new(
            "mcp.transport_unavailable",
            format!(
                "legacy MCP SSE endpoint returned HTTP {}",
                response.status().as_u16()
            ),
        ));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !content_type.starts_with("text/event-stream") {
        return Err(ApiError::new(
            "mcp.transport_unavailable",
            "legacy MCP endpoint did not return an event stream",
        ));
    }

    let mut stream = response
        .bytes_stream()
        .map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|_| "event stream read failed".into())
        })
        .boxed();
    let mut buffer = Vec::new();
    let first = tokio::time::timeout(
        timeout,
        next_event(&mut stream, &mut buffer, max_event_bytes),
    )
    .await
    .map_err(|_| {
        ApiError::new(
            "mcp.transport_unavailable",
            "legacy MCP SSE endpoint did not announce a message URL",
        )
    })?
    .map_err(|detail| {
        ApiError::new(
            "mcp.transport_unavailable",
            format!("legacy MCP SSE stream failed: {detail}"),
        )
    })?
    .ok_or_else(|| {
        ApiError::new(
            "mcp.transport_unavailable",
            "legacy MCP SSE stream ended before announcing a message URL",
        )
    })?;
    if first.kind != "endpoint" {
        return Err(ApiError::new(
            "mcp.transport_unavailable",
            "legacy MCP SSE stream did not announce a message URL",
        ));
    }
    let message_url = same_origin_message_url(endpoint, &first.data)?;

    let (sender, receiver) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task_sender = sender.clone();
    tokio::spawn(async move {
        let mut pending = Vec::new();
        loop {
            let event = tokio::select! {
                _ = task_cancel.cancelled() => break,
                event = next_event(&mut stream, &mut pending, max_event_bytes) => event,
            };
            let Some(event) = event.unwrap_or(None) else {
                break;
            };
            if event.kind != "message" {
                continue;
            }
            match serde_json::from_str::<ServerJsonRpcMessage>(event.data.trim()) {
                Ok(message) => {
                    if task_sender.send(message).await.is_err() {
                        break;
                    }
                }
                // A malformed event is dropped. The native client observes the
                // request timeout or closed stream instead of a fabricated
                // response.
                Err(_) => continue,
            }
        }
        task_cancel.cancel();
    });

    Ok((
        SseTransport {
            client,
            message_url,
            token,
            sender,
            receiver,
            cancel: cancel.clone(),
        },
        cancel,
    ))
}

/// Accept only a same-origin message URL announced by the vetted endpoint.
fn same_origin_message_url(endpoint: &str, announced: &str) -> Result<Url, ApiError> {
    let base = Url::parse(endpoint).map_err(|_| {
        ApiError::new(
            "mcp.endpoint_denied",
            "legacy MCP endpoint is not a valid URL",
        )
    })?;
    let url = base.join(announced.trim()).map_err(|_| {
        ApiError::new(
            "mcp.endpoint_denied",
            "legacy MCP server announced an invalid message URL",
        )
    })?;
    if url.scheme() != base.scheme()
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != base.port_or_known_default()
    {
        return Err(ApiError::new(
            "mcp.endpoint_denied",
            "legacy MCP server announced a message URL on another origin",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::new(
            "mcp.endpoint_denied",
            "legacy MCP message URL must not embed credentials",
        ));
    }
    Ok(url)
}

/// Read one SSE event, keeping unread bytes for the next call.
async fn next_event(
    stream: &mut (impl Stream<Item = Result<Vec<u8>, String>> + Unpin),
    buffer: &mut Vec<u8>,
    max_bytes: usize,
) -> Result<Option<SseEvent>, String> {
    loop {
        if let Some((end, skip)) = event_boundary(buffer) {
            let block: Vec<u8> = buffer.drain(..end).collect();
            buffer.drain(..skip);
            let event = parse_event(&block);
            if event.kind.is_empty() && event.data.is_empty() {
                continue;
            }
            return Ok(Some(event));
        }
        let Some(chunk) = stream.next().await else {
            if buffer.is_empty() {
                return Ok(None);
            }
            let event = parse_event(buffer);
            buffer.clear();
            return Ok(Some(event));
        };
        buffer.extend_from_slice(&chunk?);
        if buffer.len() > max_bytes {
            return Err("event exceeded the configured size ceiling".into());
        }
    }
}

/// Locate the end of one event block and how many separator bytes follow it.
fn event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let mut index = 0;
    while index < buffer.len() {
        if buffer[index] == b'\n' && buffer.get(index + 1) == Some(&b'\n') {
            return Some((index, 2));
        }
        if buffer[index] == b'\r'
            && buffer.get(index + 1) == Some(&b'\n')
            && buffer.get(index + 2) == Some(&b'\r')
            && buffer.get(index + 3) == Some(&b'\n')
        {
            return Some((index, 4));
        }
        index += 1;
    }
    None
}

fn parse_event(block: &[u8]) -> SseEvent {
    let text = String::from_utf8_lossy(block);
    let mut kind = String::new();
    let mut data = String::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            kind = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    if kind.is_empty() && !data.is_empty() {
        kind = "message".into();
    }
    SseEvent { kind, data }
}

async fn read_bounded(response: Response, limit: usize) -> Result<Vec<u8>, SseError> {
    let mut stream = response.bytes_stream().boxed();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(SseError::new)?;
        if body.len() + chunk.len() > limit {
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_split_on_lf_or_crlf_boundaries() {
        let mut lf = b"event: endpoint\ndata: /messages?id=1\n\n".to_vec();
        let (end, skip) = event_boundary(&lf).expect("boundary");
        let block: Vec<u8> = lf.drain(..end).collect();
        lf.drain(..skip);
        let event = parse_event(&block);
        assert_eq!(event.kind, "endpoint");
        assert_eq!(event.data, "/messages?id=1");

        let crlf = b"event: message\r\ndata: {}\r\n\r\nevent: message\r\ndata: {}\r\n\r\n";
        let (end, skip) = event_boundary(crlf).expect("boundary");
        assert_eq!(skip, 4);
        assert_eq!(&crlf[..end], b"event: message\r\ndata: {}");
    }

    #[test]
    fn multi_line_data_is_joined_and_comments_are_ignored() {
        let event = parse_event(b": keep-alive\nevent: message\ndata: line one\ndata: line two");
        assert_eq!(event.kind, "message");
        assert_eq!(event.data, "line one\nline two");
    }

    #[test]
    fn the_announced_message_url_must_stay_on_the_vetted_origin() {
        let url = same_origin_message_url("http://127.0.0.1:9000/sse", "/messages/?session_id=abc")
            .expect("same origin");
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:9000/messages/?session_id=abc"
        );

        assert_eq!(
            same_origin_message_url("http://127.0.0.1:9000/sse", "http://evil.test/messages")
                .expect_err("other origin")
                .code,
            "mcp.endpoint_denied"
        );
        assert_eq!(
            same_origin_message_url(
                "http://127.0.0.1:9000/sse",
                "http://user:pass@127.0.0.1:9000/messages"
            )
            .expect_err("embedded credentials")
            .code,
            "mcp.endpoint_denied"
        );
    }
}
