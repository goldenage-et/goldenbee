//! Replay-then-live task streams and the Server-Sent Event block parser.

use super::transport::{decode_bytes, send};
use super::Client;
use crate::PROTOCOL_VERSION;
use common::{ApiError, StreamEvent};

/// Maximum buffered bytes for one incomplete Server-Sent Event block.
///
/// The cap bounds memory even if a peer never sends the blank line that ends a
/// block. A larger block is an explicit protocol error, not an unbounded
/// allocation.
pub const MAX_STREAM_BUFFER: usize = 1024 * 1024;

impl Client {
    /// Follow a routed task's replay-then-live Server-Sent Event stream.
    ///
    /// Event history is replayed before live delivery. Model token deltas are
    /// marked non-durable and may be missing after a reconnect; terminal and
    /// approval events remain replayable. The returned [`Watch`] suppresses
    /// duplicate sequences and exposes the last durable cursor for reconnects.
    pub async fn watch(&self, task_id: &str, after: u64) -> Result<Watch, ApiError> {
        let response = send(self, || {
            self.stream_http
                .get(format!(
                    "{}/v1/tasks/{task_id}/events/stream?after={after}",
                    self.base
                ))
                .bearer_auth(&self.token)
        })
        .await?;
        let status = response.status();
        if !status.is_success() {
            let bytes = response
                .bytes()
                .await
                .map_err(|error| ApiError::retryable("sdk.transport", error.to_string()))?;
            return Err(match decode_bytes::<serde_json::Value>(status, &bytes) {
                Err(error) => error,
                Ok(_) => ApiError::new(
                    "sdk.invalid_response",
                    format!("Gateway returned {status} without a typed error"),
                ),
            });
        }
        Ok(Watch {
            response,
            buffer: Vec::new(),
            done: false,
            cursor: StreamCursor::new(after),
        })
    }

    /// Follow a task stream from an explicit durable cursor.
    ///
    /// This is the reconnect path: pass the value returned by
    /// [`Watch::last_durable_seq`] after a disconnect or process restart.
    pub async fn watch_from(&self, task_id: &str, after: u64) -> Result<Watch, ApiError> {
        self.watch(task_id, after).await
    }
}

/// A live task stream returned by [`Client::watch`].
pub struct Watch {
    response: reqwest::Response,
    buffer: Vec<u8>,
    done: bool,
    cursor: StreamCursor,
}

/// Per-stream de-duplication and resumable cursor state.
#[derive(Debug, Clone)]
pub(crate) struct StreamCursor {
    stream_id: Option<String>,
    last_seq: u64,
    last_durable_seq: u64,
}

impl StreamCursor {
    pub(crate) fn new(after: u64) -> Self {
        Self {
            stream_id: None,
            last_seq: after,
            last_durable_seq: after,
        }
    }

    /// Highest sequence observed, including lossy ephemeral deltas.
    pub(crate) fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Highest durable sequence observed.
    pub(crate) fn last_durable_seq(&self) -> u64 {
        self.last_durable_seq
    }

    /// Accept one parsed event, dropping duplicate or replayed sequences within
    /// the same declared stream. A foreign stream identity is an explicit
    /// protocol error rather than a silently mixed stream.
    pub(crate) fn accept(&mut self, event: StreamEvent) -> Result<Option<StreamEvent>, ApiError> {
        match &self.stream_id {
            None => self.stream_id = Some(event.stream_id.clone()),
            Some(expected) if expected != &event.stream_id => {
                return Err(ApiError::new(
                    "sdk.stream_foreign",
                    "the stream changed identity; refusing to mix another task's events",
                ));
            }
            _ => {}
        }
        // Durable and ephemeral events share one sequence. Control notices such
        // as `stream.lagged` carry the last observed sequence and do not advance
        // the cursor.
        if event.kind.starts_with("stream.") {
            return Ok(Some(event));
        }
        if event.seq <= self.last_seq {
            return Ok(None);
        }
        self.last_seq = event.seq;
        if event.durable {
            self.last_durable_seq = event.seq;
        }
        Ok(Some(event))
    }
}

impl Watch {
    /// Read the next event. `None` means the stream closed normally.
    pub async fn next(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        loop {
            if let Some(block) = take_block(&mut self.buffer) {
                if let Some(event) = parse_block(&block)? {
                    if let Some(event) = self.cursor.accept(event)? {
                        return Ok(Some(event));
                    }
                }
                continue;
            }
            if self.done {
                if self.buffer.iter().all(u8::is_ascii_whitespace) {
                    return Ok(None);
                }
                let block = std::mem::take(&mut self.buffer);
                if let Some(event) = parse_block(&block)? {
                    return self.cursor.accept(event);
                }
                return Ok(None);
            }
            match self.response.chunk().await {
                Ok(Some(chunk)) => {
                    if self.buffer.len().saturating_add(chunk.len()) > MAX_STREAM_BUFFER {
                        return Err(ApiError::new(
                            "sdk.stream_overflow",
                            "Gateway stream block exceeded the bounded client buffer",
                        ));
                    }
                    self.buffer.extend_from_slice(&chunk);
                }
                Ok(None) => self.done = true,
                Err(error) => {
                    return Err(ApiError::retryable(
                        "sdk.stream",
                        format!("Gateway stream failed: {error}"),
                    ));
                }
            }
        }
    }

    /// Highest sequence observed, including lossy ephemeral deltas.
    pub fn last_seq(&self) -> u64 {
        self.cursor.last_seq()
    }

    /// Highest durable sequence observed. Use this cursor across a process
    /// restart, where ephemeral sequences are not retained.
    pub fn last_durable_seq(&self) -> u64 {
        self.cursor.last_durable_seq()
    }
}

pub(super) fn take_block(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut index = 0;
    while index + 1 < buffer.len() {
        if buffer[index] == b'\n' && buffer[index + 1] == b'\n' {
            let block = buffer[..index].to_vec();
            buffer.drain(..index + 2);
            return Some(block);
        }
        if index + 3 < buffer.len() && &buffer[index..index + 4] == b"\r\n\r\n" {
            let block = buffer[..index].to_vec();
            buffer.drain(..index + 4);
            return Some(block);
        }
        index += 1;
    }
    None
}

pub(super) fn parse_block(block: &[u8]) -> Result<Option<StreamEvent>, ApiError> {
    let text = String::from_utf8_lossy(block);
    let mut data = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            data.push('\n');
        }
    }
    let data = data.trim_end_matches('\n');
    if data.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(data).map(Some).map_err(|error| {
        ApiError::new(
            "sdk.stream_invalid",
            format!("Gateway stream event did not match protocol {PROTOCOL_VERSION}: {error}"),
        )
    })
}
