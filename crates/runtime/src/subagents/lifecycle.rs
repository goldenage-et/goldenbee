//! Durable runtime-local subagent lifecycle.
//!
//! A subagent is an ordinary task: it is admitted through the same durable
//! transaction, executed by the same native executor, observed through the same
//! event stream, and recovered by the same restart rules. This module owns only
//! the parent linkage, the narrowed policy, the lifecycle operations over
//! durable handles, and the bounded result merge.
//!
//! Cross-Worker teams remain Gateway-owned. Nothing here routes between
//! processes or opens a second task store, event bus, or executor.

use crate::core::ids::{new_id, now};
use crate::policy::PermissionMode;
use crate::tasks::admission::AdmitRequest;
use crate::{ApiError, Result, Runtime};
use common::{
    canonical_digest, Limits, Principal, SubagentPolicy, SubagentResult, SubagentSummary, Task,
};
use std::time::{Duration, Instant};

/// Caller-supplied subagent spawn arguments.
///
/// Every optional field narrows the named definition. None of them can widen
/// the definition, the Worker policy, or the parent's pinned profile.
#[derive(Debug, Clone, Default)]
pub struct SpawnRequest {
    /// Named Worker-declared definition.
    pub definition: String,
    /// Child input.
    pub input: String,
    /// Background admission override.
    pub background: Option<bool>,
    /// Logical model reference override.
    pub model: Option<String>,
    /// Tool allowlist override.
    pub tools: Option<Vec<String>>,
    /// Permission-mode override.
    pub permission_mode: Option<PermissionMode>,
}

impl Runtime {
    /// Admit and start one runtime-local subagent owned by a parent task.
    pub async fn spawn_subagent(
        &self,
        parent_task_id: &str,
        request: SpawnRequest,
    ) -> Result<SubagentResult> {
        let config = self.subagent_config().await.ok_or_else(|| {
            ApiError::new(
                "subagent.not_enabled",
                "this Worker does not declare a runtime-local subagent policy",
            )
        })?;
        let parent = self.parent_task(parent_task_id).await?;
        let definition = config
            .definition_named(&request.definition)
            .ok_or_else(|| {
                ApiError::new(
                    "subagent.definition_unknown",
                    "the named subagent definition is not declared by this Worker",
                )
                .with("definition", request.definition.clone())
                .with("declared", config.names().join(","))
            })?;
        let parent_policy = parent.subagent.clone();
        let depth = parent_policy
            .as_ref()
            .map(|policy| policy.depth)
            .unwrap_or(0)
            .saturating_add(1);
        let depth_ceiling = bounded_ceiling(
            config.depth_ceiling(),
            parent.limits.max_depth,
            parent_policy
                .as_ref()
                .map(|policy| policy.max_depth)
                .unwrap_or(0),
        );
        if depth_ceiling > 0 && depth > depth_ceiling {
            return Err(ApiError::new(
                "subagent.depth_exceeded",
                "the parent has no remaining subagent delegation depth",
            )
            .with("depth", depth.to_string())
            .with("max_depth", depth_ceiling.to_string()));
        }
        let fanout_ceiling = bounded_ceiling(
            config.fanout_ceiling(),
            parent.limits.max_fanout,
            parent_policy
                .as_ref()
                .map(|policy| policy.max_fanout)
                .unwrap_or(0),
        );
        if fanout_ceiling > 0 {
            let live = self
                .subagents_of(parent_task_id, true)
                .await
                .into_iter()
                .filter(|child| !child.state.is_terminal())
                .count();
            if live >= fanout_ceiling as usize {
                return Err(ApiError::retryable(
                    "subagent.fanout_exceeded",
                    "the parent already owns the maximum live subagents",
                )
                .with("live", live.to_string())
                .with("max_fanout", fanout_ceiling.to_string())
                .with("retry_after_ms", "250"));
            }
        }
        let revision = self
            .profile_revision_unscoped(&parent.namespace, &parent.profile, parent.profile_revision)
            .await
            .ok_or_else(|| {
                ApiError::new(
                    "profile.revision_unavailable",
                    "the parent's pinned profile revision is no longer available",
                )
                .with("profile", parent.profile.clone())
                .with("revision", parent.profile_revision.to_string())
            })?;
        if !revision.members.is_empty() && definition.agent_name() != parent.agent {
            return Err(ApiError::new(
                "subagent.agent_unsupported",
                "a composed parent profile runs its own members; a child cannot select another agent",
            )
            .with("agent", definition.agent_name().to_string())
            .with("parent_agent", parent.agent.clone()));
        }
        let parent_provider = parent
            .resolved_model
            .as_ref()
            .map(|model| model.provider.as_str());
        let model = config.narrow_model(definition, parent_provider, request.model.as_deref())?;
        // A base (non-composed) agent owns its native model reference. An
        // override that names the same provider is a no-op; a different model
        // cannot be enforced without rebuilding package code, so it is denied
        // instead of silently ignored.
        let model = match (model, revision.members.is_empty()) {
            (Some(model), true) => {
                let base = self
                    .config()
                    .agent_providers
                    .get(definition.agent_name())
                    .cloned();
                if base.as_deref() != Some(model.as_str()) {
                    return Err(ApiError::new(
                        "subagent.model_unsupported",
                        "the selected base agent cannot change its native model reference",
                    )
                    .with("model", model));
                }
                None
            }
            (model, _) => model,
        };
        let tools = config.narrow_tools(definition, request.tools.as_deref())?;
        let parent_mode = self.permission_snapshot().mode();
        let mode = config.narrow_mode(definition, parent_mode, request.permission_mode)?;
        let background = config.resolve_background(definition, request.background)?;

        let policy = SubagentPolicy {
            parent_task_id: parent.id.clone(),
            root_task_id: parent
                .root_task_id
                .clone()
                .unwrap_or_else(|| parent.id.clone()),
            definition: request.definition.clone(),
            depth,
            background,
            model,
            tools,
            permission_mode: Some(mode.as_str().to_string()),
            max_fanout: fanout_ceiling,
            max_depth: depth_ceiling,
        };
        let limits = Limits {
            max_depth: policy.max_depth,
            max_fanout: policy.max_fanout,
            ..Limits::default()
        }
        .narrower(&parent.limits);
        let canonical = serde_json::json!({
            "parent": parent.id,
            "definition": policy.definition,
            "input": request.input,
            "background": background,
            "model": policy.model,
            "tools": policy.tools,
            "permission_mode": policy.permission_mode,
        });
        let digest = canonical_digest(&canonical);
        let idempotency_key = format!("sub:spawn:{}:{}", parent.id, digest);
        let admitted = self
            .admit_durable(
                &principal_of(&parent),
                AdmitRequest {
                    namespace: parent.namespace.clone(),
                    agent: definition.agent_name().to_string(),
                    session_id: new_id("sub"),
                    input: request.input.clone(),
                    input_images: Vec::new(),
                    idempotency_key,
                    request_digest: digest,
                    profile: parent.profile.clone(),
                    pinned_revision: Some(parent.profile_revision),
                    limits,
                    requested_team: None,
                    claimed_team: None,
                    authority_scopes: parent.scopes.clone(),
                    delegation_path: parent.delegation_path.clone(),
                    authority_expires_at: parent.authority_expires_at,
                    trace_id: parent.trace_id.clone(),
                    traffic: parent.traffic.clone(),
                    subagent: Some(policy.clone()),
                },
            )
            .await?;
        let child = admitted.task;
        if !admitted.replayed {
            self.dispatch_admitted(&child).await;
        }
        let refreshed = self.task_unscoped(&child.id).await;
        let child = refreshed.unwrap_or(child);
        let _ = self
            .append_event(
                &parent.id,
                "subagent.spawned",
                serde_json::json!({
                    "child_task_id": child.id,
                    "definition": policy.definition,
                    "agent": child.agent,
                    "background": background,
                    "depth": depth,
                    "state": child.state,
                    "session_id": child.session_id,
                    "trace_id": child.trace_id,
                }),
                true,
            )
            .await;
        let _ = self
            .audit(
                &parent.caller,
                "subagent.spawn",
                &child.id,
                "accepted",
                &format!(
                    "parent={} definition={} agent={} background={} depth={}",
                    parent.id, policy.definition, child.agent, background, depth
                ),
            )
            .await;
        if background {
            return Ok(SubagentResult::from_task(&child, config.result_limit()));
        }
        self.await_child(&parent, &child, config.wait_limit_ms())
            .await
    }

    /// Continue a completed child in its existing session.
    ///
    /// The follow-up is a new durable task with the same lineage and the same
    /// frozen policy; it never reuses a handle after a closed child.
    pub async fn continue_subagent(
        &self,
        parent_task_id: &str,
        child_task_id: &str,
        input: String,
        background: Option<bool>,
    ) -> Result<SubagentResult> {
        let config = self.subagent_config().await.ok_or_else(|| {
            ApiError::new(
                "subagent.not_enabled",
                "this Worker does not declare a runtime-local subagent policy",
            )
        })?;
        let parent = self.parent_task(parent_task_id).await?;
        let child = self.owned_child(&parent, child_task_id).await?;
        if child.closed_at.is_some() {
            return Err(ApiError::new(
                "subagent.closed",
                "this subagent was closed by its lineage owner",
            )
            .with("task_id", child.id.clone()));
        }
        if !child.state.is_terminal() {
            return Err(ApiError::retryable(
                "subagent.busy",
                "the subagent is still running; wait for it before sending input",
            )
            .with("task_id", child.id.clone())
            .with("state", format!("{:?}", child.state)));
        }
        let Some(policy) = child.subagent.clone() else {
            return Err(ApiError::new(
                "subagent.not_a_child",
                "the target task is not a runtime-local subagent",
            ));
        };
        let background = background.unwrap_or(policy.background);
        if background && !config.allows_background() {
            return Err(ApiError::new(
                "subagent.background_denied",
                "this Worker does not permit background subagents",
            ));
        }
        let follow_up = SubagentPolicy {
            background,
            ..policy
        };
        let digest = canonical_digest(&serde_json::json!({
            "continue": child.id,
            "input": input,
            "background": background,
        }));
        let admitted = self
            .admit_durable(
                &principal_of(&parent),
                AdmitRequest {
                    namespace: child.namespace.clone(),
                    agent: child.agent.clone(),
                    session_id: child.session_id.clone(),
                    input: input.clone(),
                    input_images: Vec::new(),
                    idempotency_key: format!("sub:continue:{}:{}", child.id, digest),
                    request_digest: digest,
                    profile: child.profile.clone(),
                    pinned_revision: Some(child.profile_revision),
                    limits: child.limits.clone().narrower(&parent.limits),
                    requested_team: None,
                    claimed_team: None,
                    authority_scopes: parent.scopes.clone(),
                    delegation_path: parent.delegation_path.clone(),
                    authority_expires_at: parent.authority_expires_at,
                    trace_id: parent.trace_id.clone(),
                    traffic: child.traffic.clone(),
                    subagent: Some(follow_up.clone()),
                },
            )
            .await?;
        let next = admitted.task;
        if !admitted.replayed {
            self.dispatch_admitted(&next).await;
        }
        let refreshed = self.task_unscoped(&next.id).await;
        let next = refreshed.unwrap_or(next);
        let _ = self
            .append_event(
                &parent.id,
                "subagent.continued",
                serde_json::json!({
                    "child_task_id": next.id,
                    "previous_task_id": child.id,
                    "definition": follow_up.definition,
                    "background": background,
                    "state": next.state,
                    "session_id": next.session_id,
                }),
                true,
            )
            .await;
        if background {
            return Ok(SubagentResult::from_task(&next, config.result_limit()));
        }
        self.await_child(&parent, &next, config.wait_limit_ms())
            .await
    }

    /// Wait for one owned child, bounded by the Worker wait ceiling.
    ///
    /// While waiting, the parent releases its execution permit through the same
    /// pause registry used by approval waits. A parent that waits on a child
    /// therefore cannot deadlock the process execution ceiling.
    pub async fn wait_subagent(
        &self,
        parent_task_id: &str,
        child_task_id: &str,
        timeout_ms: Option<u64>,
    ) -> Result<SubagentResult> {
        let config = self.subagent_config().await.ok_or_else(|| {
            ApiError::new(
                "subagent.not_enabled",
                "this Worker does not declare a runtime-local subagent policy",
            )
        })?;
        let parent = self.parent_task(parent_task_id).await?;
        let child = self.owned_child(&parent, child_task_id).await?;
        let timeout = timeout_ms
            .unwrap_or(config.wait_limit_ms())
            .min(config.wait_limit_ms())
            .max(1);
        self.await_child(&parent, &child, timeout).await
    }

    /// List children of one parent, most recent last.
    ///
    /// A root task may also inspect its descendants' direct children by passing
    /// the root identity; ownership for mutation still requires the direct
    /// parent.
    pub async fn subagents_of(
        &self,
        parent_task_id: &str,
        include_closed: bool,
    ) -> Vec<SubagentSummary> {
        let mut children: Vec<SubagentSummary> = self
            .store()
            .read(|state| {
                state
                    .tasks
                    .values()
                    .filter(|task| task.parent_task_id.as_deref() == Some(parent_task_id))
                    .filter(|task| include_closed || task.closed_at.is_none())
                    .map(summary)
                    .collect()
            })
            .await;
        children.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.task_id.cmp(&right.task_id))
        });
        children
    }

    /// Cancel one owned child without closing it.
    ///
    /// An interrupted child keeps its durable history and result and can still
    /// receive follow-up input.
    pub async fn interrupt_subagent(
        &self,
        parent_task_id: &str,
        child_task_id: &str,
    ) -> Result<SubagentResult> {
        let config = self.subagent_config().await.ok_or_else(|| {
            ApiError::new(
                "subagent.not_enabled",
                "this Worker does not declare a runtime-local subagent policy",
            )
        })?;
        let parent = self.parent_task(parent_task_id).await?;
        let child = self.owned_child(&parent, child_task_id).await?;
        if child.state.is_terminal() {
            // Nothing is running; an interrupt is a truthful no-op.
            return Ok(SubagentResult::from_task(&child, config.result_limit()));
        }
        let child = self.cancel_claimed(&child.id, &child.caller, true).await?;
        let _ = self
            .append_event(
                &parent.id,
                "subagent.interrupted",
                serde_json::json!({
                    "child_task_id": child.id,
                    "definition": child.subagent.as_ref().map(|policy| policy.definition.clone()),
                    "state": child.state,
                }),
                true,
            )
            .await;
        let _ = self
            .audit(
                &parent.caller,
                "subagent.interrupt",
                &child.id,
                if child.state.is_terminal() {
                    "cancelled"
                } else {
                    "requested"
                },
                "",
            )
            .await;
        Ok(SubagentResult::from_task(&child, config.result_limit()))
    }

    /// Close one owned child.
    ///
    /// Closing cancels work that is still running, records the close durably on
    /// the child, and removes it from the default listing. It is idempotent: a
    /// second close records no duplicate event.
    pub async fn close_subagent(
        &self,
        parent_task_id: &str,
        child_task_id: &str,
    ) -> Result<SubagentResult> {
        let config = self.subagent_config().await.ok_or_else(|| {
            ApiError::new(
                "subagent.not_enabled",
                "this Worker does not declare a runtime-local subagent policy",
            )
        })?;
        let parent = self.parent_task(parent_task_id).await?;
        let child = self.owned_child(&parent, child_task_id).await?;
        if child.closed_at.is_some() {
            return Ok(SubagentResult::from_task(&child, config.result_limit()));
        }
        let child = if child.state.is_terminal() {
            // A terminal child may still own running background descendants;
            // closing the child stops the work it owns as well.
            self.cancel_owned_subagents(&child.id).await;
            child
        } else {
            self.cancel_claimed(&child.id, &child.caller, true).await?
        };
        let closed = self
            .store()
            .write(|state| {
                let task = state
                    .tasks
                    .get_mut(&child.id)
                    .ok_or_else(|| ApiError::new("task.not_found", "subagent disappeared"))?;
                if task.closed_at.is_none() {
                    task.closed_at = Some(now());
                    task.updated_at = now();
                }
                Ok(task.clone())
            })
            .await?;
        let _ = self
            .append_event(
                &parent.id,
                "subagent.closed",
                serde_json::json!({
                    "child_task_id": closed.id,
                    "definition": closed.subagent.as_ref().map(|policy| policy.definition.clone()),
                    "state": closed.state,
                }),
                true,
            )
            .await;
        let _ = self
            .append_event(
                &closed.id,
                "subagent.closed",
                serde_json::json!({"parent_task_id": parent.id, "state": closed.state}),
                true,
            )
            .await;
        let _ = self
            .audit(
                &parent.caller,
                "subagent.close",
                &closed.id,
                "closed",
                &format!("parent={}", parent.id),
            )
            .await;
        Ok(SubagentResult::from_task(&closed, config.result_limit()))
    }

    /// Cancel every non-terminal child owned by one task, recursively.
    ///
    /// Called by the parent cancellation path and when a parent ends in a
    /// non-success state. Cancellation itself is durable, so a restart cannot
    /// resurrect work the lineage owner already stopped.
    pub(crate) async fn cancel_owned_subagents(&self, parent_task_id: &str) {
        // Collect the whole non-terminal subtree first, then cancel each leaf
        // through the non-recursive cancellation entry point. A running child's
        // own finish path may cascade again; cancellation is idempotent.
        let descendants: Vec<(String, String)> = self
            .store()
            .read(|state| {
                let mut found = Vec::new();
                let mut frontier = vec![parent_task_id.to_string()];
                while let Some(parent) = frontier.pop() {
                    for task in state.tasks.values() {
                        if task.parent_task_id.as_deref() == Some(parent.as_str())
                            && !task.state.is_terminal()
                        {
                            found.push((task.id.clone(), task.caller.clone()));
                            frontier.push(task.id.clone());
                        }
                    }
                }
                found
            })
            .await;
        for (child, caller) in descendants {
            let _ = self.cancel_one(&child, &caller).await;
        }
    }

    /// Append the once-only terminal notification for a child that became
    /// terminal outside the normal finish path (cancellation of accepted work).
    pub(crate) async fn notify_parent_terminal(&self, child: &Task) {
        let Some(parent_id) = child.parent_task_id.clone() else {
            return;
        };
        if !child.state.is_terminal() {
            return;
        }
        let payload = Self::subagent_terminal_payload(child);
        let _ = self
            .append_event(&parent_id, "subagent.terminal", payload, true)
            .await;
        let _ = self
            .audit(
                &child.caller,
                "subagent.terminal",
                &child.id,
                "terminal",
                &format!("parent={parent_id} state={:?}", child.state),
            )
            .await;
    }

    /// Bounded payload appended to a parent stream when a child becomes
    /// terminal. Used by the terminal commit path so the notification is not
    /// duplicated and does not depend on a live subscriber.
    pub(crate) fn subagent_terminal_payload(child: &Task) -> serde_json::Value {
        serde_json::to_value(SubagentResult::from_task(child, 4_096)).unwrap_or_else(|_| {
            serde_json::json!({
                "task_id": child.id,
                "state": child.state,
                "parent_task_id": child.parent_task_id,
            })
        })
    }

    async fn parent_task(&self, parent_task_id: &str) -> Result<Task> {
        let parent = self
            .task_unscoped(parent_task_id)
            .await
            .ok_or_else(|| ApiError::new("task.not_found", "the parent task is not visible"))?;
        if parent.state.is_terminal() {
            return Err(ApiError::new(
                "subagent.parent_terminal",
                "a terminal parent task cannot spawn or manage subagents",
            )
            .with("task_id", parent.id.clone())
            .with("state", format!("{:?}", parent.state)));
        }
        Ok(parent)
    }

    async fn owned_child(&self, parent: &Task, child_task_id: &str) -> Result<Task> {
        let child = self
            .task_unscoped(child_task_id)
            .await
            .ok_or_else(|| ApiError::new("task.not_found", "the subagent task is not visible"))?;
        if child.parent_task_id.as_deref() != Some(parent.id.as_str())
            || child.namespace != parent.namespace
            || child.caller != parent.caller
        {
            return Err(ApiError::new(
                "subagent.not_owned",
                "the task is not a child of this parent",
            )
            .with("task_id", child.id)
            .with("parent_task_id", parent.id.clone()));
        }
        Ok(child)
    }

    async fn await_child(
        &self,
        parent: &Task,
        child: &Task,
        timeout_ms: u64,
    ) -> Result<SubagentResult> {
        let limit = self
            .subagent_config()
            .await
            .map(|config| config.result_limit())
            .unwrap_or(crate::subagents::DEFAULT_RESULT_BYTES);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
        let parent_cancel = self.cancel_token(&parent.id).await;
        let mut releasing = false;
        loop {
            let Some(current) = self.task_unscoped(&child.id).await else {
                return Err(ApiError::new(
                    "task.not_found",
                    "the subagent task is no longer visible",
                ));
            };
            if current.state.is_terminal() {
                if releasing {
                    self.set_pause(&parent.id, crate::PAUSE_RUNNING).await;
                }
                return Ok(SubagentResult::from_task(&current, limit));
            }
            if parent_cancel
                .as_ref()
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
            {
                // The parent cancellation path owns the cascade. Returning a
                // non-terminal result is truthful and lets the tool call end.
                return Ok(SubagentResult::from_task(&current, limit));
            }
            if Instant::now() >= deadline {
                if releasing {
                    self.set_pause(&parent.id, crate::PAUSE_RUNNING).await;
                }
                return Ok(SubagentResult::from_task(&current, limit));
            }
            if !releasing {
                self.set_pause(&parent.id, crate::PAUSE_WAITING).await;
                releasing = true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

fn principal_of(parent: &Task) -> Principal {
    Principal {
        id: parent.caller.clone(),
        namespace: parent.namespace.clone(),
        scopes: parent.scopes.clone(),
    }
}

fn summary(task: &Task) -> SubagentSummary {
    let policy = task.subagent.as_ref();
    SubagentSummary {
        task_id: task.id.clone(),
        parent_task_id: policy
            .map(|policy| policy.parent_task_id.clone())
            .or_else(|| task.parent_task_id.clone())
            .unwrap_or_default(),
        root_task_id: policy
            .map(|policy| policy.root_task_id.clone())
            .or_else(|| task.root_task_id.clone())
            .unwrap_or_default(),
        definition: policy
            .map(|policy| policy.definition.clone())
            .unwrap_or_default(),
        agent: task.agent.clone(),
        state: task.state,
        background: policy.is_some_and(|policy| policy.background),
        depth: policy.map(|policy| policy.depth).unwrap_or(0),
        closed: task.closed_at.is_some(),
        created_at: task.created_at,
        updated_at: task.updated_at,
    }
}

/// Smallest non-zero ceiling among the Worker policy, the parent task limits,
/// and the frozen parent subagent policy. Zero means "unbounded" when every
/// source is zero.
fn bounded_ceiling(worker: u32, task_limit: u32, parent_policy: u32) -> u32 {
    [worker, task_limit, parent_policy]
        .into_iter()
        .filter(|value| *value > 0)
        .min()
        .unwrap_or(0)
}
