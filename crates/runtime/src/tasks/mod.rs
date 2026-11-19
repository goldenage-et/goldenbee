//! Durable task admission, execution coordination, and cancellation.

mod approvals;
mod cancel;
pub(crate) mod events;
mod recover;
mod resume;
mod run;

pub use recover::Recovery;

use crate::core::crypto::DelegationClaims;
use crate::core::ids::{new_id, now, task_key};
use crate::{ApiError, ExecutionOutcome, Result, Runtime};
use common::{
    scopes, Principal, SubmitTaskRequest, SubmitTaskResponse, Task, TaskState, TeamRoster, Usage,
};

impl Runtime {
    /// Submit a task without a Gateway delegation context.
    pub async fn submit_task(
        &self,
        principal: &Principal,
        request: SubmitTaskRequest,
    ) -> Result<SubmitTaskResponse> {
        self.submit_task_with_claims(principal, request, None).await
    }

    /// Submit a task, honoring a Gateway delegation roster when present.
    pub async fn submit_task_with_claims(
        &self,
        principal: &Principal,
        request: SubmitTaskRequest,
        claims: Option<&DelegationClaims>,
    ) -> Result<SubmitTaskResponse> {
        self.submit_task_traced(principal, request, claims, None)
            .await
    }

    /// Submit a task with an optional propagated trace identity.
    pub async fn submit_task_with_trace(
        &self,
        principal: &Principal,
        request: SubmitTaskRequest,
        claims: Option<&DelegationClaims>,
        trace: Option<&str>,
    ) -> Result<SubmitTaskResponse> {
        self.submit_task_traced(principal, request, claims, trace)
            .await
    }

    async fn submit_task_traced(
        &self,
        principal: &Principal,
        request: SubmitTaskRequest,
        claims: Option<&DelegationClaims>,
        trace: Option<&str>,
    ) -> Result<SubmitTaskResponse> {
        principal.require(scopes::TASK_SUBMIT)?;
        let namespace = request
            .namespace
            .clone()
            .unwrap_or_else(|| principal.namespace.clone());
        principal.require_namespace(&namespace)?;
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

        let claimed_team: Option<TeamRoster> = match claims {
            Some(claims) => match &claims.team {
                Some(roster) => {
                    if request.team.as_deref() != Some(roster.name.as_str()) {
                        return Err(ApiError::new(
                            "team.name_mismatch",
                            "the delegated roster does not match the requested team",
                        )
                        .with("team", roster.name.clone()));
                    }
                    let master = roster.master_member().ok_or_else(|| {
                        ApiError::new(
                            "team.master_invalid",
                            "the delegated roster has no designated Master",
                        )
                    })?;
                    if !master.capabilities.iter().any(|cap| cap == &request.agent) {
                        return Err(ApiError::new(
                            "scope.capability_denied",
                            "the designated Master does not authorize this capability",
                        )
                        .with("capability", request.agent.clone()));
                    }
                    if !claims.capabilities.iter().any(|cap| cap == &request.agent) {
                        return Err(ApiError::new(
                            "delegation.capability_escalation",
                            "the task capability is outside the signed delegation",
                        ));
                    }
                    Some(roster.clone())
                }
                None => None,
            },
            None => None,
        };
        let profile_name = request
            .profile
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let canonical = serde_json::json!({
            "agent": request.agent,
            "session_id": request.session_id,
            "input": request.input,
            "profile": profile_name,
            "team": request.team,
            "team_version": claimed_team.as_ref().map(|roster| roster.version),
            "limits": request.limits,
        });
        let digest = common::canonical_digest(&canonical);
        let (authority_scopes, delegation_path, authority_expires_at) = match claims {
            Some(claims) => (
                claims.scopes.clone(),
                if claims.path.is_empty() {
                    vec![self.config().instance_id.clone()]
                } else {
                    claims.path.clone()
                },
                Some(claims.expires_at),
            ),
            None => (
                principal.scopes.clone(),
                vec![self.config().instance_id.clone()],
                None,
            ),
        };
        let key = task_key(&namespace, &principal.id, &request.idempotency_key);
        let source = self.config().instance_id.clone();
        let incarnation = self.incarnation().to_string();
        let retention = self.config().event_retention.max(1);
        let asset_revision = self.config_revision();
        let trace_id = common::trace_id(trace);
        let traffic = if claims.is_some() { "product" } else { "local" };

        let (task, replayed) = self
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

                let local_team = match (&claimed_team, request.team.as_deref()) {
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
                let revision = profile.active().ok_or_else(|| {
                    ApiError::new("profile.inactive", "profile has no active revision")
                })?;
                self.validate_revision(revision, state)?;

                let agent = match &claimed_team {
                    Some(_) => request.agent.clone(),
                    None => local_team
                        .as_ref()
                        .map(|team| team.master.clone())
                        .unwrap_or_else(|| request.agent.clone()),
                };
                if !self
                    .config()
                    .capabilities
                    .iter()
                    .any(|capability| capability.id == agent)
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
                {
                    return Err(ApiError::new(
                        "profile.capability_denied",
                        format!("profile does not include agent capability '{agent}'"),
                    ));
                }
                let limits = request.limits.clone().narrower(&revision.limits);
                let resolved_model = self.resolve_invocation_model(
                    revision,
                    &agent,
                    limits.max_tokens,
                    revision.compaction.as_ref(),
                )?;
                let timestamp = now();
                let id = new_id("task");
                let task = Task {
                    id: id.clone(),
                    namespace: namespace.clone(),
                    caller: principal.id.clone(),
                    agent: agent.clone(),
                    session_id: request.session_id.clone(),
                    input: request.input.clone(),
                    idempotency_key: request.idempotency_key.clone(),
                    request_digest: digest.clone(),
                    profile: profile_name.clone(),
                    profile_revision: revision.revision,
                    asset_revision,
                    trace_id: trace_id.clone(),
                    traffic: traffic.into(),
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
                    &id,
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
                );
                Ok((task, false))
            })
            .await?;
        // Acceptance is durable now; a later drain may proceed.
        drop(admission);

        let mut response_state = task.state;
        if replayed {
            // The original acceptance is already durable. A failed replay audit
            // cannot turn that committed acceptance into a rejection.
            let _ = self
                .audit(
                    &principal.id,
                    "task.submit",
                    &task.id,
                    "replayed",
                    &format!("agent={} profile={}", task.agent, task.profile),
                )
                .await;
        } else {
            let cancel = tokio_util::sync::CancellationToken::new();
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
            if let Err(error) = self.before_task_hook(&hook_ctx).await {
                self.finish_task(&task.id, ExecutionOutcome::failure(error))
                    .await;
                response_state = TaskState::Failed;
            } else {
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
            }
        }

        Ok(SubmitTaskResponse {
            task_id: task.id,
            state: response_state,
            replayed,
            unknown_submission: false,
            agent: task.agent,
            profile: format!("{}:{}", task.profile, task.profile_revision),
            team_version: task.team_version,
        })
    }

    /// Record that a policy-allowed fallback produced this invocation.
    ///
    /// The task projection is updated only after the fallback provider has
    /// successfully returned a stream, so the recorded provider is truthful.
    pub async fn record_model_fallback(&self, task_id: &str, model: common::ResolvedModel) {
        let updated = self
            .store()
            .write(|state| {
                let task = state.tasks.get_mut(task_id).ok_or_else(|| {
                    ApiError::new("task.not_found", "task disappeared during fallback")
                })?;
                task.resolved_model = Some(model.clone());
                task.updated_at = now();
                Ok(())
            })
            .await;
        if updated.is_ok() {
            let _ = self
                .append_event(
                    task_id,
                    "model.fallback",
                    serde_json::json!({"resolved_model": model}),
                    true,
                )
                .await;
        }
    }

    /// Fetch a task visible to the principal.
    pub async fn task(&self, principal: &Principal, task_id: &str) -> Result<Task> {
        principal.require(scopes::TASK_READ)?;
        let task = self
            .store()
            .read(|state| state.tasks.get(task_id).cloned())
            .await
            .ok_or_else(|| ApiError::new("task.not_found", "task is not visible"))?;
        principal.require_namespace(&task.namespace)?;
        if task.caller != principal.id && !principal.has(scopes::ADMIN) {
            return Err(ApiError::new(
                "scope.task_owner",
                "task belongs to another caller",
            ));
        }
        Ok(task)
    }

    /// Fetch a task for reconciliation without caller scoping.
    pub async fn task_unscoped(&self, task_id: &str) -> Option<Task> {
        self.store()
            .read(|state| state.tasks.get(task_id).cloned())
            .await
    }

    /// Attach package-owned objective evidence to a committed task outcome.
    ///
    /// This never rewrites the task state or native outcome. A package hook
    /// records digest references, check outcomes, and unverified limits so the
    /// caller can inspect what was actually proved.
    pub async fn set_task_evidence(
        &self,
        task_id: &str,
        evidence: serde_json::Value,
    ) -> Result<()> {
        self.store()
            .write(|state| {
                let task = state.tasks.get_mut(task_id).ok_or_else(|| {
                    ApiError::new("task.not_found", "task is not available for evidence")
                })?;
                task.evidence = Some(evidence);
                task.updated_at = crate::now();
                Ok(())
            })
            .await
    }

    /// List tasks visible to the principal.
    pub async fn tasks(
        &self,
        principal: &Principal,
        state: Option<TaskState>,
    ) -> Result<Vec<Task>> {
        principal.require(scopes::TASK_READ)?;
        let namespace = principal.namespace.clone();
        let caller = principal.id.clone();
        let admin = principal.has(scopes::ADMIN);
        let wanted = state;
        Ok(self
            .store()
            .read(|state| {
                state
                    .tasks
                    .values()
                    .filter(|task| task.namespace == namespace)
                    .filter(|task| admin || task.caller == caller)
                    .filter(|task| wanted.is_none() || Some(task.state) == wanted)
                    .cloned()
                    .collect()
            })
            .await)
    }

    /// Return every persisted task. Used by recovery and reconciliation only.
    pub async fn all_tasks(&self) -> Vec<Task> {
        self.store()
            .read(|state| state.tasks.values().cloned().collect())
            .await
    }

    /// Bind a confirmed native invocation identity to a task.
    pub async fn bind_native(&self, task_id: &str, invocation: &str) -> Result<()> {
        self.store()
            .write(|state| {
                if let Some(task) = state.tasks.get_mut(task_id) {
                    task.native_invocation_id = Some(invocation.to_string());
                    task.updated_at = now();
                }
                Ok(())
            })
            .await
    }

    /// Bind the public task handle to the native graph thread before dispatch.
    ///
    /// The native `Agent::run` path uses the session id as the graph thread, so
    /// the public binding is exactly that value. Repeating the same binding is
    /// idempotent; a conflicting binding is rejected by the graph executor.
    pub async fn bind_thread(&self, task_id: &str, thread: &str) -> Result<()> {
        self.store()
            .write(|state| {
                if let Some(task) = state.tasks.get_mut(task_id) {
                    task.native_thread_id = Some(thread.to_string());
                    task.updated_at = now();
                }
                Ok(())
            })
            .await
    }
}

fn validate_identifier(field: &str, value: &str) -> Result<()> {
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
