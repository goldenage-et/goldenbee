//! Authenticated duplex Gateway socket adapter.
//!
//! The adapter obtains a short-lived one-use ticket through the authenticated
//! HTTP surface, connects with only that ticket in the URL, and carries typed
//! commands over the same task, stream, cancellation, and approval semantics
//! as the HTTP SDK methods. It keeps no event journal and never resubmits work
//! while reconnecting.

use common::{
    ApiError, SocketCommand, SocketCommandFrame, SocketOutcome, SocketScope, SocketServerFrame,
    SocketTicket,
};
use futures::{SinkExt, StreamExt};
use std::collections::VecDeque;
use std::time::Duration;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

/// Maximum buffered non-matching frames while waiting for one command ack.
pub const MAX_SOCKET_PENDING: usize = 256;

/// Reusable socket connector. The bearer token stays in the HTTP ticket request
/// and never appears in the WebSocket URL.
pub struct Socket {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Socket {
    /// Build a socket connector for one Gateway endpoint and caller token.
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        Self {
            base,
            token: token.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Mint one short-lived one-use upgrade ticket through authenticated HTTP.
    pub async fn ticket(&self) -> Result<SocketTicket, ApiError> {
        let response = self
            .http
            .post(format!("{}/v1/socket/tickets", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| {
                ApiError::retryable("sdk.transport", "Gateway socket ticket request failed")
            })?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|_| {
            ApiError::retryable("sdk.transport", "Gateway ticket response was not readable")
        })?;
        if !status.is_success() {
            if let Ok(error) = serde_json::from_slice::<ApiError>(&bytes) {
                return Err(error);
            }
            return Err(ApiError::new(
                "sdk.invalid_response",
                format!("Gateway returned {status} without a typed error"),
            ));
        }
        serde_json::from_slice(&bytes).map_err(|error| {
            ApiError::new(
                "sdk.invalid_response",
                format!("Gateway socket ticket did not match protocol: {error}"),
            )
        })
    }

    /// Redeem one ticket and open a duplex command session.
    pub async fn connect(&self) -> Result<SocketSession, ApiError> {
        let ticket = self.ticket().await?;
        let scheme = if self.base.starts_with("https://") {
            "wss"
        } else {
            "ws"
        };
        let authority = self
            .base
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(self.base.as_str());
        let url = format!(
            "{scheme}://{authority}{}?ticket={}",
            ticket.path, ticket.ticket
        );
        let (stream, _) = connect_async(url).await.map_err(handshake_error)?;
        Ok(SocketSession {
            stream,
            pending: VecDeque::new(),
        })
    }

    /// Open a session and submit one task command.
    pub async fn submit(
        &self,
        command_id: &str,
        request: &common::GatewaySubmitRequest,
    ) -> Result<common::GatewaySubmitResponse, ApiError> {
        let mut session = self.connect().await?;
        match session
            .command(&SocketCommandFrame::new(
                command_id,
                SocketScope {
                    worker: request.worker.clone(),
                    task_id: None,
                },
                SocketCommand::Submit {
                    request: request.clone(),
                },
            ))
            .await?
        {
            SocketOutcome::Submitted { response } => Ok(*response),
            _ => Err(ApiError::new(
                "sdk.socket_protocol",
                "the Gateway acknowledged a different command outcome",
            )),
        }
    }
}

/// One open duplex command session.
pub struct SocketSession {
    stream: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    pending: VecDeque<SocketServerFrame>,
}

impl SocketSession {
    /// Send one typed command frame and wait for its matching acknowledgment or
    /// typed error. Events and heartbeats observed while waiting are retained
    /// for [`Self::next_event`].
    pub async fn command(&mut self, frame: &SocketCommandFrame) -> Result<SocketOutcome, ApiError> {
        let wanted = frame.command_id.clone();
        self.send(frame).await?;
        loop {
            match self.next_frame().await? {
                SocketServerFrame::Ack {
                    command_id,
                    outcome,
                } if command_id == wanted => return Ok(outcome),
                SocketServerFrame::Error {
                    command_id: Some(command_id),
                    error,
                } if command_id == wanted => return Err(error),
                SocketServerFrame::Error {
                    command_id: None,
                    error,
                } => return Err(error),
                other => self.buffer(other)?,
            }
        }
    }

    /// Send one frame without waiting for an acknowledgment.
    pub async fn send(&mut self, frame: &SocketCommandFrame) -> Result<(), ApiError> {
        let text = serde_json::to_string(frame).map_err(|_| {
            ApiError::new("sdk.socket_frame", "the command frame could not be encoded")
        })?;
        self.stream
            .send(Message::Text(text.into()))
            .await
            .map_err(|error| {
                ApiError::retryable("sdk.socket", format!("Gateway socket send failed: {error}"))
            })
    }

    /// Read the next server frame. Heartbeats are returned to the caller.
    pub async fn next_frame(&mut self) -> Result<SocketServerFrame, ApiError> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(frame);
        }
        loop {
            match self.stream.next().await {
                Some(Ok(Message::Text(text))) => {
                    let frame: SocketServerFrame =
                        serde_json::from_str(text.as_str()).map_err(|error| {
                            ApiError::new(
                                "sdk.socket_protocol",
                                format!("Gateway socket frame did not match protocol: {error}"),
                            )
                        })?;
                    if matches!(frame, SocketServerFrame::Heartbeat { .. }) {
                        // Answer the Gateway heartbeat so a quiet subscriber
                        // stays inside the idle bound. A lost pong is safe: the
                        // server closes at the bound and the durable cursor
                        // resumes without resubmitting work.
                        self.stream
                            .send(Message::Ping(Vec::new().into()))
                            .await
                            .map_err(|error| {
                                ApiError::retryable(
                                    "sdk.socket",
                                    format!("Gateway heartbeat reply failed: {error}"),
                                )
                            })?;
                    }
                    return Ok(frame);
                }
                Some(Ok(Message::Ping(_)))
                | Some(Ok(Message::Pong(_)))
                | Some(Ok(Message::Frame(_))) => continue,
                Some(Ok(Message::Close(_))) | None => {
                    return Err(ApiError::new(
                        "sdk.socket_closed",
                        "the Gateway socket closed; resume with a fresh ticket and durable cursor",
                    ));
                }
                Some(Ok(Message::Binary(_))) => {
                    return Err(ApiError::new(
                        "sdk.socket_protocol",
                        "the Gateway sent an unexpected binary frame",
                    ));
                }
                Some(Err(error)) => {
                    return Err(ApiError::retryable(
                        "sdk.socket",
                        format!("Gateway socket failed: {error}"),
                    ));
                }
            }
        }
    }

    /// Read the next subscription event, skipping heartbeats and command
    /// acknowledgments.
    pub async fn next_event(&mut self) -> Result<common::StreamEvent, ApiError> {
        loop {
            match self.next_frame().await? {
                SocketServerFrame::Event { event, .. } => return Ok(event),
                SocketServerFrame::Heartbeat { .. } | SocketServerFrame::Ack { .. } => continue,
                SocketServerFrame::Error { error, .. } => return Err(error),
            }
        }
    }

    fn buffer(&mut self, frame: SocketServerFrame) -> Result<(), ApiError> {
        if self.pending.len() >= MAX_SOCKET_PENDING {
            return Err(ApiError::new(
                "sdk.socket_overflow",
                "the bounded client frame buffer is full; drain subscription events before issuing another command",
            ));
        }
        self.pending.push_back(frame);
        Ok(())
    }
}

fn handshake_error(error: WsError) -> ApiError {
    if let WsError::Http(response) = &error {
        if let Some(body) = response.body() {
            if let Ok(error) = serde_json::from_slice::<ApiError>(body) {
                return error;
            }
        }
        return ApiError::new(
            "sdk.socket_handshake",
            format!(
                "Gateway rejected the socket upgrade with {}",
                response.status()
            ),
        );
    }
    ApiError::retryable(
        "sdk.socket_handshake",
        format!("Gateway socket handshake failed: {error}"),
    )
}
