//! Static peer resolution, narrowed child delegation, and durable child
//! bindings.
//!
//! The transport is the pinned A2A protocol; this module owns only the
//! authorization decision, the child claims, and the parent-to-child binding
//! that lets a restarted parent query or cancel the same remote task without
//! resubmitting the objective.

use crate::core::crypto::{issue_delegation, DelegationClaims};
use crate::peer::config::{PeerConfig, PeerRecord};
use crate::peer::transport::{PeerTransport, VerifiedPeer};
use crate::{ApiError, Runtime};
use a2a_protocol_types::artifact::Artifact;
use a2a_protocol_types::message::{MessageRole, PartContent};
use a2a_protocol_types::task::TaskState as RemoteTaskState;
use a2a_protocol_types::{ContextId, Message, MessageId, Task as RemoteTask};
use common::{scopes, Limits, Principal, Task, TeamMemberGrant, TeamRoster};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Durable parent-to-child delegation binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerDelegation {
    /// Stable local binding id.
    pub id: String,
    /// Frozen static peer id or derived team-member transport id.
    pub peer: String,
    /// Frozen team name, when this child came from a process-level roster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Frozen team version, when this child came from a process-level roster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_version: Option<u64>,
    /// Resolved roster member binding used for reattachment and cancellation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_member: Option<TeamMemberGrant>,
    /// Immutable roster snapshot authorized for this child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_roster: Option<TeamRoster>,
    /// Expected remote instance identity.
    pub peer_identity: String,
    /// Caller-scoped idempotency key.
    pub idempotency_key: String,
    /// Objective text sent once as the native A2A message.
    pub objective: String,
    /// Requested and authorized capability.
    pub capability: String,
    /// Parent harness task, when correlated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    /// Parent Worker Instance identity.
    pub parent_instance: String,
    /// Trace identity propagated from the parent execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Root identity for the delegation path.
    pub root_id: String,
    /// Delegation path, including both instance identities.
    pub path: Vec<String>,
    /// Child delegation depth.
    pub depth: u32,
    /// Child maximum depth.
    pub max_depth: u32,
    /// Effective narrowed limits.
    pub limits: Limits,
    /// Effective narrowed scopes.
    pub scopes: Vec<String>,
    /// Effective child capabilities. Always one entry in this slice.
    pub capabilities: Vec<String>,
    /// Original parent authority expiry, when the parent was itself delegated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_expires_at: Option<i64>,
    /// Deterministic remote message id.
    pub message_id: String,
    /// Deterministic remote context id.
    pub context_id: String,
    /// Remote A2A task identity, once confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_task_id: Option<String>,
    /// Remote A2A state wire value, once confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_state: Option<String>,
    /// Last observed remote output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Immutable artifact evidence observed on the remote native task. Digests
    /// are computed from the exact A2A text parts received by this Worker.
    #[serde(default)]
    pub artifacts: Vec<DelegatedArtifact>,
    /// Local truthful lifecycle: `prepared`, `submitted`, `working`,
    /// `completed`, `failed`, `canceled`, `rejected`, `unknown`.
    pub state: String,
    /// Caller principal that owns this binding.
    pub caller: String,
    /// Caller namespace.
    pub namespace: String,
    /// Last transport or remote error, never a credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<ApiError>,
    /// Creation time.
    pub created_at: i64,
    /// Last update time.
    pub updated_at: i64,
}

impl PeerDelegation {
    /// Whether the remote task can no longer change state here.
    pub fn terminal(&self) -> bool {
        matches!(
            self.state.as_str(),
            "completed" | "failed" | "canceled" | "rejected"
        )
    }
}

/// Immutable evidence observed on a remote native A2A task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedArtifact {
    /// Remote native artifact identity.
    pub artifact_id: String,
    /// Optional remote artifact name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// SHA-256 of the exact text content received by the parent.
    pub digest: String,
    /// Text byte length observed by the parent.
    pub size: u64,
}

/// One delegate request. The capability is always explicit so the parent never
/// guesses which remote skill receives the objective.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegateRequest {
    /// Authorized capability to invoke.
    pub capability: String,
    /// Objective text.
    pub objective: String,
    /// Optional parent harness task id for correlation.
    #[serde(default)]
    pub parent_task_id: Option<String>,
    /// Caller idempotency key. A stable value lets a restart reattach.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Requested resource limits. They are narrowed before issuance.
    #[serde(default)]
    pub limits: Limits,
}

impl Runtime {
    /// Configured authorized peers with their last verified identity and
    /// effective capabilities. Unknown peers never appear.
    pub async fn peer_records(&self, principal: &Principal) -> crate::Result<Vec<PeerRecord>> {
        principal.require(scopes::TASK_READ)?;
        let configs = self.config().peers.clone();
        let cached = self.store().read(|state| state.peer_records.clone()).await;
        Ok(configs
            .iter()
            .map(|(id, config)| {
                cached
                    .get(id)
                    .filter(|record| {
                        record.identity == config.identity
                            && record.endpoint.trim_end_matches('/')
                                == config.endpoint.trim_end_matches('/')
                    })
                    .cloned()
                    .unwrap_or_else(|| PeerRecord::configured(id, config))
            })
            .collect())
    }

    /// One configured peer record.
    pub async fn peer_record(
        &self,
        principal: &Principal,
        peer_id: &str,
    ) -> crate::Result<PeerRecord> {
        principal.require(scopes::TASK_READ)?;
        let config = self.peer_config(peer_id)?;
        let cached = self
            .store()
            .read(|state| state.peer_records.get(peer_id).cloned())
            .await
            .filter(|record| {
                record.identity == config.identity
                    && record.endpoint.trim_end_matches('/')
                        == config.endpoint.trim_end_matches('/')
            });
        Ok(cached.unwrap_or_else(|| PeerRecord::configured(peer_id, &config)))
    }

    /// Resolve and verify one peer before any delegation or management action.
    pub async fn resolve_peer(
        &self,
        principal: &Principal,
        peer_id: &str,
    ) -> crate::Result<PeerRecord> {
        principal.require(scopes::TASK_READ)?;
        let config = self.peer_config(peer_id)?;
        self.verify_peer(peer_id, &config).await?;
        self.peer_record(principal, peer_id).await
    }

    /// Delegate one objective to a static authorized peer.
    pub async fn delegate_to_peer(
        &self,
        principal: &Principal,
        claims: Option<&DelegationClaims>,
        peer_id: &str,
        request: DelegateRequest,
    ) -> crate::Result<PeerDelegation> {
        let config = self.peer_config(peer_id)?;
        config.validate(peer_id, self.config().allow_remote)?;
        self.delegate_resolved(principal, claims, peer_id, request, config, None)
            .await
    }

    /// Delegate to an authorized member of the parent task's frozen roster.
    ///
    /// This path does not use Gateway discovery or a static peer allow-list.
    /// Authorization comes from the signed roster snapshot stored on the
    /// parent task, and every child is narrowed again against the parent's
    /// limits, scopes, depth, and delegation path.
    pub async fn delegate_to_team_member(
        &self,
        principal: &Principal,
        claims: Option<&DelegationClaims>,
        team: &str,
        member_id: &str,
        request: DelegateRequest,
    ) -> crate::Result<PeerDelegation> {
        principal.require(scopes::TASK_SUBMIT)?;
        let parent_id = request.parent_task_id.as_deref().ok_or_else(|| {
            ApiError::new(
                "team.parent_required",
                "team delegation requires the Master's parent task",
            )
        })?;
        let parent = self.task(principal, parent_id).await?;
        if parent.team.as_deref() != Some(team) {
            return Err(ApiError::new(
                "team.mismatch",
                "the parent task does not belong to this team",
            )
            .with("team", team.to_string()));
        }
        let roster = parent.team_roster.clone().ok_or_else(|| {
            ApiError::new(
                "team.membership_missing",
                "the parent task has no immutable team roster",
            )
        })?;
        let member = roster.member(member_id).cloned().ok_or_else(|| {
            ApiError::new(
                "team.member_hidden",
                format!("Worker '{member_id}' is not an authorized roster member"),
            )
            .with("team", roster.name.clone())
            .with("worker", member_id.to_string())
        })?;
        if member.instance == self.config().instance_id
            || parent
                .delegation_path
                .iter()
                .any(|instance| instance == &member.instance)
        {
            return Err(ApiError::new(
                "delegation.cycle",
                "delegation path already contains this Worker instance",
            )
            .with("team", roster.name.clone())
            .with("worker", member.worker.clone()));
        }
        let parent_depth = claims
            .map(|claims| claims.depth)
            .unwrap_or_else(|| parent.delegation_path.len().saturating_sub(1) as u32);
        let child_depth = parent_depth.saturating_add(1);
        if child_depth > roster.limits.max_depth
            || (member.max_depth > 0 && child_depth > member.max_depth)
        {
            return Err(ApiError::new(
                "delegation.depth_exceeded",
                "the team roster has no remaining delegation depth",
            )
            .with("team", roster.name.clone())
            .with("depth", child_depth.to_string())
            .with("max_depth", roster.limits.max_depth.to_string()));
        }
        let mut config = PeerConfig {
            identity: member.instance.clone(),
            endpoint: member.endpoint.clone(),
            capabilities: member.capabilities.clone(),
            token_ttl_ms: 30_000,
            max_depth: roster.limits.max_depth.saturating_add(1).min(16),
            max_fanout: roster.limits.max_fanout,
            limits: roster.limits.clone(),
        };
        config.max_depth = config.max_depth.max(child_depth.saturating_add(1)).min(16);
        let peer_id = team_peer_id(&roster.name, &member.worker);
        config.validate(&peer_id, self.config().allow_remote)?;
        let context = TeamContext {
            roster,
            parent,
            member,
        };
        self.delegate_resolved(principal, claims, &peer_id, request, config, Some(context))
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn delegate_resolved(
        &self,
        principal: &Principal,
        claims: Option<&DelegationClaims>,
        peer_id: &str,
        request: DelegateRequest,
        config: PeerConfig,
        team: Option<TeamContext>,
    ) -> crate::Result<PeerDelegation> {
        principal.require(scopes::TASK_SUBMIT)?;
        if request.objective.trim().is_empty() {
            return Err(ApiError::new(
                "peer.objective_invalid",
                "delegated objective must not be empty",
            ));
        }
        let key = request.idempotency_key.clone().unwrap_or_else(|| {
            stable_key(
                &self.config().instance_id,
                &principal.namespace,
                &principal.id,
                peer_id,
                request.parent_task_id.as_deref().unwrap_or(""),
                &request.capability,
                &request.objective,
            )
        });
        let id = binding_id(
            &self.config().instance_id,
            &principal.namespace,
            &principal.id,
            peer_id,
            &key,
        );

        if let Some(existing) = self.binding(&id).await {
            self.require_binding_access(principal, &existing)?;
            if existing.objective != request.objective || existing.capability != request.capability
            {
                return Err(ApiError::new(
                    "peer.idempotency_conflict",
                    "idempotency key was already used for another objective or capability",
                ));
            }
            return self.reattach_binding(existing).await;
        }

        if let Some(parent_id) = &request.parent_task_id {
            self.task(principal, parent_id).await?;
        }
        let verified = self.verify_peer(peer_id, &config).await?;
        let authority = self
            .narrowed_authority(
                principal,
                claims,
                peer_id,
                &config,
                &request,
                &verified,
                team.as_ref(),
            )
            .await?;
        let token = self.child_token(&authority, &config, &id, team.as_ref())?;
        let trace_id = match &request.parent_task_id {
            Some(parent) => self
                .store()
                .read(|state| state.tasks.get(parent).map(|task| task.trace_id.clone()))
                .await
                .filter(|trace| !trace.is_empty())
                .unwrap_or_else(|| common::trace_id(None)),
            None => common::trace_id(None),
        };
        let message_id = format!("{id}-m1");
        let context_id = format!("{id}-ctx");
        let bound = PeerDelegation {
            id: id.clone(),
            peer: peer_id.to_string(),
            team: team.as_ref().map(|team| team.roster.name.clone()),
            team_version: team.as_ref().map(|team| team.roster.version),
            team_member: team.as_ref().map(|team| team.member.clone()),
            team_roster: team.as_ref().map(|team| team.roster.clone()),
            peer_identity: config.identity.clone(),
            idempotency_key: key,
            objective: request.objective.clone(),
            capability: request.capability.clone(),
            parent_task_id: request.parent_task_id.clone(),
            parent_instance: self.config().instance_id.clone(),
            trace_id: Some(trace_id.clone()),
            root_id: authority.root_id.clone(),
            path: authority.path.clone(),
            depth: authority.depth,
            max_depth: authority.max_depth,
            limits: authority.limits.clone(),
            scopes: authority.scopes.clone(),
            capabilities: vec![request.capability.clone()],
            authority_expires_at: authority.authority_expires_at,
            message_id: message_id.clone(),
            context_id: context_id.clone(),
            remote_task_id: None,
            remote_state: None,
            output: None,
            artifacts: Vec::new(),
            state: "prepared".into(),
            caller: principal.id.clone(),
            namespace: principal.namespace.clone(),
            last_error: None,
            created_at: common::now_ms(),
            updated_at: common::now_ms(),
        };
        self.store_binding(bound.clone()).await?;
        self.audit(
            &principal.id,
            "peer.delegate",
            &id,
            "prepared",
            &format!("peer={peer_id} capability={}", request.capability),
        )
        .await?;

        let transport = self.peer_transport(&config)?;
        let message = delegation_message(&bound, &token);
        match transport.send_message(&token, message).await {
            Ok(task) => {
                // The remote accepted the objective. A crash here must be
                // recovered by context/task query, never by a fresh submission.
                crate::core::fault::exit_if(crate::core::fault::Point::AfterPeerSend);
                let mut bound = self.binding(&id).await.unwrap_or(bound);
                apply_remote_task(&mut bound, &task);
                self.store_binding(bound.clone()).await?;
                Ok(bound)
            }
            Err(error) => {
                // A lost acknowledgment is not a definitive failure. Reattach
                // through the deterministic context before reporting anything.
                if let Ok(recovered) = self
                    .reattach_by_context(bound.clone(), &transport, &token)
                    .await
                {
                    return Ok(recovered);
                }
                let mut bound = self.binding(&id).await.unwrap_or(bound);
                bound.last_error = Some(error.clone());
                bound.state = if error.retryable {
                    "unknown".into()
                } else {
                    "failed".into()
                };
                bound.updated_at = common::now_ms();
                self.store_binding(bound).await?;
                Err(error)
            }
        }
    }

    /// List this caller's delegation bindings.
    pub async fn peer_delegations(
        &self,
        principal: &Principal,
    ) -> crate::Result<Vec<PeerDelegation>> {
        principal.require(scopes::TASK_READ)?;
        Ok(self
            .store()
            .read(|state| {
                state
                    .peer_delegations
                    .values()
                    .filter(|binding| {
                        binding.namespace == principal.namespace && binding.caller == principal.id
                    })
                    .cloned()
                    .collect()
            })
            .await)
    }

    /// Query or reattach the existing child. A successful call never submits a
    /// second objective.
    pub async fn peer_delegation(
        &self,
        principal: &Principal,
        id: &str,
    ) -> crate::Result<PeerDelegation> {
        principal.require(scopes::TASK_READ)?;
        let binding = self.binding(id).await.ok_or_else(|| {
            ApiError::new("peer.delegation_not_found", "delegation is not visible")
        })?;
        self.require_binding_access(principal, &binding)?;
        if binding.terminal() {
            return Ok(binding);
        }
        self.reattach_binding(binding).await
    }

    /// Reattach a binding after a broken stream or process restart.
    pub async fn recover_peer_delegation(
        &self,
        principal: &Principal,
        id: &str,
    ) -> crate::Result<PeerDelegation> {
        self.peer_delegation(principal, id).await
    }

    /// Cancel the exact remote task. The remote task id is never guessed.
    pub async fn cancel_peer_delegation(
        &self,
        principal: &Principal,
        id: &str,
    ) -> crate::Result<PeerDelegation> {
        principal.require(scopes::TASK_CANCEL)?;
        let mut binding = self.binding(id).await.ok_or_else(|| {
            ApiError::new("peer.delegation_not_found", "delegation is not visible")
        })?;
        self.require_binding_access(principal, &binding)?;
        if binding.state == "canceled" {
            return Ok(binding);
        }
        if binding.terminal() {
            // Truthful: a completed/failed remote task is not cancelable.
            return Ok(binding);
        }
        let config = self.binding_config(&binding)?;
        let transport = self.peer_transport(&config)?;
        let token = self.binding_token(&binding, &config)?;
        if binding.remote_task_id.is_none() {
            binding = self
                .reattach_by_context(binding, &transport, &token)
                .await?;
        }
        let remote_id = binding.remote_task_id.clone().ok_or_else(|| {
            ApiError::new(
                "peer.task_unavailable",
                "the remote task has no confirmed identity to cancel yet",
            )
            .with("delegation", binding.id.clone())
        })?;
        match transport.cancel_task(&token, &remote_id).await {
            Ok(task) => {
                apply_remote_task(&mut binding, &task);
                binding.last_error = None;
                self.store_binding(binding.clone()).await?;
                self.audit(
                    &principal.id,
                    "peer.cancel",
                    &binding.id,
                    "canceled",
                    &format!("remote={remote_id}"),
                )
                .await?;
                Ok(binding)
            }
            Err(error) if error.code == "peer.task_not_cancelable" => {
                // The native state machine refused; report its authoritative
                // state rather than claiming a cancellation.
                let refreshed = self
                    .reattach_by_context(binding, &transport, &token)
                    .await?;
                Ok(refreshed)
            }
            Err(error) => {
                binding.last_error = Some(error.clone());
                binding.updated_at = common::now_ms();
                self.store_binding(binding).await?;
                Err(error)
            }
        }
    }

    fn peer_config(&self, peer_id: &str) -> crate::Result<PeerConfig> {
        self.config().peers.get(peer_id).cloned().ok_or_else(|| {
            ApiError::new(
                "peer.not_found",
                format!("peer '{peer_id}' is not an explicitly authorized static peer"),
            )
            .with("peer", peer_id.to_string())
        })
    }

    fn binding_config(&self, binding: &PeerDelegation) -> crate::Result<PeerConfig> {
        match (&binding.team_member, &binding.team_roster) {
            (Some(member), Some(roster)) => Ok(PeerConfig {
                identity: member.instance.clone(),
                endpoint: member.endpoint.clone(),
                capabilities: member.capabilities.clone(),
                token_ttl_ms: 30_000,
                max_depth: roster.limits.max_depth.saturating_add(1).min(16),
                max_fanout: roster.limits.max_fanout,
                limits: roster.limits.clone(),
            }),
            _ => self.peer_config(&binding.peer),
        }
    }

    fn peer_transport(&self, config: &PeerConfig) -> crate::Result<PeerTransport> {
        let base = config.base_url()?;
        PeerTransport::new(&base, config.limits.max_duration_ms.max(10_000))
    }

    async fn verify_peer(&self, peer_id: &str, config: &PeerConfig) -> crate::Result<VerifiedPeer> {
        config.validate(peer_id, self.config().allow_remote)?;
        let transport = self.peer_transport(config)?;
        let verified = transport.verify().await?;
        if verified.instance != config.identity {
            return Err(ApiError::new(
                "peer.identity_mismatch",
                format!(
                    "peer '{peer_id}' reported instance '{}' but '{}' is authorized",
                    verified.instance, config.identity
                ),
            )
            .with("expected", config.identity.clone())
            .with("observed", verified.instance));
        }
        let approved: BTreeSet<&str> = config.capabilities.iter().map(String::as_str).collect();
        let advertised: BTreeSet<&str> = verified.capabilities.iter().map(String::as_str).collect();
        let effective = approved
            .intersection(&advertised)
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>();
        if effective.is_empty() {
            return Err(ApiError::new(
                "peer.capability_unavailable",
                format!("peer '{peer_id}' advertises no capability this operator authorized"),
            )
            .with("authorized", config.capabilities.join(","))
            .with("advertised", verified.capabilities.join(",")));
        }
        let record = PeerRecord {
            id: peer_id.to_string(),
            identity: config.identity.clone(),
            endpoint: config.endpoint.trim_end_matches('/').to_string(),
            instance: Some(verified.instance.clone()),
            package: Some(verified.card.name.clone()),
            package_version: Some(verified.card.version.clone()),
            advertised_capabilities: verified.capabilities.clone(),
            capabilities: effective,
            tls: config.tls()?,
            verified_at: Some(common::now_ms()),
            status: "verified".into(),
        };
        self.store()
            .write(|state| {
                state.peer_records.insert(peer_id.to_string(), record);
                Ok(())
            })
            .await?;
        Ok(verified)
    }

    #[allow(clippy::too_many_arguments)]
    async fn narrowed_authority(
        &self,
        principal: &Principal,
        claims: Option<&DelegationClaims>,
        peer_id: &str,
        config: &PeerConfig,
        request: &DelegateRequest,
        verified: &VerifiedPeer,
        team: Option<&TeamContext>,
    ) -> crate::Result<Authority> {
        let parent_capabilities: BTreeSet<String> = match (team, claims) {
            (Some(team), _) => team.roster.authorized_capabilities().into_iter().collect(),
            (None, Some(claims)) => claims.capabilities.iter().cloned().collect(),
            (None, None) => self
                .config()
                .capabilities
                .iter()
                .map(|capability| capability.id.clone())
                .collect(),
        };
        if !parent_capabilities.contains(&request.capability) {
            return Err(ApiError::new(
                "delegation.capability_escalation",
                format!(
                    "capability '{}' is outside the parent's effective authority",
                    request.capability
                ),
            )
            .with("capability", request.capability.clone()));
        }
        if !config.capabilities.contains(&request.capability) {
            return Err(ApiError::new(
                "peer.capability_denied",
                format!(
                    "capability '{}' is not authorized for peer '{peer_id}'",
                    request.capability
                ),
            )
            .with("authorized", config.capabilities.join(",")));
        }
        if !verified.capabilities.contains(&request.capability) {
            return Err(ApiError::new(
                "peer.capability_unavailable",
                format!(
                    "peer '{peer_id}' does not advertise capability '{}'",
                    request.capability
                ),
            )
            .with("advertised", verified.capabilities.join(",")));
        }
        let child_depth = match team {
            Some(team) => {
                team.parent
                    .delegation_path
                    .len()
                    .saturating_sub(1)
                    .max(claims.map(|claims| claims.depth as usize).unwrap_or(0))
                    as u32
                    + 1
            }
            None => claims.map(|claims| claims.depth + 1).unwrap_or(0),
        };
        let mut max_depth = config.max_depth;
        if claims.is_some_and(|claims| claims.max_depth < max_depth) {
            max_depth = claims.map(|claims| claims.max_depth).unwrap_or(max_depth);
        }
        if request.limits.max_depth > 0 && request.limits.max_depth < max_depth {
            max_depth = request.limits.max_depth;
        }
        if claims.is_some() && child_depth >= max_depth {
            return Err(ApiError::new(
                "delegation.depth_exceeded",
                "parent delegation has no remaining depth for a child",
            )
            .with("child_depth", child_depth.to_string())
            .with("max_depth", max_depth.to_string()));
        }
        let mut max_fanout = config.max_fanout;
        if claims.is_some_and(|claims| {
            claims.limits.max_fanout > 0 && claims.limits.max_fanout < max_fanout
        }) {
            max_fanout = claims
                .map(|claims| claims.limits.max_fanout)
                .unwrap_or(max_fanout);
        }
        if request.limits.max_fanout > 0 && request.limits.max_fanout < max_fanout {
            max_fanout = request.limits.max_fanout;
        }
        let active = self
            .store()
            .read(|state| {
                state
                    .peer_delegations
                    .values()
                    .filter(|binding| {
                        let same_target = match team {
                            Some(team) => {
                                binding.team.as_deref() == Some(team.roster.name.as_str())
                                    && binding.parent_task_id == request.parent_task_id
                            }
                            None => binding.peer == peer_id,
                        };
                        same_target
                            && !binding.terminal()
                            && binding.root_id
                                == request
                                    .parent_task_id
                                    .clone()
                                    .unwrap_or_else(|| self.config().instance_id.clone())
                    })
                    .count()
            })
            .await;
        if active >= max_fanout as usize {
            return Err(ApiError::retryable(
                "delegation.fanout_exceeded",
                format!("peer '{peer_id}' already has {active} active delegated children"),
            )
            .with("max_fanout", max_fanout.to_string()));
        }
        let mut limits = request.limits.narrower(&config.limits);
        if let Some(claims) = claims {
            limits = limits.narrower(&claims.limits);
        }
        if let Some(team) = team {
            limits = limits.narrower(&team.parent.limits);
        }
        limits.max_depth = max_depth;
        limits.max_fanout = max_fanout;
        if let Some(team) = team {
            // The token verifier uses `depth >= max_depth`, so token depth is
            // translated by one. The caller-visible budget keeps the roster's
            // policy cap and never exceeds the parent task.
            limits.max_depth = narrower_nonzero(limits.max_depth, team.roster.limits.max_depth);
            limits.max_depth = narrower_nonzero(limits.max_depth, team.parent.limits.max_depth);
            limits.max_fanout = narrower_nonzero(limits.max_fanout, team.parent.limits.max_fanout);
        }
        let mut expires_at = match claims {
            Some(claims) => (common::now_ms() + config.token_ttl_ms as i64).min(claims.expires_at),
            None => common::now_ms() + config.token_ttl_ms as i64,
        };
        if let Some(team) = team {
            if let Some(parent_expiry) = team.parent.authority_expires_at {
                expires_at = expires_at.min(parent_expiry);
            }
        }
        if expires_at <= common::now_ms() {
            return Err(ApiError::new(
                "delegation.expired",
                "parent delegation authority has expired",
            ));
        }
        let scopes = match (team, claims) {
            (Some(team), _) => team.parent.scopes.clone(),
            (None, Some(claims)) => claims.scopes.clone(),
            (None, None) => principal.scopes.clone(),
        };
        if !scopes
            .iter()
            .any(|scope| scope == "*" || scope == scopes::TASK_SUBMIT)
        {
            return Err(ApiError::new(
                "scope.denied",
                "parent authority cannot submit a delegated task",
            ));
        }
        let root_id = claims
            .map(|claims| claims.token_id.clone())
            .or_else(|| request.parent_task_id.clone())
            .unwrap_or_else(|| self.config().instance_id.clone());
        let (authority_principal, authority_namespace) = match team {
            Some(team) => (team.parent.caller.clone(), team.parent.namespace.clone()),
            None => (principal.id.clone(), principal.namespace.clone()),
        };
        let mut path = match team {
            Some(team) => team.parent.delegation_path.clone(),
            None => claims
                .map(|claims| {
                    if claims.path.is_empty() {
                        vec![claims.issuer.clone()]
                    } else {
                        claims.path.clone()
                    }
                })
                .unwrap_or_else(|| vec![self.config().instance_id.clone()]),
        };
        path.push(config.identity.clone());
        Ok(Authority {
            principal: authority_principal,
            namespace: authority_namespace,
            capability: request.capability.clone(),
            scopes,
            depth: child_depth,
            max_depth,
            limits,
            root_id,
            path,
            authority_expires_at: Some(expires_at),
        })
    }

    fn child_token(
        &self,
        authority: &Authority,
        config: &PeerConfig,
        id: &str,
        team: Option<&TeamContext>,
    ) -> crate::Result<String> {
        let claims = DelegationClaims {
            issuer: self.config().instance_id.clone(),
            audience: config.identity.clone(),
            principal: authority.principal.clone(),
            namespace: authority.namespace.clone(),
            scopes: authority.scopes.clone(),
            capabilities: vec![authority.capability.clone()],
            expires_at: authority
                .authority_expires_at
                .map(|expiry| expiry.min(common::now_ms() + config.token_ttl_ms as i64))
                .unwrap_or_else(|| common::now_ms() + config.token_ttl_ms as i64),
            depth: authority.depth,
            max_depth: authority.max_depth,
            limits: authority.limits.clone(),
            token_id: format!("{id}-token"),
            team: team.map(|team| team.roster.clone()),
            path: authority.path.clone(),
        };
        self.issue_child_token(&claims)
    }

    fn issue_child_token(&self, claims: &DelegationClaims) -> crate::Result<String> {
        issue_delegation(&self.config().delegation_secret, claims)
    }

    fn binding_token(
        &self,
        binding: &PeerDelegation,
        config: &PeerConfig,
    ) -> crate::Result<String> {
        let claims = DelegationClaims {
            issuer: self.config().instance_id.clone(),
            audience: config.identity.clone(),
            principal: binding.caller.clone(),
            namespace: binding.namespace.clone(),
            scopes: binding.scopes.clone(),
            capabilities: binding.capabilities.clone(),
            expires_at: binding
                .authority_expires_at
                .map(|expiry| expiry.min(common::now_ms() + config.token_ttl_ms as i64))
                .unwrap_or_else(|| common::now_ms() + config.token_ttl_ms as i64),
            depth: binding.depth,
            max_depth: binding.max_depth,
            limits: binding.limits.clone(),
            token_id: format!("{}-op-{}", binding.id, uuid::Uuid::new_v4().simple()),
            team: binding.team_roster.clone(),
            path: binding.path.clone(),
        };
        if claims.expires_at <= common::now_ms() {
            return Err(ApiError::new(
                "delegation.expired",
                "the stored delegation authority has expired",
            ));
        }
        self.issue_child_token(&claims)
    }

    async fn reattach_binding(&self, binding: PeerDelegation) -> crate::Result<PeerDelegation> {
        let config = self.binding_config(&binding)?;
        let transport = self.peer_transport(&config)?;
        let token = self.binding_token(&binding, &config)?;
        if let Some(remote_id) = &binding.remote_task_id {
            if let Ok(task) = transport.get_task(&token, remote_id, 20).await {
                let mut binding = binding;
                apply_remote_task(&mut binding, &task);
                self.store_binding(binding.clone()).await?;
                return Ok(binding);
            }
        }
        self.reattach_by_context(binding, &transport, &token).await
    }

    async fn reattach_by_context(
        &self,
        mut binding: PeerDelegation,
        transport: &PeerTransport,
        token: &str,
    ) -> crate::Result<PeerDelegation> {
        match transport.list_tasks(token, &binding.context_id).await {
            Ok(tasks) => {
                if let Some(task) = tasks
                    .iter()
                    .find(|task| task.context_id.0 == binding.context_id)
                {
                    apply_remote_task(&mut binding, task);
                    binding.last_error = None;
                    self.store_binding(binding.clone()).await?;
                    return Ok(binding);
                }
                if let Some(remote_id) = &binding.remote_task_id {
                    if let Ok(task) = transport.get_task(token, remote_id, 20).await {
                        apply_remote_task(&mut binding, &task);
                        binding.last_error = None;
                        self.store_binding(binding.clone()).await?;
                        return Ok(binding);
                    }
                }
                Ok(binding)
            }
            Err(error) => {
                binding.last_error = Some(error.clone());
                binding.updated_at = common::now_ms();
                self.store_binding(binding).await?;
                Err(error)
            }
        }
    }

    async fn binding(&self, id: &str) -> Option<PeerDelegation> {
        self.store()
            .read(|state| state.peer_delegations.get(id).cloned())
            .await
    }

    async fn store_binding(&self, binding: PeerDelegation) -> crate::Result<()> {
        self.store()
            .write(|state| {
                state
                    .peer_delegations
                    .insert(binding.id.clone(), binding.clone());
                Ok(())
            })
            .await
    }

    fn require_binding_access(
        &self,
        principal: &Principal,
        binding: &PeerDelegation,
    ) -> crate::Result<()> {
        principal.require_namespace(&binding.namespace)?;
        if binding.caller != principal.id {
            return Err(ApiError::new(
                "peer.delegation_forbidden",
                "delegation belongs to another caller",
            ));
        }
        Ok(())
    }
}

struct Authority {
    principal: String,
    namespace: String,
    capability: String,
    scopes: Vec<String>,
    depth: u32,
    max_depth: u32,
    limits: Limits,
    root_id: String,
    path: Vec<String>,
    authority_expires_at: Option<i64>,
}

struct TeamContext {
    roster: TeamRoster,
    parent: Task,
    member: TeamMemberGrant,
}

fn narrower_nonzero(current: u32, ceiling: u32) -> u32 {
    match (current, ceiling) {
        (0, ceiling) => ceiling,
        (current, 0) => current,
        (current, ceiling) => current.min(ceiling),
    }
}

fn team_peer_id(team: &str, worker: &str) -> String {
    let mut hasher = Sha256::new();
    for value in [team, worker] {
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("team-{}", &digest[..24])
}

fn delegation_message(binding: &PeerDelegation, _token: &str) -> Message {
    let metadata = serde_json::json!({
        "goldenbee.capability": binding.capability,
        "goldenbee.delegation_id": binding.id,
        "goldenbee.parent_instance": binding.parent_instance,
        "goldenbee.parent_task": binding.parent_task_id,
        "goldenbee.trace_id": binding.trace_id,
        "goldenbee.root_id": binding.root_id,
        "goldenbee.path": binding.path,
        "goldenbee.depth": binding.depth,
        "goldenbee.max_depth": binding.max_depth,
        "goldenbee.limits": binding.limits,
        "goldenbee.team": binding.team,
        "goldenbee.team_version": binding.team_version,
        "goldenbee.team_member": binding.team_member.as_ref().map(|member| member.worker.clone()),
    });
    Message {
        id: MessageId::new(binding.message_id.clone()),
        role: MessageRole::User,
        parts: vec![a2a_protocol_types::Part::text(binding.objective.clone())],
        task_id: None,
        context_id: Some(ContextId::new(binding.context_id.clone())),
        reference_task_ids: None,
        extensions: None,
        metadata: Some(metadata),
    }
}

fn apply_remote_task(binding: &mut PeerDelegation, task: &RemoteTask) {
    binding.remote_task_id = Some(task.id.0.clone());
    binding.remote_state = Some(remote_state_name(task.status.state).to_string());
    binding.state = match task.status.state {
        RemoteTaskState::Completed => "completed",
        RemoteTaskState::Failed => "failed",
        RemoteTaskState::Canceled => "canceled",
        RemoteTaskState::Rejected => "rejected",
        RemoteTaskState::Working => "working",
        RemoteTaskState::Submitted | RemoteTaskState::Unspecified => "submitted",
        RemoteTaskState::InputRequired => "input_required",
        RemoteTaskState::AuthRequired => "auth_required",
        _ => "unknown",
    }
    .to_string();
    let artifacts = task.artifacts.as_deref().unwrap_or_default();
    binding.output = artifact_text(artifacts);
    binding.artifacts = artifacts.iter().map(artifact_evidence).collect();
    binding.last_error = None;
    binding.updated_at = common::now_ms();
}

fn artifact_evidence(artifact: &Artifact) -> DelegatedArtifact {
    let mut text = String::new();
    for part in &artifact.parts {
        if let PartContent::Text(value) = &part.content {
            text.push_str(value);
        }
    }
    DelegatedArtifact {
        artifact_id: artifact.id.0.clone(),
        name: artifact.name.clone(),
        digest: common::bytes_digest(text.as_bytes()),
        size: text.len() as u64,
    }
}

fn remote_state_name(state: RemoteTaskState) -> &'static str {
    match state {
        RemoteTaskState::Unspecified => "TASK_STATE_UNSPECIFIED",
        RemoteTaskState::Submitted => "TASK_STATE_SUBMITTED",
        RemoteTaskState::Working => "TASK_STATE_WORKING",
        RemoteTaskState::InputRequired => "TASK_STATE_INPUT_REQUIRED",
        RemoteTaskState::AuthRequired => "TASK_STATE_AUTH_REQUIRED",
        RemoteTaskState::Completed => "TASK_STATE_COMPLETED",
        RemoteTaskState::Failed => "TASK_STATE_FAILED",
        RemoteTaskState::Canceled => "TASK_STATE_CANCELED",
        RemoteTaskState::Rejected => "TASK_STATE_REJECTED",
        _ => "TASK_STATE_UNSPECIFIED",
    }
}

fn artifact_text(artifacts: &[Artifact]) -> Option<String> {
    let mut text = String::new();
    for artifact in artifacts {
        for part in &artifact.parts {
            if let PartContent::Text(value) = &part.content {
                text.push_str(value);
            }
        }
    }
    (!text.is_empty()).then_some(text)
}

fn stable_key(
    _instance: &str,
    _namespace: &str,
    _caller: &str,
    peer: &str,
    parent: &str,
    capability: &str,
    objective: &str,
) -> String {
    format!("{peer}\u{1f}{capability}\u{1f}{parent}\u{1f}{objective}")
}

fn binding_id(instance: &str, namespace: &str, caller: &str, peer: &str, key: &str) -> String {
    let mut hasher = Sha256::new();
    for value in [instance, namespace, caller, peer, key] {
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("peer-{digest}")
}
