//! Durable task event streams with replay and explicit cursor expiry.

use crate::{new_id, now, Runtime, State};
use common::{ApiError, StreamEvent, StreamPage, EVENT_SCHEMA_VERSION};
use serde_json::Value;

impl Runtime {
    /// Append one event to a task stream. Durable events are retained for the
    /// configured window. Ephemeral events are broadcast only and never touch
    /// the state file; they are not replayable.
    pub async fn append_event(
        &self,
        task_id: &str,
        kind: &str,
        payload: Value,
        durable: bool,
    ) -> Result<StreamEvent, ApiError> {
        let exists = self
            .store()
            .read(|state| state.tasks.contains_key(task_id))
            .await;
        if !exists {
            return Err(ApiError::new(
                "task.not_found",
                "task stream does not exist",
            ));
        }
        let seq = self.next_seq(task_id).await;
        let trace_id = self
            .store()
            .read(|state| state.tasks.get(task_id).map(|task| task.trace_id.clone()))
            .await;
        let event = StreamEvent {
            event_id: new_id("evt"),
            source: self.config().instance_id.clone(),
            incarnation: self.incarnation().to_string(),
            stream_id: format!("task:{task_id}"),
            seq,
            schema_version: EVENT_SCHEMA_VERSION,
            kind: kind.to_string(),
            durable,
            payload: redact_json(payload),
            trace_id,
            at: now(),
        };
        if durable {
            let retention = self.config().event_retention.max(1);
            let stored = event.clone();
            self.store()
                .write(|state| {
                    let stream = state.events.entry(task_id.to_string()).or_default();
                    stream.push(stored);
                    if stream.len() > retention {
                        let remove = stream.len() - retention;
                        stream.drain(0..remove);
                    }
                    Ok(())
                })
                .await?;
        }
        let _ = self.live_sender().send(event.clone());
        Ok(event)
    }

    /// Read retained events after a cursor. Ephemeral events are not retained.
    pub async fn events_since(
        &self,
        task_id: &str,
        after: u64,
        limit: usize,
    ) -> Result<StreamPage, ApiError> {
        let retention = self.config().event_retention.max(1);
        self.store()
            .read(|state| {
                let stream = state.events.get(task_id).ok_or_else(|| {
                    ApiError::new("stream.not_found", "task stream is not visible")
                })?;
                let oldest = stream.first().map(|event| event.seq).unwrap_or(1);
                let cursor_expired =
                    after > 0 && after.saturating_add(1) < oldest && !stream.is_empty();
                let events: Vec<_> = stream
                    .iter()
                    .filter(|event| event.seq > after)
                    .take(limit.clamp(1, retention))
                    .cloned()
                    .collect();
                let next_seq = events.last().map(|event| event.seq).unwrap_or(after);
                Ok(StreamPage {
                    stream_id: format!("task:{task_id}"),
                    events,
                    next_seq,
                    oldest_seq: oldest,
                    cursor_expired,
                })
            })
            .await
    }
}

/// Append an event inside an existing metadata transaction, so task acceptance
/// and its first durable event commit or fail together.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_event(
    state: &mut State,
    source: &str,
    incarnation: &str,
    retention: usize,
    task_id: &str,
    seq: u64,
    kind: &str,
    payload: Value,
    durable: bool,
) -> Result<StreamEvent, ApiError> {
    if !state.tasks.contains_key(task_id) {
        return Err(ApiError::new(
            "task.not_found",
            "task stream does not exist",
        ));
    }
    let trace_id = state
        .tasks
        .get(task_id)
        .map(|task| task.trace_id.clone())
        .filter(|trace| !trace.is_empty());
    let stream = state.events.entry(task_id.to_string()).or_default();
    if stream.iter().any(|stored| stored.seq == seq) {
        return Err(ApiError::new(
            "stream.sequence_conflict",
            "task stream already contains this sequence",
        ));
    }
    let event = StreamEvent {
        event_id: new_id("evt"),
        source: source.to_string(),
        incarnation: incarnation.to_string(),
        stream_id: format!("task:{task_id}"),
        seq,
        schema_version: EVENT_SCHEMA_VERSION,
        kind: kind.to_string(),
        durable,
        payload: redact_json(payload),
        trace_id,
        at: now(),
    };
    let position = stream.partition_point(|stored| stored.seq < seq);
    stream.insert(position, event.clone());
    if stream.len() > retention.max(1) {
        let remove = stream.len() - retention.max(1);
        stream.drain(0..remove);
    }
    Ok(event)
}

fn redact_json(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(common::redact(&text)),
        Value::Array(values) => Value::Array(values.into_iter().map(redact_json).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    // Numeric accounting fields are evidence, not credentials.
                    // Keep them visible while still redacting secret-bearing
                    // keys such as `token`, `access_token`, and `password`.
                    let accounting = matches!(
                        lower.as_str(),
                        "prompt_tokens"
                            | "candidate_tokens"
                            | "estimated_tokens"
                            | "max_tokens"
                            | "tokens_used"
                    );
                    if !accounting
                        && (lower.contains("secret")
                            || lower.contains("token")
                            || lower.contains("password"))
                    {
                        (key, Value::String("[REDACTED]".into()))
                    } else {
                        (key, redact_json(value))
                    }
                })
                .collect(),
        ),
        other => other,
    }
}
