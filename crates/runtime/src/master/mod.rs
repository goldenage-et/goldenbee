//! Durable Master execution plans and objective-evidence evaluation.
//!
//! A Master is an ordinary Worker. This module owns only the durable plan that
//! binds its harness task to remote A2A children, the observed child evidence,
//! and the verification decision. Native A2A remains the child task owner; the
//! Gateway is not involved in plan recovery.

use crate::core::crypto::DelegationClaims;
use crate::core::ids::now;
use crate::peer::{DelegateRequest, DelegatedArtifact, PeerDelegation};
use crate::{ApiError, Result, Runtime};
use common::{
    scopes, CreatePlanRequest, CriterionEvidence, PlanChildRequest, PlanControlRequest,
    PlanCriterion, Principal, Task, TeamRoster,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// Maximum bounded wait accepted by dispatch/recovery.
const MAX_WAIT_MS: u64 = 15_000;
/// Maximum planned children per Master plan.
const MAX_CHILDREN: usize = 16;

/// Durable Master plan projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterPlan {
    /// Stable plan identity.
    pub id: String,
    /// Master harness task that owns the frozen authority.
    pub parent_task_id: String,
    /// Caller namespace.
    pub namespace: String,
    /// Caller principal.
    pub caller: String,
    /// Objective retained across restart.
    pub objective: String,
    /// Canonical request digest used for idempotent creation.
    pub request_digest: String,
    /// Required result criteria.
    pub criteria: Vec<PlanCriterion>,
    /// Planned children and their last observed evidence.
    pub children: Vec<PlanChild>,
    /// Management state: `planned`, `dispatching`, `recovering`, `verified`,
    /// `partial`, `conflicting`, `unverified`, `canceled`, or `escalated`.
    pub state: String,
    /// Frozen team name, when this plan belongs to a Gateway roster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Frozen team version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_version: Option<u64>,
    /// Immutable roster snapshot copied from the Master task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_roster: Option<TeamRoster>,
    /// Package, profile, and instance provenance retained across restart.
    pub provenance: PlanProvenance,
    /// Last evaluation result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<PlanEvaluation>,
    /// Last recovery report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<PlanRecovery>,
    /// Explicit escalation, when requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<PlanEscalation>,
    /// Creation time.
    pub created_at: i64,
    /// Last update time.
    pub updated_at: i64,
}

/// Invocation provenance frozen when the plan was created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanProvenance {
    /// Master Worker Instance identity.
    pub instance: String,
    /// Installed package identity.
    pub package: String,
    /// Installed package version.
    pub package_version: String,
    /// Process incarnation that created the plan.
    pub incarnation: String,
    /// Frozen Runtime Profile name.
    pub profile: String,
    /// Frozen Runtime Profile revision.
    pub profile_revision: u64,
    /// Frozen asset revision.
    pub asset_revision: u64,
}

/// One planned child.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanChild {
    /// Stable child id within the plan.
    pub id: String,
    /// Frozen roster member identity, when this is a team plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// Static peer identity, when this is a peer plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    /// Authorized capability.
    pub capability: String,
    /// Objective sent once.
    pub objective: String,
    /// Whether successful completion is required.
    pub required: bool,
    /// Requested child limits, narrowed again before issuance.
    #[serde(default)]
    pub limits: common::Limits,
    /// Stable delegation idempotency key.
    pub idempotency_key: String,
    /// Durable parent-to-child binding id, once prepared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,
    /// Native remote A2A task identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_task_id: Option<String>,
    /// Last truthful child state.
    pub state: String,
    /// Last observed remote A2A state wire value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_state: Option<String>,
    /// Last observed output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Immutable artifact evidence observed by this Master.
    #[serde(default)]
    pub artifacts: Vec<DelegatedArtifact>,
    /// Last transport or policy error, redacted by construction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<ApiError>,
    /// Last update time.
    pub updated_at: i64,
}

/// One objective criterion result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionResult {
    /// Criterion id.
    pub id: String,
    /// Child id.
    pub child: String,
    /// Evidence requirement kind.
    pub kind: String,
    /// `satisfied`, `missing`, or `conflicting`.
    pub status: String,
    /// Redacted explanation.
    pub detail: String,
    /// Observed evidence used for the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<serde_json::Value>,
}

/// Durable evaluation of every criterion against observed child evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEvaluation {
    /// `verified`, `partial`, `unverified`, or `conflicting`.
    pub outcome: String,
    /// Per-criterion decisions.
    pub criteria: Vec<CriterionResult>,
    /// Criterion ids with missing evidence.
    pub missing: Vec<String>,
    /// Criterion ids with conflicting evidence.
    pub conflicting: Vec<String>,
    /// Required child ids that are not successfully complete.
    pub incomplete: Vec<String>,
    /// Evaluation time.
    pub at: i64,
}

/// Recovery report proving what was queried and what, if anything, was sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRecovery {
    /// Recovery time.
    pub at: i64,
    /// True when the persisted plan context was restored.
    pub restored: bool,
    /// Stored child binding handles queried.
    pub queried: Vec<String>,
    /// Children dispatched because no binding existed before any send.
    pub dispatched: Vec<String>,
    /// Children still uncertain after the bounded wait.
    pub unresolved: Vec<String>,
    /// Replacement children created. Always zero for successful recovery.
    pub duplicates: u32,
}

/// Explicit escalation without widening frozen authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEscalation {
    /// Redacted reason.
    pub reason: String,
    /// Escalation time.
    pub at: i64,
}

impl Runtime {
    /// Create or replay one durable Master plan.
    pub async fn create_master_plan(
        &self,
        principal: &Principal,
        request: CreatePlanRequest,
    ) -> Result<MasterPlan> {
        principal.require(scopes::TASK_SUBMIT)?;
        if request.objective.trim().is_empty() {
            return Err(ApiError::new(
                "plan.objective_invalid",
                "plan objective must not be empty",
            ));
        }
        if request.criteria.is_empty() {
            return Err(ApiError::new(
                "plan.criteria_required",
                "a plan needs at least one required result criterion",
            ));
        }
        if request.children.is_empty() || request.children.len() > MAX_CHILDREN {
            return Err(ApiError::new(
                "plan.children_invalid",
                format!("a plan needs 1-{MAX_CHILDREN} children"),
            ));
        }
        let parent = self.task(principal, &request.parent_task_id).await?;
        if parent.team.is_some() && parent.team_roster.is_none() {
            return Err(ApiError::new(
                "plan.team_roster_missing",
                "a team Master task has no immutable roster snapshot",
            ));
        }
        let mut child_ids = BTreeSet::new();
        for child in &request.children {
            validate_child(child, &parent)?;
            if !child_ids.insert(child.id.as_str()) {
                return Err(ApiError::new(
                    "plan.duplicate_child",
                    format!("child '{}' is declared twice", child.id),
                ));
            }
        }
        let mut criterion_ids = BTreeSet::new();
        for criterion in &request.criteria {
            if !child_ids.contains(criterion.child.as_str()) {
                return Err(ApiError::new(
                    "plan.criterion_child_unknown",
                    format!(
                        "criterion '{}' references an unknown child '{}'",
                        criterion.id, criterion.child
                    ),
                ));
            }
            if !criterion_ids.insert(criterion.id.as_str()) {
                return Err(ApiError::new(
                    "plan.duplicate_criterion",
                    format!("criterion '{}' is declared twice", criterion.id),
                ));
            }
        }

        let canonical = serde_json::to_value(&request)
            .map_err(|error| ApiError::new("plan.invalid", error.to_string()))?;
        let digest = common::canonical_digest(&canonical);
        let id = plan_id(
            &self.config().instance_id,
            &principal.namespace,
            &principal.id,
            &digest,
        );
        if let Some(existing) = self.plan_record(&id).await {
            self.require_plan_access(principal, &existing)?;
            return Ok(existing);
        }
        let timestamp = now();
        let plan = MasterPlan {
            id: id.clone(),
            parent_task_id: parent.id.clone(),
            namespace: principal.namespace.clone(),
            caller: principal.id.clone(),
            objective: request.objective.clone(),
            request_digest: digest,
            criteria: request.criteria.clone(),
            children: request
                .children
                .iter()
                .map(|child| PlanChild {
                    id: child.id.clone(),
                    member: child.member.clone(),
                    peer: child.peer.clone(),
                    capability: child.capability.clone(),
                    objective: child.objective.clone(),
                    required: child.required,
                    limits: child.limits.clone(),
                    idempotency_key: format!("{id}:{}", child.id),
                    binding_id: None,
                    remote_task_id: None,
                    state: "planned".into(),
                    remote_state: None,
                    output: None,
                    artifacts: Vec::new(),
                    last_error: None,
                    updated_at: timestamp,
                })
                .collect(),
            state: "planned".into(),
            team: parent.team.clone(),
            team_version: parent.team_version,
            team_roster: parent.team_roster.clone(),
            provenance: PlanProvenance {
                instance: self.config().instance_id.clone(),
                package: self.config().package.clone(),
                package_version: self.config().package_version.clone(),
                incarnation: self.incarnation().to_string(),
                profile: parent.profile.clone(),
                profile_revision: parent.profile_revision,
                asset_revision: parent.asset_revision,
            },
            evaluation: None,
            recovery: None,
            escalation: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        self.store()
            .write(|state| {
                state.plans.insert(plan.id.clone(), plan.clone());
                Ok(())
            })
            .await?;
        self.audit(
            &principal.id,
            "plan.create",
            &plan.id,
            "planned",
            &format!(
                "parent={} children={} criteria={}",
                plan.parent_task_id,
                plan.children.len(),
                plan.criteria.len()
            ),
        )
        .await?;
        let _ = self
            .append_event(
                &plan.parent_task_id,
                "master.plan_created",
                json!({
                    "plan_id": plan.id,
                    "children": plan.children.iter().map(|child| json!({
                        "id": child.id,
                        "target": child.member.clone().or_else(|| child.peer.clone()),
                        "capability": child.capability,
                    })).collect::<Vec<_>>(),
                    "criteria": plan.criteria,
                }),
                true,
            )
            .await;
        Ok(plan)
    }

    /// List plans visible to the caller.
    pub async fn master_plans(&self, principal: &Principal) -> Result<Vec<MasterPlan>> {
        principal.require(scopes::TASK_READ)?;
        Ok(self
            .store()
            .read(|state| {
                state
                    .plans
                    .values()
                    .filter(|plan| {
                        plan.namespace == principal.namespace && plan.caller == principal.id
                    })
                    .cloned()
                    .collect()
            })
            .await)
    }

    /// Fetch one plan.
    pub async fn master_plan(&self, principal: &Principal, id: &str) -> Result<MasterPlan> {
        principal.require(scopes::TASK_READ)?;
        let plan = self
            .plan_record(id)
            .await
            .ok_or_else(|| ApiError::new("plan.not_found", "plan is not visible"))?;
        self.require_plan_access(principal, &plan)?;
        Ok(plan)
    }

    /// Dispatch every planned child concurrently and return within the bounded
    /// wait. Repeating the call never creates a second child.
    pub async fn dispatch_master_plan(
        &self,
        principal: &Principal,
        claims: Option<DelegationClaims>,
        id: &str,
        control: PlanControlRequest,
    ) -> Result<MasterPlan> {
        principal.require(scopes::TASK_SUBMIT)?;
        let plan = self
            .plan_record(id)
            .await
            .ok_or_else(|| ApiError::new("plan.not_found", "plan is not visible"))?;
        self.require_plan_access(principal, &plan)?;
        if matches!(plan.state.as_str(), "verified" | "canceled" | "escalated") {
            return Ok(plan);
        }
        let dispatch = self
            .store()
            .write(|state| {
                let plan = state
                    .plans
                    .get_mut(id)
                    .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared"))?;
                let mut dispatch = Vec::new();
                let already_started = plan.state == "dispatching" || plan.state == "recovering";
                if !already_started {
                    plan.state = "dispatching".into();
                    plan.updated_at = now();
                    dispatch = plan
                        .children
                        .iter()
                        .filter(|child| !child_terminal(&child.state))
                        .map(|child| child.id.clone())
                        .collect();
                }
                Ok(dispatch)
            })
            .await?;
        for child_id in dispatch {
            let runtime = self.clone();
            let principal = principal.clone();
            let claims = claims.clone();
            let plan_id = id.to_string();
            tokio::spawn(async move {
                runtime
                    .run_plan_child(&principal, claims.as_ref(), &plan_id, &child_id)
                    .await;
            });
        }
        self.await_plan_handles(principal, id, control.wait_ms)
            .await?;
        self.plan_record(id)
            .await
            .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared during dispatch"))
    }

    /// Recover a plan after restart by querying stored child handles. A child
    /// whose binding was never prepared is dispatched exactly once; an existing
    /// handle is reattached, never replaced.
    pub async fn recover_master_plan(
        &self,
        principal: &Principal,
        claims: Option<DelegationClaims>,
        id: &str,
        control: PlanControlRequest,
    ) -> Result<MasterPlan> {
        principal.require(scopes::TASK_SUBMIT)?;
        let plan = self
            .plan_record(id)
            .await
            .ok_or_else(|| ApiError::new("plan.not_found", "plan is not visible"))?;
        self.require_plan_access(principal, &plan)?;
        let mut queried = Vec::new();
        let mut dispatched = Vec::new();
        for child in plan.children.clone() {
            let binding = self.find_plan_binding(&plan, &child).await;
            match binding {
                Some(binding) => {
                    queried.push(binding.id.clone());
                    self.apply_binding(&plan.id, &child.id, &binding).await?;
                    if !binding.terminal() {
                        match self.peer_delegation(principal, &binding.id).await {
                            Ok(refreshed) => {
                                self.apply_binding(&plan.id, &child.id, &refreshed).await?;
                            }
                            Err(error) => {
                                self.record_child_error(&plan.id, &child.id, &error).await?;
                            }
                        }
                    }
                }
                None if plan.state == "dispatching" || plan.state == "recovering" => {
                    // No binding was committed before the crash. The delegate
                    // path persists a binding before send, so no remote effect
                    // can exist for this child.
                    dispatched.push(child.id.clone());
                    self.run_plan_child(principal, claims.as_ref(), &plan.id, &child.id)
                        .await;
                }
                None => {}
            }
        }
        self.await_plan_handles(principal, id, control.wait_ms)
            .await?;
        let unresolved = self
            .plan_record(id)
            .await
            .map(|plan| {
                plan.children
                    .iter()
                    .filter(|child| !child_terminal(&child.state))
                    .map(|child| child.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.store()
            .write(|state| {
                let plan = state
                    .plans
                    .get_mut(id)
                    .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared"))?;
                plan.recovery = Some(PlanRecovery {
                    at: now(),
                    restored: true,
                    queried: queried.clone(),
                    dispatched: dispatched.clone(),
                    unresolved,
                    duplicates: 0,
                });
                plan.updated_at = now();
                Ok(())
            })
            .await?;
        self.refresh_plan_evaluation(principal, id).await?;
        self.audit(
            &principal.id,
            "plan.recover",
            id,
            "restored",
            &format!(
                "queried={} dispatched={} duplicates=0",
                queried.len(),
                dispatched.len()
            ),
        )
        .await?;
        self.plan_record(id)
            .await
            .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared during recovery"))
    }

    /// Re-evaluate observed evidence without dispatching or trusting a new
    /// child claim.
    pub async fn evaluate_master_plan(
        &self,
        principal: &Principal,
        id: &str,
    ) -> Result<MasterPlan> {
        principal.require(scopes::TASK_READ)?;
        let plan = self
            .plan_record(id)
            .await
            .ok_or_else(|| ApiError::new("plan.not_found", "plan is not visible"))?;
        self.require_plan_access(principal, &plan)?;
        self.refresh_plan_evaluation(principal, &plan.id).await?;
        self.plan_record(id)
            .await
            .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared during evaluation"))
    }

    /// Cancel every exact child handle owned by this plan.
    pub async fn cancel_master_plan(&self, principal: &Principal, id: &str) -> Result<MasterPlan> {
        principal.require(scopes::TASK_CANCEL)?;
        let plan = self
            .plan_record(id)
            .await
            .ok_or_else(|| ApiError::new("plan.not_found", "plan is not visible"))?;
        self.require_plan_access(principal, &plan)?;
        for child in plan.children.clone() {
            if child_terminal(&child.state) {
                continue;
            }
            let Some(binding) = self.find_plan_binding(&plan, &child).await else {
                continue;
            };
            match self.cancel_peer_delegation(principal, &binding.id).await {
                Ok(canceled) => self.apply_binding(&plan.id, &child.id, &canceled).await?,
                Err(error) => self.record_child_error(&plan.id, &child.id, &error).await?,
            }
        }
        self.store()
            .write(|state| {
                if let Some(plan) = state.plans.get_mut(id) {
                    plan.state = "canceled".into();
                    plan.updated_at = now();
                }
                Ok(())
            })
            .await?;
        self.audit(
            &principal.id,
            "plan.cancel",
            id,
            "canceled",
            "all exact child handles",
        )
        .await?;
        self.plan_record(id)
            .await
            .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared during cancellation"))
    }

    /// Escalate a partial or uncertain plan explicitly, without changing the
    /// frozen roster or child authority.
    pub async fn escalate_master_plan(
        &self,
        principal: &Principal,
        id: &str,
        reason: String,
    ) -> Result<MasterPlan> {
        principal.require(scopes::TASK_SUBMIT)?;
        if reason.trim().is_empty() || reason.len() > 2_000 {
            return Err(ApiError::new(
                "plan.escalation_invalid",
                "escalation reason must be 1-2000 characters",
            ));
        }
        let plan = self
            .plan_record(id)
            .await
            .ok_or_else(|| ApiError::new("plan.not_found", "plan is not visible"))?;
        self.require_plan_access(principal, &plan)?;
        self.store()
            .write(|state| {
                let plan = state
                    .plans
                    .get_mut(id)
                    .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared"))?;
                plan.escalation = Some(PlanEscalation {
                    reason: reason.clone(),
                    at: now(),
                });
                plan.state = "escalated".into();
                plan.updated_at = now();
                Ok(())
            })
            .await?;
        self.audit(&principal.id, "plan.escalate", id, "escalated", &reason)
            .await?;
        self.plan_record(id)
            .await
            .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared during escalation"))
    }

    async fn run_plan_child(
        &self,
        principal: &Principal,
        claims: Option<&DelegationClaims>,
        plan_id: &str,
        child_id: &str,
    ) {
        let Some(plan) = self.plan_record(plan_id).await else {
            return;
        };
        let Some(child) = plan
            .children
            .iter()
            .find(|child| child.id == child_id)
            .cloned()
        else {
            return;
        };
        if child_terminal(&child.state) {
            return;
        }
        let request = DelegateRequest {
            capability: child.capability.clone(),
            objective: child.objective.clone(),
            parent_task_id: Some(plan.parent_task_id.clone()),
            idempotency_key: Some(child.idempotency_key.clone()),
            limits: child.limits.clone(),
        };
        let outcome = if let Some(member) = &child.member {
            let team = plan.team.clone().unwrap_or_default();
            self.delegate_to_team_member(principal, claims, &team, member, request)
                .await
        } else if let Some(peer) = &child.peer {
            self.delegate_to_peer(principal, claims, peer, request)
                .await
        } else {
            Err(ApiError::new(
                "plan.child_target_missing",
                "planned child has no member or peer target",
            ))
        };
        match outcome {
            Ok(binding) => {
                let _ = self.apply_binding(plan_id, child_id, &binding).await;
                let _ = self
                    .append_event(
                        &plan.parent_task_id,
                        "master.child_observed",
                        json!({
                            "plan_id": plan_id,
                            "child": child_id,
                            "binding": binding.id,
                            "remote_task_id": binding.remote_task_id,
                            "state": binding.state,
                            "artifact_digests": binding.artifacts.iter().map(|artifact| artifact.digest.clone()).collect::<Vec<_>>(),
                        }),
                        true,
                    )
                    .await;
            }
            Err(error) => {
                let _ = self.record_child_error(plan_id, child_id, &error).await;
            }
        }
        let _ = self.refresh_plan_evaluation(principal, plan_id).await;
    }

    async fn await_plan_handles(
        &self,
        principal: &Principal,
        plan_id: &str,
        wait_ms: Option<u64>,
    ) -> Result<()> {
        let deadline =
            Instant::now() + Duration::from_millis(wait_ms.unwrap_or(0).min(MAX_WAIT_MS));
        loop {
            let plan = self
                .plan_record(plan_id)
                .await
                .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared during wait"))?;
            let pending = plan
                .children
                .iter()
                .filter(|child| !child_terminal(&child.state))
                .filter_map(|child| child.binding_id.clone())
                .collect::<Vec<_>>();
            if pending.is_empty() || Instant::now() >= deadline {
                break;
            }
            for binding_id in pending {
                match self.peer_delegation(principal, &binding_id).await {
                    Ok(binding) => {
                        if let Some(child) = plan
                            .children
                            .iter()
                            .find(|child| child.binding_id.as_deref() == Some(binding_id.as_str()))
                        {
                            self.apply_binding(plan_id, &child.id, &binding).await?;
                        }
                    }
                    Err(error) => {
                        if let Some(child) = plan
                            .children
                            .iter()
                            .find(|child| child.binding_id.as_deref() == Some(binding_id.as_str()))
                        {
                            self.record_child_error(plan_id, &child.id, &error).await?;
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.refresh_plan_evaluation(principal, plan_id).await
    }

    async fn find_plan_binding(
        &self,
        plan: &MasterPlan,
        child: &PlanChild,
    ) -> Option<PeerDelegation> {
        if let Some(binding_id) = &child.binding_id {
            if let Some(binding) = self.binding_by_id(binding_id).await {
                return Some(binding);
            }
        }
        self.store()
            .read(|state| {
                state
                    .peer_delegations
                    .values()
                    .find(|binding| {
                        binding.idempotency_key == child.idempotency_key
                            && binding.caller == plan.caller
                            && binding.namespace == plan.namespace
                    })
                    .cloned()
            })
            .await
    }

    async fn binding_by_id(&self, id: &str) -> Option<PeerDelegation> {
        self.store()
            .read(|state| state.peer_delegations.get(id).cloned())
            .await
    }

    async fn apply_binding(
        &self,
        plan_id: &str,
        child_id: &str,
        binding: &PeerDelegation,
    ) -> Result<()> {
        self.store()
            .write(|state| {
                let plan = state
                    .plans
                    .get_mut(plan_id)
                    .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared"))?;
                let child = plan
                    .children
                    .iter_mut()
                    .find(|child| child.id == child_id)
                    .ok_or_else(|| ApiError::new("plan.child_unknown", "child disappeared"))?;
                child.binding_id = Some(binding.id.clone());
                child.remote_task_id = binding.remote_task_id.clone();
                child.remote_state = binding.remote_state.clone();
                child.state = binding.state.clone();
                child.output = binding.output.clone();
                child.artifacts = binding.artifacts.clone();
                child.last_error = binding.last_error.clone();
                child.updated_at = now();
                plan.updated_at = now();
                Ok(())
            })
            .await
    }

    async fn record_child_error(
        &self,
        plan_id: &str,
        child_id: &str,
        error: &ApiError,
    ) -> Result<()> {
        let state = plan_error_state(error);
        self.store()
            .write(|store| {
                let plan = store
                    .plans
                    .get_mut(plan_id)
                    .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared"))?;
                let child = plan
                    .children
                    .iter_mut()
                    .find(|child| child.id == child_id)
                    .ok_or_else(|| ApiError::new("plan.child_unknown", "child disappeared"))?;
                child.state = state.to_string();
                child.last_error = Some(error.clone());
                child.updated_at = now();
                plan.updated_at = now();
                Ok(())
            })
            .await
    }

    async fn refresh_plan_evaluation(&self, principal: &Principal, plan_id: &str) -> Result<()> {
        let visible = self
            .plan_record(plan_id)
            .await
            .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared"))?;
        self.require_plan_access(principal, &visible)?;
        // Evaluate against the plan inside the same write that persists the
        // result. Two children can finish concurrently, so computing from an
        // earlier read and writing later would overwrite newer evidence.
        self.store()
            .write(|state| {
                let plan = state
                    .plans
                    .get_mut(plan_id)
                    .ok_or_else(|| ApiError::new("plan.not_found", "plan disappeared"))?;
                let evaluation = evaluate(plan);
                if plan.escalation.is_some() {
                    plan.state = "escalated".into();
                } else if plan.state == "canceled" {
                    // Cancellation is a management decision, not a success.
                } else if plan.children.iter().any(|child| child_active(&child.state)) {
                    plan.state = if plan.state == "recovering" {
                        "recovering".into()
                    } else {
                        "dispatching".into()
                    };
                } else {
                    plan.state = evaluation.outcome.clone();
                }
                plan.evaluation = Some(evaluation);
                plan.updated_at = now();
                Ok(())
            })
            .await
    }

    async fn plan_record(&self, id: &str) -> Option<MasterPlan> {
        self.store()
            .read(|state| state.plans.get(id).cloned())
            .await
    }

    fn require_plan_access(&self, principal: &Principal, plan: &MasterPlan) -> Result<()> {
        principal.require_namespace(&plan.namespace)?;
        if plan.caller != principal.id && !principal.has(scopes::ADMIN) {
            return Err(ApiError::new(
                "plan.plan_forbidden",
                "plan belongs to another caller",
            ));
        }
        Ok(())
    }
}

fn plan_id(instance: &str, namespace: &str, caller: &str, digest: &str) -> String {
    let mut hasher = Sha256::new();
    for value in [instance, namespace, caller, digest] {
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    let bytes = hasher.finalize();
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("plan-{}", &hex[..32])
}

fn validate_child(child: &PlanChildRequest, parent: &Task) -> Result<()> {
    if child.id.trim().is_empty() || child.id.len() > 100 {
        return Err(ApiError::new(
            "plan.child_id_invalid",
            "child id must be 1-100 characters",
        ));
    }
    if child.capability.trim().is_empty() {
        return Err(ApiError::new(
            "plan.capability_invalid",
            "child capability must not be empty",
        ));
    }
    if child.objective.trim().is_empty() {
        return Err(ApiError::new(
            "plan.child_objective_invalid",
            "child objective must not be empty",
        ));
    }
    match (&child.member, &child.peer) {
        (Some(_), Some(_)) => Err(ApiError::new(
            "plan.child_target_ambiguous",
            "a child must name exactly one member or peer target",
        )),
        (None, None) => Err(ApiError::new(
            "plan.child_target_missing",
            "a child must name a member or peer target",
        )),
        (Some(member), None) => {
            let roster = parent.team_roster.as_ref().ok_or_else(|| {
                ApiError::new(
                    "plan.team_roster_missing",
                    "member delegation requires a frozen roster on the Master task",
                )
            })?;
            let binding = roster.member(member).ok_or_else(|| {
                ApiError::new(
                    "team.member_hidden",
                    format!("Worker '{member}' is not an authorized roster member"),
                )
            })?;
            if !binding
                .capabilities
                .iter()
                .any(|cap| cap == &child.capability)
            {
                return Err(ApiError::new(
                    "peer.capability_denied",
                    format!(
                        "capability '{}' is not authorized for roster member '{member}'",
                        child.capability
                    ),
                ));
            }
            Ok(())
        }
        (None, Some(_)) => Ok(()),
    }
}

fn child_terminal(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "canceled" | "rejected" | "unreachable" | "saturated" | "unknown"
    )
}

fn child_active(state: &str) -> bool {
    matches!(
        state,
        "planned"
            | "dispatching"
            | "prepared"
            | "submitted"
            | "working"
            | "input_required"
            | "auth_required"
    )
}

fn plan_error_state(error: &ApiError) -> &'static str {
    match error.code.as_str() {
        "peer.unreachable" | "peer.unavailable" => "unreachable",
        "peer.saturated"
        | "delegation.fanout_exceeded"
        | "capacity.queue_full"
        | "capacity.principal_quota" => "saturated",
        "delegation.expired" | "delegation.invalid" | "delegation.replayed" => "rejected",
        code if code.starts_with("delegation.")
            || code.starts_with("team.")
            || code.starts_with("scope.")
            || code.starts_with("peer.capability")
            || code.starts_with("peer.identity")
            || code == "peer.not_found" =>
        {
            "rejected"
        }
        _ if error.retryable => "unknown",
        _ => "failed",
    }
}

fn evaluate(plan: &MasterPlan) -> PlanEvaluation {
    let mut results = Vec::new();
    let mut missing = Vec::new();
    let mut conflicting = Vec::new();
    let mut satisfied = 0usize;
    for criterion in &plan.criteria {
        let Some(child) = plan
            .children
            .iter()
            .find(|child| child.id == criterion.child)
        else {
            missing.push(criterion.id.clone());
            results.push(CriterionResult {
                id: criterion.id.clone(),
                child: criterion.child.clone(),
                kind: evidence_kind(&criterion.evidence).into(),
                status: "missing".into(),
                detail: "criterion references no planned child".into(),
                observed: None,
            });
            continue;
        };
        let (status, detail, observed) = evaluate_criterion(criterion, child);
        match status {
            "satisfied" => satisfied += 1,
            "conflicting" => conflicting.push(criterion.id.clone()),
            _ => missing.push(criterion.id.clone()),
        }
        results.push(CriterionResult {
            id: criterion.id.clone(),
            child: child.id.clone(),
            kind: evidence_kind(&criterion.evidence).into(),
            status: status.into(),
            detail,
            observed,
        });
    }
    let incomplete = plan
        .children
        .iter()
        .filter(|child| child.required && child.state != "completed")
        .map(|child| child.id.clone())
        .collect::<Vec<_>>();
    let any_active = plan.children.iter().any(|child| child_active(&child.state));
    let all_satisfied = !plan.criteria.is_empty() && missing.is_empty() && conflicting.is_empty();
    let verified = all_satisfied && incomplete.is_empty() && !any_active;
    let outcome = if !conflicting.is_empty() {
        "conflicting"
    } else if verified {
        "verified"
    } else if satisfied > 0 || plan.children.iter().any(|child| child.state == "completed") {
        "partial"
    } else {
        "unverified"
    };
    PlanEvaluation {
        outcome: outcome.into(),
        criteria: results,
        missing,
        conflicting,
        incomplete,
        at: now(),
    }
}

fn evaluate_criterion(
    criterion: &PlanCriterion,
    child: &PlanChild,
) -> (&'static str, String, Option<serde_json::Value>) {
    let observed = Some(json!({
        "state": child.state,
        "remote_state": child.remote_state,
        "output": child.output,
        "artifacts": child.artifacts,
    }));
    if child.state != "completed" {
        return (
            "missing",
            format!(
                "child '{}' has not completed successfully (state={})",
                child.id, child.state
            ),
            observed,
        );
    }
    match &criterion.evidence {
        CriterionEvidence::OutputContains { text } => {
            if child
                .output
                .as_deref()
                .is_some_and(|output| output.contains(text))
            {
                (
                    "satisfied",
                    "observed child output contains the required text".into(),
                    observed,
                )
            } else {
                (
                    "missing",
                    "observed child output does not contain the required text".into(),
                    observed,
                )
            }
        }
        CriterionEvidence::ArtifactDigest {
            artifact_id,
            digest,
        } => {
            let candidates = child
                .artifacts
                .iter()
                .filter(|artifact| {
                    artifact_id
                        .as_deref()
                        .is_none_or(|id| artifact.artifact_id == id)
                })
                .collect::<Vec<_>>();
            let Some(artifact) = candidates.first() else {
                return (
                    "missing",
                    "the child reported success without an observable artifact".into(),
                    observed,
                );
            };
            if !artifact.digest.starts_with("sha256:") || artifact.size == 0 {
                return (
                    "missing",
                    "the observed artifact has no verifiable digest or content".into(),
                    observed,
                );
            }
            if let Some(expected) = digest {
                if expected != &artifact.digest {
                    return (
                        "conflicting",
                        format!(
                            "observed artifact digest '{}' differs from expected '{}'",
                            artifact.digest, expected
                        ),
                        observed,
                    );
                }
            }
            (
                "satisfied",
                "observed immutable artifact digest is present and consistent".into(),
                observed,
            )
        }
    }
}

fn evidence_kind(evidence: &CriterionEvidence) -> &'static str {
    match evidence {
        CriterionEvidence::OutputContains { .. } => "output_contains",
        CriterionEvidence::ArtifactDigest { .. } => "artifact_digest",
    }
}
