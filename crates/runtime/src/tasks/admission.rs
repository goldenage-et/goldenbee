//! Durable admission shared by public submission and runtime-local subagents.
//!
//! There is exactly one durable acceptance transaction. A subagent is an
//! ordinary task admitted through it with a frozen [`AdmitRequest`] that
//! carries parent/root linkage, the pinned parent profile revision, and the
//! narrowed subagent policy. Public submission and subagent spawn differ only
//! in the request they build and in whether the resulting handle is returned to
//! a client or to the parent task.

use crate::core::ids::{new_id, now, task_key};
use crate::{ApiError, Result, Runtime};
use common::{scopes, Limits, Principal, Task, TaskState, TeamRoster, Usage};

/// Everything the durable acceptance transaction needs.
pub(crate) struct AdmitRequest {
    /// Owning namespace, resolved before admission.
    pub namespace: String,
    /// Requested agent. A local team replaces it with the designated Master.
    pub agent: String,
    /// Session identity scoped by namespace and caller.
    pub session_id: String,
    /// User input.
    pub input: String,
    /// Bounded inline images admitted with the text input.
    pub input_images: Vec<common::RunImage>,
    /// Caller-scoped idempotency key.
    pub idempotency_key: String,
    /// Canonical digest of the accepted payload.
    pub request_digest: String,
    /// Profile name.
    pub profile: String,
    /// Pin this exact revision instead of the active revision. A subagent pins
    /// the parent's immutable revision so a later activation cannot change it.
    pub pinned_revision: Option<u64>,
    /// Requested limits. Narrowed again by the pinned profile revision.
    pub limits: Limits,
    /// Requested team name from a local submission.
    pub requested_team: Option<String>,
    /// Team roster already authorized by a Gateway delegation.
    pub claimed_team: Option<TeamRoster>,
    /// Authority scopes recorded on the task.
    pub authority_scopes: Vec<String>,
    /// Delegation path recorded on the task.
    pub delegation_path: Vec<String>,
    /// Expiry of the submitting authority.
    pub authority_expires_at: Option<i64>,
    /// Propagated trace identity.
    pub trace_id: String,
    /// `product` for a delegated request, `local` otherwise.
    pub traffic: String,
    /// Frozen subagent narrowing, when this child was spawned by a task.
    pub subagent: Option<common::SubagentPolicy>,
}

/// A durably accepted task.
pub(crate) struct Admitted {
    /// Durable projection at acceptance.
    pub task: Task,
    /// True when an existing handle was returned for a duplicate key.
    pub replayed: bool,
}

impl Runtime {
    /// Admit one task durably. Nothing is dispatched here.
    pub(crate) async fn admit_durable(
        &self,
        principal: &Principal,
        request: AdmitRequest,
    ) -> Result<Admitted> {
        principal.require(scopes::TASK_SUBMIT)?;
        // Hold the admission read gate across the durable acceptance write.
        // Drain takes the write side, so once it returns no later task can be
        // accepted even when this request passed earlier validation.
        let admission = self.admission_guard().await;
        if !*admission {
            let mut error = ApiError::retryable(
                "lifecycle.draining",
                "the Worker is draining and no longer admits new tasks",
            );
            error.retryable = true;
            error.details.insert("retry_after_ms".into(), "1000".into());
            return Err(error);
        }

        validate_identifier("agent", &request.agent)?;
        validate_identifier("session_id", &request.session_id)?;
        if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 200 {
            return Err(ApiError::new(
                "validation.idempotency_key",
                "idempotency_key must be 1-200 characters",
            ));
        }
        if request.input.len() > 1_000_000 {
            return Err(ApiError::new(
                "validation.input_too_large",
                "input exceeds the 1 MB management limit",
            ));
        }
        // Inline images are bounded before any durable write. A malformed image
        // fails here, so no task is admitted and no execution begins.
        if request.input_images.len() > common::MAX_RUN_IMAGES {
            return Err(ApiError::new(
                "validation.input_too_large",
                "input has more images than the published limit",
            )
            .with("limit", common::MAX_RUN_IMAGES.to_string()));
        }
        let mut input_bytes = request.input.len();
        for image in &request.input_images {
            image.validate()?;
            input_bytes = input_bytes.saturating_add(image.decoded_len());
        }
        if input_bytes > common::MAX_RUN_TEXT_BYTES {
            return Err(ApiError::new(
                "validation.input_too_large",
                "combined text and image input exceeds the published limit",
            )
            .with("limit_bytes", common::MAX_RUN_TEXT_BYTES.to_string()));
        }

        let AdmitRequest {
            namespace,
            agent: requested_agent,
            session_id,
            input,
            input_images,
            idempotency_key,
            request_digest: digest,
            profile: profile_name,
            pinned_revision,
            limits,
            requested_team,
            claimed_team,
            authority_scopes,
            delegation_path,
            authority_expires_at,
            trace_id,
            traffic,
            subagent,
        } = request;

        let key = task_key(&namespace, &principal.id, &idempotency_key);
        let source = self.config().instance_id.clone();
        let incarnation = self.incarnation().to_string();
        let retention = self.config().event_retention.max(1);
        let asset_revision = self.config_revision();
        let id = new_id("task");
        let accepted_seq = self.next_seq(&id).await;

        let written = self
            .store()
            .write(|state| {
                if let Some(existing_id) = state.task_index.get(&key) {
                    let existing = state.tasks.get(existing_id).ok_or_else(|| {
                        ApiError::new("task.index_corrupt", "idempotency index points nowhere")
                    })?;
                    if existing.request_digest != digest {
                        return Err(ApiError::new(
                            "task.idempotency_conflict",
                            "this idempotency key was used with a different payload",
                        )
                        .with("task_id", existing.id.clone()));
                    }
                    return Ok((existing.clone(), true));
                }
                let non_terminal = state
                    .tasks
                    .values()
                    .filter(|task| !task.state.is_terminal())
                    .count();
                let admission_limit = self
                    .config()
                    .max_queue
                    .saturating_add(self.config().max_active);
                if subagent.is_some() {
                    // Reserved capacity: children never consume the parent's
                    // per-principal quota, and their own count is bounded by the
                    // reserved child capacity so a parent that already owns its
                    // full quota can still admit the child it waits on.
                    let reserved = self.config().max_subagents.max(1);
                    let child_work = state
                        .tasks
                        .values()
                        .filter(|task| !task.state.is_terminal() && task.subagent.is_some())
                        .count();
                    if child_work >= reserved {
                        return Err(ApiError::retryable(
                            "capacity.subagent_reserved",
                            "the reserved subagent admission capacity is full",
                        )
                        .with("reserved", reserved.to_string())
                        .with("retry_after_ms", "250"));
                    }
                    if non_terminal >= admission_limit.saturating_add(reserved) {
                        return Err(ApiError::retryable(
                            "capacity.queue_full",
                            "the finite admission queue is full",
                        )
                        .with("active_limit", self.config().max_active.to_string())
                        .with("queue_limit", self.config().max_queue.to_string())
                        .with("retry_after_ms", "250"));
                    }
                } else {
                    if non_terminal >= admission_limit {
                        return Err(ApiError::retryable(
                            "capacity.queue_full",
                            "the finite admission queue is full",
                        )
                        .with("active_limit", self.config().max_active.to_string())
                        .with("queue_limit", self.config().max_queue.to_string())
                        .with("retry_after_ms", "250"));
                    }
                    let principal_work = state
                        .tasks
                        .values()
                        .filter(|task| !task.state.is_terminal() && task.caller == principal.id)
                        .count();
                    if principal_work >= self.config().per_principal {
                        return Err(ApiError::retryable(
                            "capacity.principal_quota",
                            "the principal already owns the maximum admitted work",
                        )
                        .with("quota", self.config().per_principal.to_string())
                        .with("retry_after_ms", "250"));
                    }
                }

                let local_team = match (&claimed_team, requested_team.as_deref()) {
                    (Some(roster), _) => Some(roster.clone()),
                    (None, Some(name)) => {
                        let team = state.teams.get(name).ok_or_else(|| {
                            ApiError::new("team.not_found", "team is not registered")
                        })?;
                        if !team.active {
                            return Err(ApiError::new("team.inactive", "team is not active"));
                        }
                        Some(TeamRoster {
                            name: team.name.clone(),
                            version: team.version,
                            master: team.master.clone(),
                            members: Vec::new(),
                            limits: team.limits.clone(),
                        })
                    }
                    (None, None) => None,
                };
                let profile = state
                    .profiles
                    .get(&crate::profile_key(&namespace, &profile_name))
                    .ok_or_else(|| {
                        ApiError::new("profile.not_found", "profile is not registered")
                    })?;
                let revision = match pinned_revision {
                    Some(number) => profile.revision(number).cloned().ok_or_else(|| {
                        ApiError::new(
                            "profile.revision_unavailable",
                            "the pinned profile revision is no longer available",
                        )
                        .with("profile", profile_name.clone())
                        .with("revision", number.to_string())
                    })?,
                    None => profile.active().cloned().ok_or_else(|| {
                        ApiError::new("profile.inactive", "profile has no active revision")
                    })?,
                };
                let revision = match &subagent {
                    Some(policy) => crate::subagents::narrowed_revision(&revision, policy)?,
                    None => revision,
                };
                self.validate_revision(&revision, state)?;

                let agent = match &claimed_team {
                    Some(_) => requested_agent.clone(),
                    None => local_team
                        .as_ref()
                        .map(|team| team.master.clone())
                        .unwrap_or_else(|| requested_agent.clone()),
                };
                if !self
                    .config()
                    .capabilities
                    .iter()
                    .any(|capability| capability.id == agent)
                    && !self.config().routing_agents.contains(&agent)
                    && local_team.is_none()
                {
                    return Err(ApiError::new(
                        "capability.not_registered",
                        "agent capability is not registered by this Worker",
                    ));
                }
                if !revision
                    .grants
                    .iter()
                    .any(|grant| grant == "*" || grant == scopes::TASK_SUBMIT)
                {
                    return Err(ApiError::new(
                        "profile.grant_revoked",
                        "the active profile no longer grants task submission",
                    ));
                }
                for grant in &revision.grants {
                    if grant != "*" && !principal.has(grant) {
                        return Err(ApiError::new(
                            "profile.authority_denied",
                            format!("principal lacks profile grant '{grant}'"),
                        ));
                    }
                }
                if !revision.capabilities.is_empty()
                    && !revision
                        .capabilities
                        .iter()
                        .any(|capability| capability == "*")
                    && !revision.capabilities.contains(&agent)
                    && !self.config().routing_agents.contains(&agent)
                {
                    return Err(ApiError::new(
                        "profile.capability_denied",
                        format!("profile does not include agent capability '{agent}'"),
                    ));
                }
                let limits = limits.narrower(&revision.limits);
                let resolved_model = self.resolve_invocation_model(
                    &revision,
                    &agent,
                    &limits,
                    revision.compaction.as_ref(),
                )?;
                let timestamp = now();
                let task = Task {
                    id: id.clone(),
                    namespace: namespace.clone(),
                    caller: principal.id.clone(),
                    agent: agent.clone(),
                    session_id: session_id.clone(),
                    input: input.clone(),
                    input_images: input_images.clone(),
                    idempotency_key: idempotency_key.clone(),
                    request_digest: digest.clone(),
                    profile: profile_name.clone(),
                    profile_revision: revision.revision,
                    asset_revision,
                    trace_id: trace_id.clone(),
                    traffic,
                    resolved_model,
                    team: local_team.as_ref().map(|team| team.name.clone()),
                    team_version: local_team.as_ref().map(|team| team.version),
                    team_roster: claimed_team.clone(),
                    scopes: authority_scopes.clone(),
                    delegation_path: delegation_path.clone(),
                    authority_expires_at,
                    state: TaskState::Accepted,
                    attempt: 0,
                    native_invocation_id: None,
                    native_thread_id: None,
                    native_a2a_task_id: None,
                    output: None,
                    evidence: None,
                    error: None,
                    uncertainty: None,
                    limits: limits.clone(),
                    usage: Usage::default(),
                    created_at: timestamp,
                    updated_at: timestamp,
                    parent_task_id: subagent
                        .as_ref()
                        .map(|policy| policy.parent_task_id.clone()),
                    root_task_id: subagent.as_ref().map(|policy| policy.root_task_id.clone()),
                    subagent: subagent.clone(),
                    closed_at: None,
                };
                state.task_index.insert(key.clone(), id.clone());
                state.tasks.insert(id.clone(), task.clone());
                // The task projection, its idempotency binding, the first
                // durable event, and the audit record commit in one atomic
                // metadata write. Acceptance is never acknowledged for a
                // partially recorded task.
                crate::tasks::events::push_event(
                    state,
                    &source,
                    &incarnation,
                    retention,
                    &self.redactor(),
                    &id,
                    accepted_seq,
                    "task.accepted",
                    serde_json::json!({
                        "state": "accepted",
                        "agent": task.agent,
                        "profile": task.profile,
                        "profile_revision": task.profile_revision,
                        "asset_revision": task.asset_revision,
                        "trace_id": task.trace_id,
                        "traffic": task.traffic,
                        "resolved_model": task.resolved_model,
                        "limits": task.limits,
                        "parent_task_id": task.parent_task_id,
                        "root_task_id": task.root_task_id,
                        "subagent": task.subagent,
                    }),
                    true,
                )?;
                crate::push_audit(
                    state,
                    &principal.id,
                    "task.submit",
                    &id,
                    "accepted",
                    &format!("agent={} profile={}", task.agent, task.profile),
                    &source,
                    asset_revision,
                    &trace_id,
                    &self.redactor(),
                );
                Ok((task, false))
            })
            .await;
        let (task, replayed) = match written {
            Ok((task, replayed)) => (task, replayed),
            Err(error) => {
                self.forget_seq(&id).await;
                return Err(error);
            }
        };
        if replayed {
            self.forget_seq(&id).await;
        }
        // Acceptance is durable now; a later drain may proceed.
        drop(admission);
        Ok(Admitted { task, replayed })
    }

    /// Dispatch a durably accepted task through the existing executor.
    ///
    /// Returns the state the submitter should observe. A replayed acceptance
    /// owns no new dispatch.
    pub(crate) async fn dispatch_admitted(&self, task: &Task) -> TaskState {
        let cancel = tokio_util::sync::CancellationToken::new();
        self.session_start_hook(&crate::worker::SessionHookContext {
            task_id: Some(task.id.clone()),
            namespace: task.namespace.clone(),
            caller: task.caller.clone(),
            agent: task.agent.clone(),
            session_id: task.session_id.clone(),
            profile: task.profile.clone(),
            profile_revision: task.profile_revision,
            asset_revision: task.asset_revision,
        })
        .await;
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
        if let crate::worker::HookDecision::Deny { reason } =
            self.prompt_submit_hook(&hook_ctx).await
        {
            self.finish_task(
                &task.id,
                crate::ExecutionOutcome::failure(ApiError::new(
                    "hook.prompt_blocked",
                    format!("prompt was blocked by a hook: {}", common::redact(&reason)),
                )),
            )
            .await;
            return TaskState::Failed;
        }
        if let Err(error) = self.before_task_hook(&hook_ctx).await {
            self.finish_task(&task.id, crate::ExecutionOutcome::failure(error))
                .await;
            return TaskState::Failed;
        }
        // Acceptance metadata is durable at this point. The injected
        // acknowledgment fault proves a lost response does not duplicate
        // work, and restart recovery resumes the accepted task.
        crate::core::fault::exit_if(crate::core::fault::Point::BeforeAcknowledge);
        self.register_cancel_token(&task.id, cancel).await;
        let runtime = self.clone();
        let task_id = task.id.clone();
        tokio::spawn(async move {
            runtime.run_task(task_id).await;
        });
        TaskState::Accepted
    }
}

pub(crate) fn validate_identifier(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > 128
        || value
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(ApiError::new(
            "validation.identifier",
            format!("{field} is not a valid opaque identifier"),
        ));
    }
    Ok(())
}
