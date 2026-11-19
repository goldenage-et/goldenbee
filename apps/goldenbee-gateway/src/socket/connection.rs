//! One authenticated duplex connection over existing Gateway behavior.
//!
//! The connection owns command parsing, bounded fan-out, heartbeat, idle
//! detection, and frame limits. Task, stream, cancellation, and approval
//! semantics stay in `gateway-core`.

use super::sse::EventDecoder;
use crate::config::SocketConfig;
use crate::state::SharedState;
use axum::extract::ws::{Message, WebSocket};
use common::{ApiError, SocketCommand, SocketCommandFrame, SocketOutcome, SocketServerFrame};
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use gateway_core::{Caller, EventStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

/// Effective bounds for one socket process.
#[derive(Debug, Clone)]
pub(crate) struct SocketLimits {
    pub(crate) max_frame_bytes: usize,
    pub(crate) subscriber_queue: usize,
    pub(crate) max_subscriptions: usize,
    pub(crate) idle_timeout_ms: u64,
    pub(crate) heartbeat_ms: u64,
    pub(crate) send_timeout_ms: u64,
    pub(crate) ticket_ttl_ms: u64,
}

impl From<SocketConfig> for SocketLimits {
    fn from(config: SocketConfig) -> Self {
        Self {
            max_frame_bytes: config.max_frame_bytes,
            subscriber_queue: config.subscriber_queue,
            max_subscriptions: config.max_subscriptions,
            idle_timeout_ms: config.idle_timeout_ms,
            heartbeat_ms: config.heartbeat_ms,
            send_timeout_ms: config.send_timeout_ms,
            ticket_ttl_ms: config.ticket_ttl_ms,
        }
    }
}

type CloseSignal = watch::Sender<Option<ApiError>>;

pub(crate) async fn run(
    socket: WebSocket,
    state: SharedState,
    caller: Caller,
    limits: SocketLimits,
) {
    let (sink, reader) = socket.split();
    let (outgoing, incoming) = mpsc::channel(limits.subscriber_queue);
    let (close, close_rx) = watch::channel(None);
    let subscriptions = Arc::new(AtomicUsize::new(0));

    let mut writer = tokio::spawn(writer_loop(sink, incoming, close_rx, limits.clone()));
    let reader = reader_loop(
        reader,
        state,
        caller,
        limits.clone(),
        outgoing.clone(),
        close.clone(),
        subscriptions,
    );
    tokio::pin!(reader);
    tokio::select! {
        // A send failure or peer close ends the whole connection instead of
        // leaving a half-open stream behind.
        _ = &mut writer => {}
        reason = &mut reader => {
            let _ = close.send(reason);
            drop(outgoing);
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut writer).await;
        }
    }
}

async fn writer_loop(
    mut sink: SplitSink<WebSocket, Message>,
    mut incoming: mpsc::Receiver<SocketServerFrame>,
    mut close: watch::Receiver<Option<ApiError>>,
    limits: SocketLimits,
) {
    let mut heartbeat = tokio::time::interval(Duration::from_millis(limits.heartbeat_ms));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await;
    loop {
        tokio::select! {
            biased;
            _ = close.changed() => {
                let reason = close.borrow().clone();
                if let Some(error) = reason {
                    let _ = send_frame(&mut sink, &SocketServerFrame::Error {
                        command_id: None,
                        error,
                    }, &limits).await;
                }
                let _ = sink.send(Message::Close(None)).await;
                return;
            }
            _ = heartbeat.tick() => {
                if send_frame(&mut sink, &SocketServerFrame::Heartbeat { at: common::now_ms() }, &limits).await.is_err() {
                    return;
                }
            }
            frame = incoming.recv() => {
                match frame {
                    Some(frame) => {
                        if send_frame(&mut sink, &frame, &limits).await.is_err() {
                            return;
                        }
                    }
                    None => {
                        let _ = sink.send(Message::Close(None)).await;
                        return;
                    }
                }
            }
        }
    }
}

async fn send_frame(
    sink: &mut SplitSink<WebSocket, Message>,
    frame: &SocketServerFrame,
    limits: &SocketLimits,
) -> Result<(), ()> {
    let text = serde_json::to_string(frame).map_err(|_| ())?;
    if text.len() > limits.max_frame_bytes {
        return Err(());
    }
    match tokio::time::timeout(
        Duration::from_millis(limits.send_timeout_ms),
        sink.send(Message::Text(text.into())),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        _ => Err(()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn reader_loop(
    mut reader: SplitStream<WebSocket>,
    state: SharedState,
    caller: Caller,
    limits: SocketLimits,
    outgoing: mpsc::Sender<SocketServerFrame>,
    close: CloseSignal,
    subscriptions: Arc<AtomicUsize>,
) -> Option<ApiError> {
    let mut tasks = JoinSet::new();
    loop {
        let next =
            tokio::time::timeout(Duration::from_millis(limits.idle_timeout_ms), reader.next())
                .await;
        let message = match next {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(_))) | Ok(None) => return None,
            Err(_) => {
                return Some(ApiError::new(
                    "socket.idle_timeout",
                    "the socket was idle beyond its bounded timeout",
                ));
            }
        };
        match message {
            Message::Text(text) => {
                if text.len() > limits.max_frame_bytes {
                    return Some(ApiError::new(
                        "socket.frame_too_large",
                        "the client frame exceeded the bounded socket size",
                    ));
                }
                let command_id = partial_command_id(text.as_str());
                let frame: SocketCommandFrame = match serde_json::from_str(text.as_str()) {
                    Ok(frame) => frame,
                    Err(_) => {
                        if !enqueue(
                            &outgoing,
                            SocketServerFrame::Error {
                                command_id,
                                error: ApiError::new(
                                    "socket.invalid_frame",
                                    "the client frame is not a valid command envelope",
                                ),
                            },
                            &limits,
                            &close,
                        )
                        .await
                        {
                            return Some(slow_consumer());
                        }
                        continue;
                    }
                };
                if let Err(error) = frame.validate() {
                    if !enqueue(
                        &outgoing,
                        SocketServerFrame::Error {
                            command_id: Some(frame.command_id),
                            error,
                        },
                        &limits,
                        &close,
                    )
                    .await
                    {
                        return Some(slow_consumer());
                    }
                    continue;
                }
                let command_id = frame.command_id.clone();
                match frame.command {
                    SocketCommand::Subscribe { task_id, after } => {
                        if subscriptions.load(Ordering::SeqCst) >= limits.max_subscriptions {
                            if !enqueue(
                                &outgoing,
                                SocketServerFrame::Error {
                                    command_id: Some(command_id),
                                    error: ApiError::retryable(
                                        "capacity.socket_subscriptions",
                                        "this connection reached its bounded subscription count",
                                    ),
                                },
                                &limits,
                                &close,
                            )
                            .await
                            {
                                return Some(slow_consumer());
                            }
                            continue;
                        }
                        match state.core.stream(&caller, &task_id, after).await {
                            Ok(stream) => {
                                subscriptions.fetch_add(1, Ordering::SeqCst);
                                if !enqueue(
                                    &outgoing,
                                    SocketServerFrame::Ack {
                                        command_id: command_id.clone(),
                                        outcome: SocketOutcome::Subscribed {
                                            task_id: task_id.clone(),
                                            after,
                                        },
                                    },
                                    &limits,
                                    &close,
                                )
                                .await
                                {
                                    subscriptions.fetch_sub(1, Ordering::SeqCst);
                                    return Some(slow_consumer());
                                }
                                tasks.spawn(forward_events(
                                    stream,
                                    command_id,
                                    task_id,
                                    limits.clone(),
                                    outgoing.clone(),
                                    close.clone(),
                                    subscriptions.clone(),
                                ));
                            }
                            Err(error) => {
                                if !enqueue(
                                    &outgoing,
                                    SocketServerFrame::Error {
                                        command_id: Some(command_id),
                                        error,
                                    },
                                    &limits,
                                    &close,
                                )
                                .await
                                {
                                    return Some(slow_consumer());
                                }
                            }
                        }
                    }
                    SocketCommand::Submit { request } => {
                        let result = state.core.submit(&caller, request).await;
                        let frame = match result {
                            Ok(response) => SocketServerFrame::Ack {
                                command_id: command_id.clone(),
                                outcome: SocketOutcome::Submitted {
                                    response: Box::new(response),
                                },
                            },
                            Err(error) => SocketServerFrame::Error {
                                command_id: Some(command_id),
                                error,
                            },
                        };
                        if !enqueue(&outgoing, frame, &limits, &close).await {
                            return Some(slow_consumer());
                        }
                    }
                    SocketCommand::Cancel { task_id } => {
                        let frame = match state.core.cancel(&caller, &task_id).await {
                            Ok(task) => SocketServerFrame::Ack {
                                command_id: command_id.clone(),
                                outcome: SocketOutcome::Cancelled {
                                    task: Box::new(task),
                                },
                            },
                            Err(error) => SocketServerFrame::Error {
                                command_id: Some(command_id),
                                error,
                            },
                        };
                        if !enqueue(&outgoing, frame, &limits, &close).await {
                            return Some(slow_consumer());
                        }
                    }
                    SocketCommand::Approval {
                        task_id,
                        approval_id,
                        decision,
                    } => {
                        let frame = match state
                            .core
                            .decide_approval(&caller, &task_id, &approval_id, decision)
                            .await
                        {
                            Ok(approval) => SocketServerFrame::Ack {
                                command_id: command_id.clone(),
                                outcome: SocketOutcome::Approval {
                                    approval: Box::new(approval),
                                },
                            },
                            Err(error) => SocketServerFrame::Error {
                                command_id: Some(command_id),
                                error,
                            },
                        };
                        if !enqueue(&outgoing, frame, &limits, &close).await {
                            return Some(slow_consumer());
                        }
                    }
                }
            }
            // The WebSocket layer answers ping/pong liveness; receiving either
            // frame resets the idle deadline.
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Binary(_) => {
                if !enqueue(
                    &outgoing,
                    SocketServerFrame::Error {
                        command_id: None,
                        error: ApiError::new(
                            "socket.invalid_frame",
                            "binary frames are not part of the socket command protocol",
                        ),
                    },
                    &limits,
                    &close,
                )
                .await
                {
                    return Some(slow_consumer());
                }
            }
            Message::Close(_) => return None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn forward_events(
    mut stream: EventStream,
    command_id: String,
    task_id: String,
    limits: SocketLimits,
    outgoing: mpsc::Sender<SocketServerFrame>,
    close: CloseSignal,
    subscriptions: Arc<AtomicUsize>,
) {
    let mut close_rx = close.subscribe();
    let mut decoder = EventDecoder::new(limits.max_frame_bytes.saturating_mul(4).max(1024 * 1024));
    'stream: loop {
        tokio::select! {
            _ = close_rx.changed() => break,
            item = stream.next() => {
                match item {
                    None => break,
                    Some(Err(error)) => {
                        let _ = enqueue(&outgoing, SocketServerFrame::Error {
                            command_id: Some(command_id),
                            error,
                        }, &limits, &close).await;
                        break;
                    }
                    Some(Ok(chunk)) => {
                        let events = match decoder.push(&chunk) {
                            Ok(events) => events,
                            Err(error) => {
                                let _ = enqueue(&outgoing, SocketServerFrame::Error {
                                    command_id: Some(command_id),
                                    error,
                                }, &limits, &close).await;
                                break;
                            }
                        };
                        for event in events {
                            let frame = SocketServerFrame::Event {
                                command_id: command_id.clone(),
                                task_id: task_id.clone(),
                                event,
                            };
                            let size = serde_json::to_string(&frame).map(|text| text.len()).unwrap_or(usize::MAX);
                            if size > limits.max_frame_bytes {
                                let _ = enqueue(&outgoing, SocketServerFrame::Error {
                                    command_id: Some(command_id),
                                    error: ApiError::new(
                                        "socket.frame_too_large",
                                        "the forwarded event exceeded the bounded socket frame",
                                    ),
                                }, &limits, &close).await;
                                break 'stream;
                            }
                            if !enqueue(&outgoing, frame, &limits, &close).await {
                                break 'stream;
                            }
                        }
                    }
                }
            }
        }
    }
    subscriptions.fetch_sub(1, Ordering::SeqCst);
}

async fn enqueue(
    outgoing: &mpsc::Sender<SocketServerFrame>,
    frame: SocketServerFrame,
    limits: &SocketLimits,
    close: &CloseSignal,
) -> bool {
    match tokio::time::timeout(
        Duration::from_millis(limits.send_timeout_ms),
        outgoing.send(frame),
    )
    .await
    {
        Ok(Ok(())) => true,
        _ => {
            let _ = close.send(Some(slow_consumer()));
            false
        }
    }
}

fn slow_consumer() -> ApiError {
    ApiError::retryable(
        "capacity.socket_slow_consumer",
        "the client did not drain the bounded socket queue",
    )
}

fn partial_command_id(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    value
        .get("command_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> SocketLimits {
        SocketLimits {
            max_frame_bytes: 1024,
            subscriber_queue: 1,
            max_subscriptions: 1,
            idle_timeout_ms: 60_000,
            heartbeat_ms: 20_000,
            send_timeout_ms: 10,
            ticket_ttl_ms: 30_000,
        }
    }

    #[tokio::test]
    async fn a_full_subscriber_queue_fails_as_a_bounded_slow_consumer() {
        let (outgoing, mut incoming) = mpsc::channel(1);
        outgoing
            .send(SocketServerFrame::Heartbeat { at: 0 })
            .await
            .expect("fill the only queue slot");
        let (close, closed) = watch::channel(None);
        let accepted = enqueue(
            &outgoing,
            SocketServerFrame::Error {
                command_id: None,
                error: ApiError::new("test", "second frame"),
            },
            &limits(),
            &close,
        )
        .await;
        assert!(!accepted, "a full queue must not block indefinitely");
        assert_eq!(
            closed.borrow().as_ref().map(|error| error.code.as_str()),
            Some("capacity.socket_slow_consumer")
        );
        assert!(incoming.try_recv().is_ok());
    }
}
