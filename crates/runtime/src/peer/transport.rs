//! Thin authenticated JSON-RPC transport over the pinned A2A v1 wire types.
//!
//! The pinned `A2aV1Client` cannot attach an `Authorization` header, while this
//! Worker's A2A surface requires an authenticated caller. This adapter carries
//! only the HTTP/SSE transport and bearer header; the message, task, JSON-RPC,
//! and stream shapes are the pinned `a2a_protocol_types` values, and the remote
//! side remains the native `RequestHandler`/`V1Executor`. No second A2A state
//! machine is introduced.

use crate::ApiError;
use a2a_protocol_types::events::StreamResponse;
use a2a_protocol_types::jsonrpc::{JsonRpcError, JsonRpcRequest};
use a2a_protocol_types::{AgentCard, Message, Task};
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const A2A_VERSION: &str = "1.0";

/// Verified peer card plus its public instance identity.
#[derive(Debug, Clone)]
pub struct VerifiedPeer {
    /// Agent card resolved through the pinned client path.
    pub card: AgentCard,
    /// Instance identity reported by `/v1/health`.
    pub instance: String,
    /// Capability ids advertised by the card.
    pub capabilities: Vec<String>,
}

/// One authenticated A2A v1 connection to an authorized peer.
#[derive(Clone)]
pub struct PeerTransport {
    client: reqwest::Client,
    base: String,
    interface: String,
}

impl PeerTransport {
    /// Build a transport for one validated endpoint. Redirects are refused so a
    /// peer card cannot move the connection to another origin.
    pub fn new(base: &str, timeout_ms: u64) -> Result<Self, ApiError> {
        let base = base.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms.max(1)))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                ApiError::new(
                    "peer.unavailable",
                    format!("cannot build peer client: {error}"),
                )
            })?;
        Ok(Self {
            client,
            interface: format!("{base}/a2a"),
            base,
        })
    }

    /// Resolve the public card through the pinned native client and verify the
    /// instance identity on the same origin before returning anything usable.
    pub async fn verify(&self) -> Result<VerifiedPeer, ApiError> {
        let card =
            adk_rust::server::a2a::client::v1_client::A2aV1Client::resolve_agent_card(&self.base)
                .await
                .map_err(|error| peer_client_error("resolve agent card", error))?;
        let interface = card
            .supported_interfaces
            .iter()
            .find(|interface| interface.protocol_binding == "JSONRPC")
            .ok_or_else(|| {
                ApiError::new(
                    "peer.protocol_unsupported",
                    "peer card does not advertise the JSONRPC interface",
                )
            })?;
        if interface.protocol_version != A2A_VERSION {
            return Err(ApiError::new(
                "peer.protocol_unsupported",
                format!(
                    "peer advertises A2A {}, expected {A2A_VERSION}",
                    interface.protocol_version
                ),
            ));
        }
        let advertised = url::Url::parse(&interface.url).map_err(|error| {
            ApiError::new(
                "peer.endpoint_mismatch",
                format!("peer card interface is not a URL: {error}"),
            )
        })?;
        let expected = url::Url::parse(&format!("{}/a2a", self.base)).map_err(|error| {
            ApiError::new(
                "peer.endpoint_invalid",
                format!("invalid endpoint: {error}"),
            )
        })?;
        if advertised.scheme() != expected.scheme()
            || advertised.host_str() != expected.host_str()
            || advertised.port_or_known_default() != expected.port_or_known_default()
            || advertised.path() != expected.path()
        {
            return Err(ApiError::new(
                "peer.endpoint_mismatch",
                "peer card interface does not match the configured endpoint",
            ));
        }
        let health = self
            .client
            .get(format!("{}/v1/health", self.base))
            .send()
            .await
            .map_err(|error| request_error(error, "read peer liveness"))?;
        if !health.status().is_success() {
            return Err(http_error(health.status().as_u16(), "read peer liveness").await);
        }
        let body: Value = health
            .json()
            .await
            .map_err(|error| request_error(error, "parse peer liveness"))?;
        let instance = body
            .get("instance")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ApiError::new(
                    "peer.identity_mismatch",
                    "peer liveness did not report an instance identity",
                )
            })?
            .to_string();
        let capabilities = card
            .skills
            .iter()
            .map(|skill| skill.id.clone())
            .collect::<Vec<_>>();
        Ok(VerifiedPeer {
            card,
            instance,
            capabilities,
        })
    }

    /// Send one message and wait for the authoritative task result.
    pub async fn send_message(&self, token: &str, message: Message) -> Result<Task, ApiError> {
        self.call(token, "SendMessage", json!({ "message": message }))
            .await
    }

    /// Open a native A2A streaming send and forward parsed stream responses.
    ///
    /// The parsing task stops when the response ends or the receiver is dropped,
    /// so a broken local stream never resubmits the objective.
    pub fn stream_message(
        &self,
        token: &str,
        message: Message,
    ) -> Result<mpsc::Receiver<Result<StreamResponse, ApiError>>, ApiError> {
        let (sender, receiver) = mpsc::channel(32);
        let client = self.clone();
        let token = token.to_string();
        tokio::spawn(async move {
            let request = JsonRpcRequest::with_params(
                request_id(),
                "SendStreamingMessage",
                json!({ "message": message }),
            );
            let response = match client
                .client
                .post(&client.interface)
                .header("A2A-Version", A2A_VERSION)
                .header("Accept", "text/event-stream")
                .bearer_auth(token)
                .json(&request)
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    let _ = sender
                        .send(Err(request_error(error, "start peer stream")))
                        .await;
                    return;
                }
            };
            let status = response.status().as_u16();
            if !response.status().is_success() {
                let _ = sender
                    .send(Err(http_error(status, "start peer stream").await))
                    .await;
                return;
            }
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        let _ = sender
                            .send(Err(request_error(error, "read peer stream")))
                            .await;
                        return;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = buffer.find("\n\n") {
                    let event = buffer[..end].to_string();
                    buffer = buffer[end + 2..].to_string();
                    let Some(data) = sse_data(&event) else {
                        continue;
                    };
                    if data.trim().is_empty() {
                        continue;
                    }
                    match parse_stream(&data) {
                        Ok(response) => {
                            if sender.send(Ok(response)).await.is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            let _ = sender.send(Err(error)).await;
                            return;
                        }
                    }
                }
            }
        });
        Ok(receiver)
    }

    /// Fetch the authoritative task snapshot.
    pub async fn get_task(
        &self,
        token: &str,
        task_id: &str,
        history_length: u32,
    ) -> Result<Task, ApiError> {
        self.call(
            token,
            "GetTask",
            json!({ "id": task_id, "historyLength": history_length }),
        )
        .await
    }

    /// List this owner's tasks in one context. Used only for lost-ack recovery.
    pub async fn list_tasks(&self, token: &str, context_id: &str) -> Result<Vec<Task>, ApiError> {
        self.call(
            token,
            "ListTasks",
            json!({ "contextId": context_id, "pageSize": 20 }),
        )
        .await
    }

    /// Cancel the exact remote task id.
    pub async fn cancel_task(&self, token: &str, task_id: &str) -> Result<Task, ApiError> {
        self.call(token, "CancelTask", json!({ "id": task_id }))
            .await
    }

    async fn call<T: DeserializeOwned>(
        &self,
        token: &str,
        method: &str,
        params: Value,
    ) -> Result<T, ApiError> {
        let request = JsonRpcRequest::with_params(request_id(), method, params);
        let response = self
            .client
            .post(&self.interface)
            .header("A2A-Version", A2A_VERSION)
            .bearer_auth(token)
            .json(&request)
            .send()
            .await
            .map_err(|error| request_error(error, method))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| request_error(error, "read peer response"))?;
        if !(200..300).contains(&status) {
            if let Some(error) = parse_error_bytes(&bytes) {
                return Err(rpc_error(error));
            }
            return Err(http_error(status, method).await);
        }
        let envelope: Envelope =
            serde_json::from_slice(&bytes).map_err(|error| invalid_response(method, error))?;
        if let Some(error) = envelope.error {
            return Err(rpc_error(error));
        }
        let result = envelope.result.ok_or_else(|| {
            ApiError::new(
                "peer.invalid_response",
                format!("peer '{method}' response carried neither result nor error"),
            )
        })?;
        serde_json::from_value(result).map_err(|error| invalid_response(method, error))
    }
}

#[derive(serde::Deserialize)]
struct Envelope {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

fn request_id() -> Value {
    Value::String(uuid::Uuid::new_v4().to_string())
}

fn sse_data(event: &str) -> Option<String> {
    let mut data = String::new();
    for line in event.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
    }
    (!data.is_empty()).then_some(data)
}

fn parse_stream(data: &str) -> Result<StreamResponse, ApiError> {
    let envelope: Envelope =
        serde_json::from_str(data).map_err(|error| invalid_response("stream", error))?;
    if let Some(error) = envelope.error {
        return Err(rpc_error(error));
    }
    let result = envelope.result.ok_or_else(|| {
        ApiError::new(
            "peer.invalid_response",
            "peer stream event carried neither result nor error",
        )
    })?;
    serde_json::from_value(result).map_err(|error| invalid_response("stream", error))
}

fn parse_error_bytes(bytes: &[u8]) -> Option<JsonRpcError> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let error = value.get("error")?;
    serde_json::from_value(error.clone()).ok()
}

fn rpc_error(error: JsonRpcError) -> ApiError {
    let message = error.message.to_ascii_lowercase();
    let code = if message.contains("delegation.replayed")
        || message.contains("already used with a different payload")
    {
        "delegation.replayed"
    } else if error.code == -32001 {
        "peer.task_not_found"
    } else if error.code == -32002 {
        "peer.task_not_cancelable"
    } else if error.code == -32602 {
        "peer.request_invalid"
    } else {
        "peer.protocol_error"
    };
    ApiError::new(code, error.message).with("remote_code", error.code.to_string())
}

async fn http_error(status: u16, operation: &str) -> ApiError {
    match status {
        401 => ApiError::new(
            "peer.unauthorized",
            format!("peer rejected delegation authentication during '{operation}'"),
        ),
        403 => ApiError::new(
            "peer.denied",
            format!("peer denied delegation authority during '{operation}'"),
        ),
        429 => ApiError::retryable(
            "peer.saturated",
            format!("peer is saturated and did not accept '{operation}'"),
        ),
        404 => ApiError::new(
            "peer.endpoint_missing",
            format!("peer endpoint is missing the A2A route during '{operation}'"),
        ),
        500..=599 => ApiError::retryable(
            "peer.unavailable",
            format!("peer returned HTTP {status} during '{operation}'"),
        ),
        _ => ApiError::new(
            "peer.request_rejected",
            format!("peer returned HTTP {status} during '{operation}'"),
        ),
    }
    .with("http_status", status.to_string())
}

fn request_error(error: reqwest::Error, operation: &str) -> ApiError {
    let code = if error.is_timeout() || error.is_connect() {
        "peer.unreachable"
    } else {
        "peer.transport_failed"
    };
    ApiError::retryable(
        code,
        format!("peer transport failed during '{operation}': {error}"),
    )
    .with("operation", operation.to_string())
}

fn peer_client_error(operation: &str, error: impl std::fmt::Display) -> ApiError {
    ApiError::retryable("peer.unreachable", format!("{operation} failed: {error}"))
}

fn invalid_response(operation: &str, error: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        "peer.invalid_response",
        format!("peer '{operation}' response is invalid: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_data_joins_continuation_lines() {
        let event = "event: message\ndata: {\"a\":1}\ndata: {\"b\":2}";
        assert_eq!(sse_data(event).as_deref(), Some("{\"a\":1}\n{\"b\":2}"));
    }

    #[test]
    fn replay_errors_are_stable() {
        let error = rpc_error(JsonRpcError {
            code: -32602,
            message: "delegation.replayed: token already used".into(),
            data: None,
        });
        assert_eq!(error.code, "delegation.replayed");
    }

    #[test]
    fn saturated_and_unavailable_are_retryable() {
        let saturated = futures::executor::block_on(http_error(429, "delegate"));
        assert_eq!(saturated.code, "peer.saturated");
        assert!(saturated.retryable);
        let unavailable = futures::executor::block_on(http_error(503, "delegate"));
        assert_eq!(unavailable.code, "peer.unavailable");
        assert!(unavailable.retryable);
        let denied = futures::executor::block_on(http_error(403, "delegate"));
        assert_eq!(denied.code, "peer.denied");
        assert!(!denied.retryable);
    }
}
