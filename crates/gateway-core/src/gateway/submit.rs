//! New submissions: caller identity, dispatch intent, remote attempt, replay.

use super::task::submission_by_id;
use super::worker_client::{issue_team_delegation, issue_worker_delegation};
use super::{Gateway, State};
use crate::caller::Caller;
use crate::fault;
use crate::store::{
    Accepted, AcceptedMapping, AttemptOutcome, Begin, Claim, ClaimToken, DispatchState, NewIntent,
    Submission,
};
use common::{
    scopes, ApiError, GatewaySubmitRequest, GatewaySubmitResponse, SubmitTaskRequest,
    SubmitTaskResponse, TeamRoster, WorkerSummary,
};
use std::time::Duration;

pub(super) enum DispatchReply {
    Accepted(SubmitTaskResponse),
    Rejected(ApiError),
    Unreachable(ApiError),
    Ambiguous(ApiError),
}

impl Gateway {
    /// Submit one task to an explicit Worker.
    ///
    /// The caller's `task:submit` scope and Worker allow-list are enforced here.
    /// Durable order is caller-scoped identity and intent, then a claimed
    /// dispatch attempt, then the remote request. A Worker acceptance is mapped
    /// only after the Worker confirms it, and only the claim owner may commit.
    pub async fn submit(
        &self,
        caller: &Caller,
        request: GatewaySubmitRequest,
    ) -> Result<GatewaySubmitResponse, ApiError> {
        self.submit_with_trace(caller, request, None).await
    }

    /// Submit one task with an optional caller-provided W3C trace identity.
    pub async fn submit_with_trace(
        &self,
        caller: &Caller,
        request: GatewaySubmitRequest,
        trace: Option<&str>,
    ) -> Result<GatewaySubmitResponse, ApiError> {
        caller.principal.require(scopes::TASK_SUBMIT)?;
        let trace_id = common::trace_id(trace);
        let existing = self
            .state
            .dispatch
            .submission_by_key(
                &caller.principal.namespace,
                &caller.principal.id,
                &request.idempotency_key,
            )
            .await?;
        let (worker_id, roster) = if let Some(existing) = &existing {
            // A retry of a durable intent replays the original routing target
            // even when the roster has since changed or deactivated.
            (existing.worker.clone(), None)
        } else {
            match (&request.worker, &request.team) {
                (_, Some(team)) => {
                    let roster =
                        self.resolve_team_roster(&self.state, caller, team, request.team_version)?;
                    if let Some(explicit) = &request.worker {
                        if explicit != &roster.master {
                            return Err(ApiError::new(
                                "route.target_conflict",
                                "an explicit Worker target must be the team's designated Master",
                            )
                            .with("team", roster.name.clone())
                            .with("master", roster.master.clone()));
                        }
                    }
                    let master = roster.master_member().ok_or_else(|| {
                        ApiError::new(
                            "team.master_invalid",
                            "the designated Master is not a roster member",
                        )
                    })?;
                    if !master.capabilities.iter().any(|cap| cap == &request.agent) {
                        return Err(ApiError::new(
                            "scope.capability_denied",
                            format!(
                                "the Master member does not authorize capability '{}'",
                                request.agent
                            ),
                        )
                        .with("team", roster.name.clone())
                        .with("worker", master.worker.clone()));
                    }
                    (roster.master.clone(), Some(roster))
                }
                (Some(worker), None) => (worker.clone(), None),
                (None, None) => {
                    return Err(ApiError::new(
                        "route.worker_required",
                        "an explicit Worker Instance or an active team is required",
                    ));
                }
            }
        };
        if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 200 {
            return Err(ApiError::new(
                "validation.idempotency_key",
                "idempotency_key must be 1-200 characters",
            ));
        }
        let digest = request_digest(&request, &worker_id);

        // New work requires a verified/reachable Worker with the capability.
        // Reconciliation of an existing intent does not: the Worker may have
        // accepted the original request before losing connectivity.
        if existing.is_none() {
            route(&self.state, caller, &worker_id, &request.agent)?;
        } else if !caller.allows(&worker_id) {
            return Err(ApiError::new(
                "scope.worker_denied",
                format!("caller is not authorized for Worker '{worker_id}'"),
            ));
        }

        let begin = self
            .state
            .dispatch
            .begin(NewIntent {
                execution_id: format!("exec-{}", uuid::Uuid::new_v4()),
                namespace: caller.principal.namespace.clone(),
                caller: caller.principal.id.clone(),
                idempotency_key: request.idempotency_key.clone(),
                request_digest: digest,
                worker: worker_id.clone(),
                agent: request.agent.clone(),
            })
            .await?;
        let submission = match begin {
            Begin::Conflict(existing) => {
                return Err(ApiError::new(
                    "task.idempotency_conflict",
                    "this idempotency key was used with a different Gateway request",
                )
                .with("execution_id", existing.execution_id));
            }
            Begin::Existing(existing) => {
                if existing.dispatch == DispatchState::Accepted {
                    return self.replay(existing).await;
                }
                existing
            }
            Begin::Created(created) => {
                // Intent is durable before a retryable remote submission.
                fault::exit_if(fault::Point::AfterIntent);
                created
            }
        };

        let previous_dispatch = submission.dispatch;
        let execution_id = submission.execution_id.clone();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
        let claim = loop {
            match self
                .state
                .dispatch
                .claim(&execution_id, &self.state.replica, self.state.claim_ttl_ms)
                .await?
            {
                Claim::Claimed(token, attempt) => break (token, attempt),
                Claim::Accepted(current) => return self.replay(current).await,
                Claim::Busy { until_ms } => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(ApiError::retryable(
                            "task.dispatch_busy",
                            "another replica still owns the dispatch attempt",
                        )
                        .with("execution_id", execution_id.clone()));
                    }
                    let wait = (until_ms - common::now_ms()).clamp(5, 50) as u64;
                    tokio::time::sleep(Duration::from_millis(wait)).await;
                    if let Some(current) = submission_by_id(&self.state, &execution_id).await {
                        if current.dispatch == DispatchState::Accepted {
                            return self.replay(current).await;
                        }
                    }
                }
                Claim::Missing => return Err(lost_mapping(&execution_id)),
            }
        };
        self.dispatch_claimed(
            claim.0,
            submission,
            caller,
            request,
            previous_dispatch,
            roster,
            &trace_id,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_claimed(
        &self,
        claim: ClaimToken,
        submission: Submission,
        caller: &Caller,
        request: GatewaySubmitRequest,
        previous_dispatch: DispatchState,
        roster: Option<TeamRoster>,
        trace_id: &str,
    ) -> Result<GatewaySubmitResponse, ApiError> {
        let worker = self.worker(&submission.worker)?;
        let token = match &roster {
            Some(roster) => issue_team_delegation(
                &self.state.secret,
                &worker,
                &caller.principal,
                &request.agent,
                scopes::TASK_SUBMIT,
                &request.limits,
                roster,
            )?,
            None => issue_worker_delegation(
                &self.state.secret,
                &worker,
                &caller.principal,
                &request.agent,
                scopes::TASK_SUBMIT,
                &request.limits,
            )?,
        };
        let body = SubmitTaskRequest {
            namespace: Some(caller.principal.namespace.clone()),
            agent: request.agent.clone(),
            session_id: request.session_id.clone(),
            input: request.input.clone(),
            idempotency_key: request.idempotency_key.clone(),
            profile: request.profile.clone(),
            team: request.team.clone(),
            worker: None,
            limits: request.limits.clone(),
        };

        match self
            .state
            .dispatch_submit(&worker, &token, &body, Some(trace_id))
            .await
        {
            DispatchReply::Accepted(response) => {
                if let Some(roster) = &roster {
                    if response.team_version != Some(roster.version) {
                        let _ = self
                            .state
                            .dispatch
                            .release(&claim, AttemptOutcome::ambiguous("team.version_conflict"))
                            .await;
                        return Err(unknown_submission(
                            &claim.execution_id,
                            &worker.id,
                            "the Worker accepted the request without the frozen team version",
                        )
                        .with("team", roster.name.clone())
                        .with("expected_version", roster.version.to_string()));
                    }
                }
                // The Worker confirmed acceptance. If the Gateway stops here,
                // the retry re-uses this exact key and receives the same Worker
                // handle.
                fault::exit_if(fault::Point::AfterAcceptance);
                let committed = self
                    .state
                    .dispatch
                    .commit(
                        &claim,
                        AcceptedMapping {
                            worker_task_id: response.task_id.clone(),
                            worker_instance: worker.instance.clone(),
                            worker_incarnation: worker.incarnation.clone(),
                            accepted: Accepted {
                                agent: response.agent.clone(),
                                profile: response.profile.clone(),
                                state: response.state,
                                team_version: response.team_version,
                            },
                        },
                    )
                    .await;
                let committed = match committed {
                    Ok(committed) => committed,
                    Err(error) => {
                        if let Some(current) =
                            submission_by_id(&self.state, &claim.execution_id).await
                        {
                            if current.dispatch == DispatchState::Accepted {
                                return self.replay(current).await;
                            }
                        }
                        return Err(
                            if matches!(
                                error.code.as_str(),
                                "task.claim_lost" | "task.dispatch_conflict"
                            ) {
                                error.with("execution_id", claim.execution_id.clone())
                            } else {
                                unknown_submission(
                                &claim.execution_id,
                                &worker.id,
                                "the Worker accepted the request but the Gateway mapping did not commit",
                            )
                            },
                        );
                    }
                };
                self.refresh_projection(&worker, &committed.execution_id)
                    .await;
                let current = submission_by_id(&self.state, &committed.execution_id)
                    .await
                    .ok_or_else(|| lost_mapping(&committed.execution_id))?;
                response_of(&current, response.replayed)
            }
            DispatchReply::Rejected(error) => {
                let _ = self
                    .state
                    .dispatch
                    .release(&claim, AttemptOutcome::rejected(&error.code))
                    .await;
                Err(error)
            }
            DispatchReply::Unreachable(error) => {
                let _ = self
                    .state
                    .dispatch
                    .release(
                        &claim,
                        AttemptOutcome::unreachable(previous_dispatch, &error.code),
                    )
                    .await;
                Err(error.with("execution_id", submission.execution_id.clone()))
            }
            DispatchReply::Ambiguous(error) => {
                let _ = self
                    .state
                    .dispatch
                    .release(&claim, AttemptOutcome::ambiguous(&error.code))
                    .await;
                Err(unknown_submission(
                    &submission.execution_id,
                    &worker.id,
                    "the remote request may have been accepted, but no acknowledgment arrived",
                ))
            }
        }
    }

    async fn replay(&self, submission: Submission) -> Result<GatewaySubmitResponse, ApiError> {
        let worker = self.worker(&submission.worker)?;
        self.refresh_projection(&worker, &submission.execution_id)
            .await;
        let current = submission_by_id(&self.state, &submission.execution_id)
            .await
            .ok_or_else(|| lost_mapping(&submission.execution_id))?;
        response_of(&current, true)
    }
}

fn response_of(submission: &Submission, replayed: bool) -> Result<GatewaySubmitResponse, ApiError> {
    let accepted = submission
        .accepted
        .as_ref()
        .ok_or_else(|| lost_mapping(&submission.execution_id))?;
    Ok(GatewaySubmitResponse {
        task_id: submission.execution_id.clone(),
        worker: submission.worker.clone(),
        worker_instance: submission.worker_instance.clone(),
        worker_incarnation: submission.worker_incarnation.clone(),
        agent: accepted.agent.clone(),
        profile: accepted.profile.clone(),
        state: accepted.state,
        replayed,
        team_version: accepted.team_version,
    })
}

pub(super) fn route(
    state: &State,
    caller: &Caller,
    worker_id: &str,
    agent: &str,
) -> Result<WorkerSummary, ApiError> {
    let worker = route_worker(state, caller, worker_id)?;
    if !worker
        .capabilities
        .iter()
        .any(|capability| capability.id == agent)
    {
        return Err(ApiError::new(
            "scope.capability_denied",
            format!("Worker '{worker_id}' does not advertise capability '{agent}'"),
        )
        .with("worker", worker_id.to_string()));
    }
    Ok(worker)
}

/// Resolve an approved, reachable Worker without requiring an agent capability.
///
/// Interactive control targets a supported package capability, not an agent
/// identifier. Routing still applies the caller's Worker allow-list and the
/// operator's approved/disabled/decommissioned state.
pub(super) fn route_worker(
    state: &State,
    caller: &Caller,
    worker_id: &str,
) -> Result<WorkerSummary, ApiError> {
    state.registry.expire(state.registry.now());
    let worker = state.registry.worker(worker_id).ok_or_else(|| {
        ApiError::new(
            "route.worker.not_found",
            format!("Worker '{worker_id}' is not approved by this Gateway"),
        )
    })?;
    if !caller.allows(worker_id) {
        return Err(ApiError::new(
            "scope.worker_denied",
            format!("caller is not authorized for Worker '{worker_id}'"),
        ));
    }
    if worker.status == "decommissioned" {
        return Err(ApiError::new(
            "route.worker.not_found",
            format!("Worker '{worker_id}' is decommissioned"),
        ));
    }
    if worker.status == "disabled" {
        return Err(ApiError::new(
            "route.worker_unavailable",
            format!("Worker '{worker_id}' is disabled by an operator"),
        )
        .with("worker", worker_id.to_string())
        .with("status", "disabled"));
    }
    if !worker.reachable {
        return Err(ApiError::retryable(
            "route.worker_unavailable",
            format!("Worker '{worker_id}' is not reachable"),
        )
        .with("worker", worker_id.to_string()));
    }
    Ok(worker)
}

pub(super) fn request_digest(request: &GatewaySubmitRequest, worker: &str) -> String {
    let profile = request
        .profile
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let canonical = serde_json::json!({
        "worker": worker,
        "agent": request.agent,
        "session_id": request.session_id,
        "input": request.input,
        "profile": profile,
        "team": request.team,
        "team_version": request.team_version,
        "limits": request.limits,
    });
    common::canonical_digest(&canonical)
}

fn lost_mapping(execution_id: &str) -> ApiError {
    ApiError::new(
        "storage.corrupt",
        "the dispatch index points to a missing execution",
    )
    .with("execution_id", execution_id.to_string())
}

pub(super) fn unknown_submission(execution_id: &str, worker: &str, detail: &str) -> ApiError {
    ApiError::retryable("task.unknown_submission", detail)
        .with("execution_id", execution_id.to_string())
        .with("worker", worker.to_string())
}
