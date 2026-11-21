//! Queueing, execution permits, and terminal task outcomes.

use crate::core::ids::{millis, now};
use crate::{ApiError, ExecutionOutcome, Runtime};
use common::{Task, TaskState, Usage};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

impl Runtime {
    pub(super) async fn run_task(&self, task_id: String) {
        let Some(task) = self.task_unscoped(&task_id).await else {
            return;
        };
        if task.state.is_terminal() {
            return;
        }
        // Reuse the cancellation token registered at admission when present, so
        // the before_task hook and the executing attempt share one token.
        // Queued and awaiting work can still be cancelled precisely.
        let cancel = self.cancel_for(&task_id).await;
        let pause = self.register_pause(&task_id).await;
        let Some(task) = self.task_unscoped(&task_id).await else {
            self.unregister_cancel(&task_id).await;
            return;
        };
        if task.state.is_terminal() {
            self.unregister_cancel(&task_id).await;
            return;
        }
        let attempt = self
            .store()
            .write(|state| {
                let Some(task) = state.tasks.get_mut(&task_id) else {
                    return Ok(None);
                };
                if task.state.is_terminal() || task.state == TaskState::Cancelling {
                    return Ok(None);
                }
                task.state = TaskState::Queued;
                task.attempt += 1;
                task.updated_at = now();
                Ok(Some(task.attempt))
            })
            .await
            .ok()
            .flatten();
        let Some(attempt) = attempt else {
            self.unregister_cancel(&task_id).await;
            return;
        };
        let _ = self
            .append_event(
                &task_id,
                "task.queued",
                serde_json::json!({"attempt": attempt}),
                true,
            )
            .await;

        self.queued_inc();
        // Acquire the per-identity writing lock before an execution slot. A
        // task waiting for session consistency therefore does not retain an
        // active execution permit.
        let lock = self
            .session_lock(&task.namespace, &task.caller, &task.session_id)
            .await;
        let session_guard = tokio::select! {
            _ = cancel.cancelled() => {
                self.queued_dec();
                self.finish_if_running(&task_id, ExecutionOutcome::cancelled()).await;
                self.unregister_cancel(&task_id).await;
                return;
            }
            guard = tokio::time::timeout(Duration::from_secs(20), lock.lock()) => match guard {
                Ok(guard) => guard,
                Err(_) => {
                    self.queued_dec();
                    self.finish_if_running(
                        &task_id,
                        ExecutionOutcome::failure(ApiError::new(
                            "session.write_conflict",
                            "another writing turn holds this session identity",
                        )),
                    )
                    .await;
                    self.unregister_cancel(&task_id).await;
                    return;
                }
            },
        };
        let after_lock = self.task_unscoped(&task_id).await.map(|task| task.state);
        if after_lock.is_none_or(TaskState::is_terminal) {
            self.queued_dec();
            drop(session_guard);
            self.unregister_cancel(&task_id).await;
            return;
        }

        let permit = tokio::select! {
            _ = cancel.cancelled() => {
                self.queued_dec();
                drop(session_guard);
                self.finish_if_running(&task_id, ExecutionOutcome::cancelled()).await;
                self.unregister_cancel(&task_id).await;
                return;
            }
            permit = tokio::time::timeout(
                Duration::from_secs(300),
                self.slots().acquire_owned(),
            ) => permit,
        };
        let permit = match permit {
            Ok(Ok(permit)) => permit,
            _ => {
                self.queued_dec();
                drop(session_guard);
                self.finish_if_running(
                    &task_id,
                    ExecutionOutcome::failure(ApiError::retryable(
                        "capacity.admission_timeout",
                        "no execution slot became available",
                    )),
                )
                .await;
                self.unregister_cancel(&task_id).await;
                return;
            }
        };
        self.queued_dec();

        let current = self.task_unscoped(&task_id).await.map(|task| task.state);
        if current.is_none_or(TaskState::is_terminal) {
            drop(permit);
            drop(session_guard);
            self.unregister_cancel(&task_id).await;
            return;
        }
        if current == Some(TaskState::Cancelling) {
            drop(permit);
            drop(session_guard);
            self.finish_if_running(&task_id, ExecutionOutcome::cancelled())
                .await;
            self.unregister_cancel(&task_id).await;
            return;
        }
        if cancel.is_cancelled() {
            drop(permit);
            drop(session_guard);
            self.finish_if_running(&task_id, ExecutionOutcome::cancelled())
                .await;
            self.unregister_cancel(&task_id).await;
            return;
        }
        let started = self
            .store()
            .write(|state| {
                let Some(task) = state.tasks.get_mut(&task_id) else {
                    return Ok(false);
                };
                if task.state.is_terminal() || task.state == TaskState::Cancelling {
                    return Ok(false);
                }
                task.state = TaskState::Running;
                task.updated_at = now();
                Ok(true)
            })
            .await
            .unwrap_or(false);
        if !started {
            drop(permit);
            drop(session_guard);
            self.finish_if_running(&task_id, ExecutionOutcome::cancelled())
                .await;
            self.unregister_cancel(&task_id).await;
            return;
        }
        self.active_inc();
        let _ = self
            .append_event(
                &task_id,
                "task.running",
                serde_json::json!({"attempt": attempt}),
                true,
            )
            .await;
        crate::core::fault::exit_if(crate::core::fault::Point::AfterReservation);

        // Hand the executor the projection that includes this attempt number,
        // so native provenance and artifact metadata bind the real attempt.
        let caller = task.caller.clone();
        let Some(task) = self.task_unscoped(&task_id).await else {
            self.release(&task_id, Some(permit), &caller).await;
            return;
        };
        let executor = self.executor_for(task.asset_revision).await;
        let (outcome, permit) = match executor {
            Some(executor) => {
                crate::core::fault::exit_if(crate::core::fault::Point::BeforeNativeStart);
                let future = executor.execute(self.clone(), task.clone(), cancel.clone());
                self.run_with_slot_lease(&task, cancel.clone(), pause, permit, future)
                    .await
            }
            None => (
                ExecutionOutcome::failure(ApiError::new(
                    "execution.unavailable",
                    "no native executor is configured in this process",
                )),
                Some(permit),
            ),
        };
        self.finish_with_hook(&task, &cancel, outcome).await;
        drop(session_guard);
        self.release(&task_id, permit, &task.caller).await;
    }

    /// Commit the terminal outcome before the after-task hook observes it.
    async fn finish_with_hook(
        &self,
        task: &Task,
        cancel: &CancellationToken,
        outcome: ExecutionOutcome,
    ) {
        let committed = outcome.clone();
        self.finish_task(&task.id, outcome).await;
        let hook_ctx = crate::worker::TaskHookContext {
            task_id: task.id.clone(),
            namespace: task.namespace.clone(),
            caller: task.caller.clone(),
            agent: task.agent.clone(),
            session_id: task.session_id.clone(),
            profile: task.profile.clone(),
            profile_revision: task.profile_revision,
            asset_revision: task.asset_revision,
            input: task.input.clone(),
            settings: self.settings().await,
            cancel: cancel.clone(),
        };
        let state = match committed.state {
            Some(TaskState::Accepted) => "accepted",
            Some(TaskState::Queued) => "queued",
            Some(TaskState::Running) => "running",
            Some(TaskState::WaitingApproval) => "waiting_approval",
            Some(TaskState::Cancelling) => "cancelling",
            Some(TaskState::Succeeded) => "succeeded",
            Some(TaskState::Failed) => "failed",
            Some(TaskState::Cancelled) => "cancelled",
            Some(TaskState::Recovering) => "recovering",
            Some(TaskState::Unknown) => "unknown",
            None if committed.error.is_some() => "failed",
            None => "succeeded",
        };
        self.stop_hook(&crate::worker::StopHookContext {
            task: hook_ctx.clone(),
            state: state.to_string(),
            error_code: committed.error.as_ref().map(|error| error.code.clone()),
            uncertainty: committed.uncertainty.clone(),
        })
        .await;
        if let Some(error) = self.after_task_hook(&hook_ctx, &committed).await {
            let _ = self
                .append_event(
                    &task.id,
                    "hook.after_task_failed",
                    serde_json::json!({
                        "code": error.code,
                        "message": common::redact(&error.message),
                    }),
                    true,
                )
                .await;
        }
    }

    /// Hold an execution permit only while the native attempt is executing.
    ///
    /// A task paused in `waiting_approval` releases its permit before the
    /// caller is notified, and reacquires one when the decision returns it to
    /// `running`. The per-session writing lock is retained across the pause so
    /// same-session turns still serialize.
    async fn run_with_slot_lease(
        &self,
        task: &Task,
        cancel: CancellationToken,
        pause: std::sync::Arc<std::sync::atomic::AtomicU8>,
        permit: tokio::sync::OwnedSemaphorePermit,
        future: impl std::future::Future<Output = ExecutionOutcome>,
    ) -> (ExecutionOutcome, Option<tokio::sync::OwnedSemaphorePermit>) {
        let mut permit = Some(permit);
        let mut execution = Box::pin(future);
        let deadline = (task.limits.max_duration_ms > 0)
            .then(|| tokio::time::Instant::now() + millis(task.limits.max_duration_ms));
        let mut poll = tokio::time::interval(Duration::from_millis(50));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let timeout = async {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::pin!(timeout);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return (ExecutionOutcome::cancelled(), permit);
                }
                outcome = &mut execution => {
                    return (outcome, permit);
                }
                _ = self.approval_notify().notified() => {}
                _ = poll.tick() => {}
                _ = &mut timeout => {
                    let stop = crate::execution::budget::BudgetStop {
                        kind: crate::execution::budget::BudgetKind::Deadline,
                        limit: task.limits.max_duration_ms,
                        used: task.limits.max_duration_ms,
                        exact_preemptive: true,
                        uncertainty:
                            "the attempt was stopped at its wall-clock deadline; completed work remains in durable events"
                                .into(),
                    };
                    return (
                        ExecutionOutcome {
                            output: None,
                            usage: Usage::default(),
                            error: Some(stop.error()),
                            state: Some(TaskState::Failed),
                            native_invocation_id: None,
                            native_thread_id: None,
                            native_a2a_task_id: None,
                            uncertainty: Some(stop.uncertainty),
                        },
                        permit,
                    );
                }
            }
            match pause.load(std::sync::atomic::Ordering::Relaxed) {
                crate::PAUSE_WAITING => {
                    if let Some(active) = permit.take() {
                        self.active_dec();
                        drop(active);
                    }
                }
                crate::PAUSE_RUNNING if permit.is_none() => {
                    let acquired = tokio::select! {
                        _ = cancel.cancelled() => None,
                        result = tokio::time::timeout(
                            Duration::from_secs(300),
                            self.slots().acquire_owned(),
                        ) => match result {
                            Ok(Ok(active)) => Some(active),
                            _ => None,
                        },
                    };
                    match acquired {
                        Some(active) => {
                            permit = Some(active);
                            self.active_inc();
                        }
                        None if cancel.is_cancelled() => {
                            return (ExecutionOutcome::cancelled(), None);
                        }
                        None => {
                            return (
                                ExecutionOutcome::failure(ApiError::retryable(
                                    "capacity.admission_timeout",
                                    "no execution slot became available after resume",
                                )),
                                None,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    async fn finish_if_running(&self, task_id: &str, outcome: ExecutionOutcome) {
        let terminal = self
            .task_unscoped(task_id)
            .await
            .is_none_or(|task| task.state.is_terminal());
        if !terminal {
            self.finish_task(task_id, outcome).await;
        }
    }

    pub(super) async fn finish_task(&self, task_id: &str, outcome: ExecutionOutcome) {
        let mut outcome = outcome;
        // Provider-confirmed values stay untouched. Missing counts gain a
        // separate estimate, and cost is derived only from operator-declared
        // pricing. This read also observes a policy fallback that ran while the
        // attempt was active.
        let snapshot = self.task_unscoped(task_id).await;
        if let Some(snapshot) = &snapshot {
            crate::execution::budget::finalize_usage(
                &mut outcome.usage,
                &snapshot.input,
                outcome.output.as_deref(),
                snapshot.resolved_model.as_ref(),
            );
        }
        let mut state = outcome.state.unwrap_or_else(|| {
            if outcome.error.is_some() {
                TaskState::Failed
            } else {
                TaskState::Succeeded
            }
        });
        let mut failure = outcome.error.clone();
        let mut uncertainty = outcome.uncertainty.clone();
        // A protected effect decides the truthful terminal projection. An
        // unresolved effect is never a success or a definite failure; a known
        // failed or absent effect cannot leave a task looking successful.
        let effect_terminal = self.effect_terminal(task_id).await;
        let effect_unknown =
            effect_terminal == Some(crate::execution::operations::EffectTerminal::Unknown);
        match effect_terminal {
            Some(crate::execution::operations::EffectTerminal::Unknown) => {
                state = TaskState::Unknown;
                uncertainty = Some(
                    "a protected external effect outcome could not be established; reconcile or review before retrying"
                        .into(),
                );
            }
            Some(crate::execution::operations::EffectTerminal::Failed)
                if state == TaskState::Succeeded =>
            {
                state = TaskState::Failed;
                failure = Some(ApiError::new(
                    "effect.failed",
                    "the external effect reached a definite failure",
                ));
            }
            Some(crate::execution::operations::EffectTerminal::Absent)
                if state == TaskState::Succeeded =>
            {
                state = TaskState::Failed;
                failure = Some(ApiError::new(
                    "effect.absent",
                    "the external effect was confirmed absent and was not applied",
                ));
            }
            _ => {}
        }
        // Commit the budget policy evidence in the same metadata transaction as
        // the terminal projection, so a caller that observes the terminal state
        // can always replay the policy event from durable history.
        let policy = snapshot.as_ref().and_then(|snapshot| {
            if let Some(error) = &failure {
                crate::execution::budget::stop_from_error(error, &snapshot.limits, &outcome.usage)
                    .map(|stop| {
                        (
                            crate::execution::budget::POLICY_EVENT,
                            stop.event_payload(
                                &snapshot.limits,
                                &outcome.usage,
                                outcome.output.is_some(),
                            ),
                        )
                    })
            } else {
                crate::execution::budget::usage_disclosure(&snapshot.limits, &outcome.usage)
                    .map(|payload| (crate::execution::budget::USAGE_UNAVAILABLE_EVENT, payload))
            }
        });
        let source = self.config().instance_id.clone();
        let incarnation = self.incarnation().to_string();
        let retention = self.config().event_retention.max(1);
        let forced = self.forced_stop();
        let policy_seq = if policy.is_some() {
            Some(self.next_seq(task_id).await)
        } else {
            None
        };
        let terminal_seq = self.next_seq(task_id).await;
        // Reserve the parent notification sequence before the commit so the
        // child's terminal projection and the parent's `subagent.terminal`
        // event land in the same metadata transaction. Notification therefore
        // happens exactly once per terminal transition.
        let parent_terminal = match snapshot
            .as_ref()
            .and_then(|task| task.parent_task_id.clone())
        {
            Some(parent) => {
                let seq = self.next_seq(&parent).await;
                Some((parent, seq))
            }
            None => None,
        };
        let updated = self
            .store()
            .write(|state_map| {
                let (updated_task, changed) = {
                    let task = state_map
                        .tasks
                        .get_mut(task_id)
                        .ok_or_else(|| ApiError::new("task.not_found", "task disappeared"))?;
                    // A forced stop is not a proven success or failure. Active
                    // native attempts become explicit uncertainty. Work that never
                    // started is left exactly as accepted or queued so restart
                    // recovery can make the truthful decision.
                    let forced_pending =
                        forced && state == TaskState::Cancelled && outcome.error.is_none();
                    if forced_pending
                        && matches!(task.state, TaskState::Accepted | TaskState::Queued)
                    {
                        return Ok((task.clone(), Vec::new(), false));
                    }
                    if forced_pending
                        && matches!(
                            task.state,
                            TaskState::Running | TaskState::WaitingApproval | TaskState::Cancelling
                        )
                    {
                        task.state = if effect_unknown {
                            TaskState::Unknown
                        } else {
                            TaskState::Recovering
                        };
                        task.output = None;
                        task.error = None;
                        task.uncertainty = uncertainty.clone().or_else(|| {
                            Some("forced shutdown before the native outcome was confirmed".into())
                        });
                    } else {
                        task.state = state;
                        task.output = outcome.output.clone();
                        task.error = failure.clone();
                        task.uncertainty = uncertainty.clone();
                        task.usage = outcome.usage.clone();
                        if let Some(id) = outcome.native_invocation_id.clone() {
                            task.native_invocation_id = Some(id);
                        }
                        if let Some(id) = outcome.native_thread_id.clone() {
                            task.native_thread_id = Some(id);
                        }
                        if let Some(id) = outcome.native_a2a_task_id.clone() {
                            task.native_a2a_task_id = Some(id);
                        }
                    }
                    task.updated_at = now();
                    (task.clone(), true)
                };
                if !changed {
                    return Ok((updated_task, Vec::new(), false));
                }
                if let Some((kind, payload)) = policy.clone() {
                    crate::tasks::events::push_event(
                        state_map,
                        &source,
                        &incarnation,
                        retention,
                        &self.redactor(),
                        task_id,
                        policy_seq.expect("reserved policy sequence"),
                        kind,
                        payload,
                        true,
                    )?;
                }
                // Commit the terminal projection and its replayable event in
                // one transaction. A caller that observes a terminal task can
                // therefore always replay the matching terminal event.
                let payload = serde_json::json!({
                    "state": updated_task.state,
                    "output": updated_task.output,
                    "error": updated_task.error,
                    "uncertainty": updated_task.uncertainty,
                    "usage": updated_task.usage,
                });
                let kind = match updated_task.state {
                    TaskState::Succeeded => "task.succeeded",
                    TaskState::Cancelled => "task.cancelled",
                    TaskState::Unknown => "task.unknown",
                    TaskState::Recovering => "task.recovering",
                    TaskState::Failed => "task.failed",
                    _ => "task.updated",
                };
                let mut events = vec![crate::tasks::events::push_event(
                    state_map,
                    &source,
                    &incarnation,
                    retention,
                    &self.redactor(),
                    task_id,
                    terminal_seq,
                    kind,
                    payload,
                    true,
                )?];
                if let Some((parent_id, seq)) = &parent_terminal {
                    if updated_task.parent_task_id.as_deref() == Some(parent_id.as_str())
                        && state_map.tasks.contains_key(parent_id)
                    {
                        let payload = Self::subagent_terminal_payload(&updated_task);
                        // A parent stream conflict must never turn the child's
                        // committed terminal projection into a failed write.
                        if let Ok(event) = crate::tasks::events::push_event(
                            state_map,
                            &source,
                            &incarnation,
                            retention,
                            &self.redactor(),
                            parent_id,
                            *seq,
                            "subagent.terminal",
                            payload,
                            true,
                        ) {
                            events.push(event);
                        }
                    }
                }
                Ok((updated_task, events, true))
            })
            .await;
        let Ok((task, events, changed)) = updated else {
            return;
        };
        if !changed {
            return;
        }
        for event in events {
            let _ = self.live_sender().send(event);
        }
        if task.state == TaskState::Recovering {
            let _ = self
                .append_event(
                    task_id,
                    "task.recovery_required",
                    serde_json::json!({"reason": "forced_shutdown"}),
                    true,
                )
                .await;
        }
        let state_name = match task.state {
            TaskState::Succeeded => "succeeded",
            TaskState::Cancelled => "cancelled",
            TaskState::Unknown => "unknown",
            TaskState::Recovering => "recovering",
            _ => "failed",
        };
        let _ = self
            .audit(
                &task.caller,
                "task.finish",
                task_id,
                state_name,
                task.error
                    .as_ref()
                    .map(|error| error.code.as_str())
                    .unwrap_or(""),
            )
            .await;
        // A runtime-local child has exactly one stop notification after its
        // terminal projection is durable, regardless of how it terminated.
        if task.state.is_terminal() {
            if let Some(parent_id) = task.parent_task_id.clone() {
                let _ = self
                    .notify_subagent_stop(
                        &parent_id,
                        &task.id,
                        &task.agent,
                        &task.session_id,
                        state_name,
                    )
                    .await;
            }
        }
        // A lineage that did not succeed must not leave owned work running.
        // A successful parent may leave background children running; their
        // completion still reports through the same durable notification.
        if task.state != TaskState::Succeeded {
            self.cancel_owned_subagents(&task.id).await;
        }
    }

    async fn release(
        &self,
        task_id: &str,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
        _caller: &str,
    ) {
        if let Some(permit) = permit {
            self.active_dec();
            drop(permit);
        }
        self.unregister_cancel(task_id).await;
    }
}
