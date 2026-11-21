//! Durable task admission, execution coordination, and cancellation.

pub(crate) mod admission;
mod approvals;
mod cancel;
pub(crate) mod events;
mod plan;
mod recover;
mod resume;
mod run;

pub use recover::Recovery;

use crate::core::crypto::DelegationClaims;
use crate::core::ids::now;
use crate::{ApiError, Result, Runtime};
use common::{
    scopes, Principal, SubmitTaskRequest, SubmitTaskResponse, Task, TaskState, TeamRoster,
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
        let namespace = request
            .namespace
            .clone()
            .unwrap_or_else(|| principal.namespace.clone());
        principal.require_namespace(&namespace)?;
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
        let admitted = self
            .admit_durable(
                principal,
                crate::tasks::admission::AdmitRequest {
                    namespace,
                    agent: request.agent.clone(),
                    session_id: request.session_id.clone(),
                    input: request.input.clone(),
                    idempotency_key: request.idempotency_key.clone(),
                    request_digest: digest,
                    profile: profile_name,
                    pinned_revision: None,
                    limits: request.limits.clone(),
                    requested_team: request.team.clone(),
                    claimed_team,
                    authority_scopes,
                    delegation_path,
                    authority_expires_at,
                    trace_id: common::trace_id(trace),
                    traffic: if claims.is_some() { "product" } else { "local" }.to_string(),
                    subagent: None,
                },
            )
            .await?;
        let task = admitted.task;
        let replayed = admitted.replayed;
        let response_state = if replayed {
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
            task.state
        } else {
            self.dispatch_admitted(&task).await
        };

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
