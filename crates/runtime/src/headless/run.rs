//! One-shot and JSONL execution over the existing task runtime.
//!
//! This module admits work through the same durable task path as HTTP and
//! executes through the same executor. It only projects that stream into the
//! shared headless wire types.

use super::frames::{self, ItemChange};
use crate::core::crypto::DelegationClaims;
use crate::core::ids::new_id;
use crate::{ApiError, Runtime};
use common::{
    JsonlFrame, Principal, RunRequest, RunResult, StreamEvent, SubmitTaskRequest, Task,
    MAX_EVENT_PAGE,
};
use std::collections::BTreeSet;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

impl Runtime {
    /// Run one task and project it as ordered JSONL frames.
    ///
    /// The task is admitted durably before the first frame is emitted. Frames
    /// come from retained and live task events; the terminal frame carries the
    /// same fields as [`RunResult`]. A wait bound that expires returns the
    /// truthful non-terminal projection with [`common::exit::UNCERTAIN`].
    pub async fn run_headless(
        &self,
        principal: &Principal,
        request: RunRequest,
        emit: &mut (dyn FnMut(&JsonlFrame) -> Result<(), ApiError> + Send),
    ) -> crate::Result<RunResult> {
        self.run_headless_with_claims(principal, request, None, emit)
            .await
    }

    /// Run one task under an optional Gateway delegation.
    ///
    /// Delegated work keeps the same capability and limit narrowing as
    /// `/v1/tasks`; the headless projection cannot widen a delegation.
    pub async fn run_headless_with_claims(
        &self,
        principal: &Principal,
        mut request: RunRequest,
        claims: Option<&DelegationClaims>,
        emit: &mut (dyn FnMut(&JsonlFrame) -> Result<(), ApiError> + Send),
    ) -> crate::Result<RunResult> {
        request.validate()?;
        if let Some(claims) = claims {
            if let Some(agent) = &request.agent {
                if !claims.capabilities.contains(agent) {
                    return Err(ApiError::new(
                        "scope.capability_denied",
                        "delegation does not grant this capability",
                    )
                    .with("capability", agent.clone()));
                }
            }
            request.limits = request.limits.narrower(&claims.limits);
        }
        let session_id = self.headless_session(principal, &request).await?;
        let agent = request.agent.clone().ok_or_else(|| {
            ApiError::new(
                "validation.run_agent",
                "a headless run must name a registered agent",
            )
        })?;
        let submit = SubmitTaskRequest {
            namespace: Some(principal.namespace.clone()),
            agent,
            session_id,
            input: request.input.text.clone(),
            idempotency_key: request
                .idempotency_key
                .clone()
                .unwrap_or_else(|| new_id("run")),
            profile: request.profile.clone(),
            team: None,
            worker: None,
            limits: request.limits.clone(),
        };
        let response = self
            .submit_task_with_images(principal, submit, claims, &request.input.images)
            .await?;
        let task = self
            .task_unscoped(&response.task_id)
            .await
            .ok_or_else(|| ApiError::new("task.not_found", "the admitted task is not visible"))?;
        emit(&JsonlFrame::task_started(&task))?;

        let deadline = request
            .wait_ms
            .map(|wait| tokio::time::Instant::now() + Duration::from_millis(wait));
        // Subscribe before replay so an event committed during the handoff is
        // either replayed or delivered live.
        let mut receiver = self.subscribe();
        let mut cursor = 0u64;
        let mut seen = Seen::default();
        let drain = self.drain_token();
        loop {
            // Drain every retained page above the cursor.
            loop {
                let page = self.events_since(&task.id, cursor, MAX_EVENT_PAGE).await?;
                if page.cursor_expired {
                    // Retention trimmed the head. Continue from the oldest
                    // retained sequence; the terminal projection remains
                    // authoritative and is checked below.
                    cursor = page.oldest_seq.saturating_sub(1);
                    continue;
                }
                if page.events.is_empty() {
                    break;
                }
                cursor = page.next_seq.max(cursor);
                for event in page.events {
                    if let Some(task) = self
                        .apply_headless_event(principal, emit, &mut seen, &event)
                        .await?
                    {
                        return finish(emit, task, None);
                    }
                }
            }
            // The terminal event may have been trimmed by retention. The durable
            // task projection is the source of truth.
            if let Some(task) = self.task_unscoped(&task.id).await {
                if task.state.is_terminal() {
                    return finish(emit, task, None);
                }
            }
            let received = match deadline {
                Some(deadline) => match tokio::time::timeout_at(deadline, receiver.recv()).await {
                    Ok(received) => received,
                    Err(_) => {
                        let task = self.task_unscoped(&task.id).await.ok_or_else(|| {
                            ApiError::new("task.not_found", "the running task is not visible")
                        })?;
                        return finish(
                            emit,
                            task,
                            Some(
                                "the run did not reach a terminal state within the requested wait window"
                                    .into(),
                            ),
                        );
                    }
                },
                None => tokio::select! {
                    received = receiver.recv() => received,
                    _ = drain.cancelled() => Err(RecvError::Closed),
                },
            };
            match received {
                Ok(event) => {
                    if event.stream_id != format!("task:{}", task.id) || event.seq <= cursor {
                        continue;
                    }
                    cursor = event.seq;
                    if let Some(task) = self
                        .apply_headless_event(principal, emit, &mut seen, &event)
                        .await?
                    {
                        return finish(emit, task, None);
                    }
                }
                // The durable stream above is the recovery path for a lagging
                // local reader.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => {
                    let task = self.task_unscoped(&task.id).await.ok_or_else(|| {
                        ApiError::new("task.not_found", "the running task is not visible")
                    })?;
                    return finish(
                        emit,
                        task,
                        Some("the live task stream closed before a terminal outcome".into()),
                    );
                }
            }
        }
    }

    /// Resolve `--continue`, `--resume`, or an explicit session identity under
    /// the same scope and namespace rules as the HTTP session routes.
    async fn headless_session(
        &self,
        principal: &Principal,
        request: &RunRequest,
    ) -> crate::Result<String> {
        if request.continue_session {
            return Ok(self.latest_session(principal).await?.session_id);
        }
        if let Some(session) = &request.resume_session {
            return Ok(self.session_view(principal, session).await?.session_id);
        }
        Ok(request
            .session_id
            .clone()
            .unwrap_or_else(|| new_id("session")))
    }

    /// Apply one event to the JSONL projection and report a terminal task.
    async fn apply_headless_event(
        &self,
        principal: &Principal,
        emit: &mut (dyn FnMut(&JsonlFrame) -> Result<(), ApiError> + Send),
        seen: &mut Seen,
        event: &StreamEvent,
    ) -> crate::Result<Option<Task>> {
        if is_terminal(&event.kind) {
            let task = self
                .task_unscoped(stream_task(&event.stream_id))
                .await
                .ok_or_else(|| ApiError::new("task.not_found", "the task is not visible"))?;
            return Ok(Some(task));
        }
        if event.kind == "task.running" {
            let attempt = event
                .payload
                .get("attempt")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32;
            if seen.attempts.insert(attempt) {
                emit(&JsonlFrame::TurnStarted {
                    task_id: stream_task(&event.stream_id).to_string(),
                    attempt,
                    seq: event.seq,
                    at: event.at,
                })?;
            }
            return Ok(None);
        }
        let Some((change, item)) = frames::project(self, principal, event).await else {
            return Ok(None);
        };
        if !seen.accept(change, item.id()) {
            return Ok(None);
        }
        let task_id = stream_task(&event.stream_id).to_string();
        let frame = match change {
            ItemChange::Started => JsonlFrame::ItemStarted {
                task_id,
                seq: event.seq,
                at: event.at,
                item,
            },
            ItemChange::Updated => JsonlFrame::ItemUpdated {
                task_id,
                seq: event.seq,
                at: event.at,
                item,
            },
            ItemChange::Completed => JsonlFrame::ItemCompleted {
                task_id,
                seq: event.seq,
                at: event.at,
                item,
            },
        };
        emit(&frame)?;
        Ok(None)
    }
}

/// Deduplication for events that both the native runner and the tool kernel
/// can emit for one call, and for a terminal frame that is written once.
#[derive(Default)]
struct Seen {
    started: BTreeSet<String>,
    completed: BTreeSet<String>,
    attempts: BTreeSet<u32>,
}

impl Seen {
    fn accept(&mut self, change: ItemChange, id: &str) -> bool {
        match change {
            ItemChange::Started => self.started.insert(id.to_string()),
            ItemChange::Completed => self.completed.insert(id.to_string()),
            ItemChange::Updated => true,
        }
    }
}

fn finish(
    emit: &mut (dyn FnMut(&JsonlFrame) -> Result<(), ApiError> + Send),
    task: Task,
    uncertainty: Option<String>,
) -> crate::Result<RunResult> {
    let mut result = RunResult::from_task(&task);
    if uncertainty.is_some() && result.uncertainty.is_none() {
        result.uncertainty = uncertainty;
        result.exit_code = common::exit_code(result.error.as_ref(), result.state);
    }
    emit(&JsonlFrame::task_completed(&result))?;
    Ok(result)
}

fn is_terminal(kind: &str) -> bool {
    matches!(
        kind,
        "task.succeeded" | "task.failed" | "task.cancelled" | "task.unknown" | "task.recovering"
    )
}

fn stream_task(stream_id: &str) -> &str {
    stream_id.strip_prefix("task:").unwrap_or(stream_id)
}
