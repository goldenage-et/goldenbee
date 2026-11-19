//! Bounded SSE block decoding for socket event forwarding.
//!
//! The Worker remains the only event journal. This decoder only turns the
//! forwarded replay-then-live byte stream into the shared event envelope.

use common::{ApiError, StreamEvent};

/// Decode SSE blocks from forwarded Worker bytes without retaining a journal.
pub(crate) struct EventDecoder {
    buffer: Vec<u8>,
    limit: usize,
}

impl EventDecoder {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            limit: limit.max(1024),
        }
    }

    /// Append one forwarded chunk and return every complete event.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<Vec<StreamEvent>, ApiError> {
        if self.buffer.len().saturating_add(chunk.len()) > self.limit {
            return Err(ApiError::new(
                "socket.subscription_overflow",
                "the forwarded Worker stream exceeded the bounded socket block buffer",
            ));
        }
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(block) = take_block(&mut self.buffer) {
            if let Some(event) = parse_block(&block)? {
                events.push(event);
            }
        }
        Ok(events)
    }
}

fn take_block(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
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

fn parse_block(block: &[u8]) -> Result<Option<StreamEvent>, ApiError> {
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
    serde_json::from_str(data).map(Some).map_err(|_| {
        ApiError::new(
            "socket.invalid_event",
            "the forwarded Worker event did not match the shared envelope",
        )
    })
}
