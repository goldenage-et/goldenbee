//! Immutable Runtime Profile revisions with optimistic concurrency.

use crate::execution::providers::ProviderRequirements;
use crate::{now, Error, Result, Runtime};
use common::{
    scopes, ApiError, ContextCompaction, Principal, Profile, ProfileRevision, ResolvedModel,
};
use std::collections::BTreeSet;

fn same_profile_revision(left: &ProfileRevision, right: &ProfileRevision) -> bool {
    fn normalized(revision: &ProfileRevision) -> serde_json::Value {
        let mut value = serde_json::to_value(revision).unwrap_or(serde_json::Value::Null);
        if let Some(object) = value.as_object_mut() {
            object.remove("revision");
            object.remove("created_at");
        }
        value
    }
    normalized(left) == normalized(right)
}

impl Runtime {
    /// Create a profile with revision 1. Creation does not activate it.
    pub async fn create_profile(
        &self,
        principal: &Principal,
        namespace: &str,
        name: &str,
        mut revision: ProfileRevision,
    ) -> Result<Profile> {
        principal.require(scopes::PROFILE_WRITE)?;
        principal.require_namespace(namespace)?;
        validate_name(name)?;
        validate_secret_refs(&revision.secret_refs)?;
        revision.revision = 1;
        revision.created_at = now();
        self.validate_revision(&revision, &Default::default())?;
        let desired = revision.clone();
        self.store()
            .write(move |state| {
                let key = crate::profile_key(namespace, name);
                if let Some(existing) = state.profiles.get(&key) {
                    // Provision retries can re-run the same create command after
                    // the profile was committed but before the caller saw the
                    // acknowledgment. Treat an identical first revision as the
                    // same operation, while still rejecting a different one.
                    if existing.revisions.len() == 1
                        && same_profile_revision(&existing.revisions[0], &desired)
                    {
                        return Ok(existing.clone());
                    }
                    return Err(ApiError::new(
                        "profile.conflict",
                        "profile already exists in this namespace",
                    ));
                }
                let profile = Profile {
                    name: name.to_string(),
                    active_revision: 0,
                    revisions: vec![revision],
                };
                state.profiles.insert(key, profile.clone());
                Ok(profile)
            })
            .await
    }

    /// Append an immutable revision, rejecting stale writers.
    pub async fn revise_profile(
        &self,
        principal: &Principal,
        namespace: &str,
        name: &str,
        expected_revision: u64,
        mut revision: ProfileRevision,
    ) -> Result<Profile> {
        principal.require(scopes::PROFILE_WRITE)?;
        principal.require_namespace(namespace)?;
        validate_secret_refs(&revision.secret_refs)?;
        self.store()
            .write(|state| {
                let snapshot = state.clone();
                let key = crate::profile_key(namespace, name);
                let profile = state
                    .profiles
                    .get_mut(&key)
                    .ok_or_else(|| ApiError::new("profile.not_found", "profile is not visible"))?;
                let current = profile.revisions.len() as u64;
                if current != expected_revision {
                    return Err(ApiError::new(
                        "profile.revision_conflict",
                        "profile was revised by another writer",
                    )
                    .with("expected", expected_revision.to_string())
                    .with("current", current.to_string()));
                }
                revision.revision = current + 1;
                revision.created_at = now();
                self.validate_revision(&revision, &snapshot)?;
                profile.revisions.push(revision);
                Ok(profile.clone())
            })
            .await
    }

    /// Validate and activate a revision. A failed validation leaves the
    /// previous active revision unchanged.
    pub async fn activate_profile(
        &self,
        principal: &Principal,
        namespace: &str,
        name: &str,
        revision: u64,
    ) -> Result<Profile> {
        principal.require(scopes::PROFILE_WRITE)?;
        principal.require_namespace(namespace)?;
        self.store()
            .write(|state| {
                let key = crate::profile_key(namespace, name);
                let snapshot = state.clone();
                let profile = state
                    .profiles
                    .get_mut(&key)
                    .ok_or_else(|| ApiError::new("profile.not_found", "profile is not visible"))?;
                let target = profile.revision(revision).cloned().ok_or_else(|| {
                    ApiError::new(
                        "profile.revision_not_found",
                        "profile revision does not exist",
                    )
                })?;
                // Validate against the snapshot so the mutation can roll back on
                // any capability, grant, or model failure.
                self.validate_revision(&target, &snapshot)?;
                profile.active_revision = revision;
                Ok(profile.clone())
            })
            .await
    }

    /// Deactivate future admission without rewriting active invocations.
    pub async fn deactivate_profile(
        &self,
        principal: &Principal,
        namespace: &str,
        name: &str,
    ) -> Result<Profile> {
        principal.require(scopes::PROFILE_WRITE)?;
        principal.require_namespace(namespace)?;
        self.store()
            .write(|state| {
                let key = crate::profile_key(namespace, name);
                let profile = state
                    .profiles
                    .get_mut(&key)
                    .ok_or_else(|| ApiError::new("profile.not_found", "profile is not visible"))?;
                profile.active_revision = 0;
                Ok(profile.clone())
            })
            .await
    }

    /// Remove an inactive profile, leaving the installed package untouched.
    pub async fn delete_profile(
        &self,
        principal: &Principal,
        namespace: &str,
        name: &str,
    ) -> Result<()> {
        principal.require(scopes::PROFILE_WRITE)?;
        principal.require_namespace(namespace)?;
        if name == "default" {
            return Err(ApiError::new(
                "profile.protected",
                "the default profile cannot be removed",
            ));
        }
        self.store()
            .write(|state| {
                let key = crate::profile_key(namespace, name);
                let profile = state
                    .profiles
                    .get(&key)
                    .ok_or_else(|| ApiError::new("profile.not_found", "profile is not visible"))?;
                if profile.active_revision != 0 {
                    return Err(ApiError::new(
                        "profile.active",
                        "deactivate the profile before removing it",
                    ));
                }
                state.profiles.remove(&key);
                Ok(())
            })
            .await
    }

    /// Fetch a profile.
    pub async fn profile(
        &self,
        principal: &Principal,
        namespace: &str,
        name: &str,
    ) -> Result<Profile> {
        principal.require(scopes::PROFILE_READ)?;
        principal.require_namespace(namespace)?;
        self.store()
            .read(|state| {
                state
                    .profiles
                    .get(&crate::profile_key(namespace, name))
                    .cloned()
            })
            .await
            .ok_or_else(|| ApiError::new("profile.not_found", "profile is not visible"))
    }

    /// List profiles in a namespace.
    pub async fn profiles(&self, principal: &Principal, namespace: &str) -> Result<Vec<Profile>> {
        principal.require(scopes::PROFILE_READ)?;
        principal.require_namespace(namespace)?;
        let prefix = format!("{namespace}\u{1f}");
        Ok(self
            .store()
            .read(|state| {
                state
                    .profiles
                    .iter()
                    .filter(|(key, _)| key.starts_with(&prefix) || namespace == "local")
                    .map(|(_, profile)| profile.clone())
                    .collect()
            })
            .await)
    }

    /// Validate an existing revision without activating it.
    ///
    /// The owning Worker validates against the same resolved assets, provider,
    /// grant, and policy state used by creation and activation. The active
    /// revision is never changed, including when validation fails.
    pub async fn validate_profile(
        &self,
        principal: &Principal,
        namespace: &str,
        name: &str,
        revision: Option<u64>,
    ) -> Result<Profile> {
        principal.require(scopes::PROFILE_READ)?;
        principal.require_namespace(namespace)?;
        let snapshot = self.store().read(|state| state.clone()).await;
        let profile = snapshot
            .profiles
            .get(&crate::profile_key(namespace, name))
            .cloned()
            .ok_or_else(|| ApiError::new("profile.not_found", "profile is not visible"))?;
        let number = revision.unwrap_or_else(|| {
            if profile.active_revision > 0 {
                profile.active_revision
            } else {
                profile
                    .revisions
                    .last()
                    .map(|revision| revision.revision)
                    .unwrap_or(0)
            }
        });
        let target = profile.revision(number).cloned().ok_or_else(|| {
            ApiError::new(
                "profile.revision_not_found",
                "profile revision does not exist",
            )
        })?;
        self.validate_revision(&target, &snapshot)?;
        Ok(profile)
    }

    /// Read one immutable revision by number without principal scoping.
    ///
    /// Used by native execution to build the exact revision pinned on a task.
    pub async fn profile_revision_unscoped(
        &self,
        namespace: &str,
        name: &str,
        revision: u64,
    ) -> Option<ProfileRevision> {
        self.store()
            .read(|state| {
                state
                    .profiles
                    .get(&crate::profile_key(namespace, name))
                    .and_then(|profile| profile.revision(revision).cloned())
            })
            .await
    }

    /// Read the active revision for admission. The returned revision is frozen
    /// onto the task.
    pub async fn active_revision(&self, namespace: &str, name: &str) -> Result<ProfileRevision> {
        let profile = self
            .store()
            .read(|state| {
                state
                    .profiles
                    .get(&crate::profile_key(namespace, name))
                    .cloned()
            })
            .await
            .ok_or_else(|| ApiError::new("profile.not_found", "profile does not exist"))?;
        profile
            .active()
            .cloned()
            .ok_or_else(|| ApiError::new("profile.inactive", "profile has no active revision"))
    }

    pub(crate) fn validate_revision(
        &self,
        revision: &ProfileRevision,
        state: &crate::State,
    ) -> Result<()> {
        if revision.instruction.len() > 100_000 {
            return Err(Error::new(
                "profile.instruction_too_large",
                "instruction exceeds 100 KB",
            ));
        }
        let instance_grants = self.config().instance_grants.read().map_err(|_| {
            ApiError::new(
                "profile.grant_unavailable",
                "instance grants are temporarily unavailable",
            )
        })?;
        // Validate the logical model reference even when the revision relies on
        // the agent's own reference, so an unusable profile is caught before it
        // is created or activated.
        if !self.config().providers.contains_key(&revision.model) {
            return Err(ApiError::new(
                "profile.model_unsupported",
                format!("model '{}' is not configured", revision.model),
            ));
        }
        self.providers().record(
            &revision.model,
            revision.revision,
            &ProviderRequirements {
                modalities: vec!["text".into()],
                tools: 0,
                max_tokens: revision.limits.max_tokens,
            },
        )?;
        let provider = self
            .config()
            .providers
            .get(&revision.model)
            .ok_or_else(|| {
                ApiError::new(
                    "profile.model_unsupported",
                    format!("model '{}' is not configured", revision.model),
                )
            })?;
        if !provider
            .modalities
            .iter()
            .any(|modality| modality == "text")
        {
            return Err(ApiError::new(
                "profile.modality_unsupported",
                "configured model has no text modality",
            ));
        }
        crate::execution::budget::validate_admission(&revision.limits, provider, &revision.model)?;
        for fallback in &provider.fallbacks {
            if let Some(target) = self.config().providers.get(fallback) {
                crate::execution::budget::validate_admission(&revision.limits, target, fallback)?;
            }
        }
        if let Some(compaction) = &revision.compaction {
            self.providers()
                .validate_compaction(&revision.model, compaction)?;
        }
        for member in &revision.members {
            if member == "coordinator" {
                return Err(ApiError::new(
                    "profile.member_reserved",
                    "'coordinator' is reserved for the native team coordinator",
                ));
            }
        }
        for capability in &revision.capabilities {
            if capability == "*" {
                continue;
            }
            if !self
                .config()
                .capabilities
                .iter()
                .any(|registered| &registered.id == capability)
            {
                return Err(ApiError::new(
                    "profile.capability_unavailable",
                    format!("capability '{capability}' is not registered by the binary"),
                ));
            }
        }
        if !instance_grants.iter().any(|grant| grant == "*") {
            for grant in &revision.grants {
                if grant == "*" || !instance_grants.contains(grant) {
                    return Err(ApiError::new(
                        "profile.grant_escalation",
                        format!("grant '{grant}' is outside instance authority"),
                    ));
                }
            }
        }
        for member in &revision.members {
            if !self
                .config()
                .capabilities
                .iter()
                .any(|registered| &registered.id == member)
            {
                return Err(ApiError::new(
                    "profile.capability_unavailable",
                    format!("profile member '{member}' is not registered by the binary"),
                ));
            }
            if !revision.capabilities.is_empty()
                && !revision
                    .capabilities
                    .iter()
                    .any(|capability| capability == "*")
                && !revision.capabilities.contains(member)
            {
                return Err(ApiError::new(
                    "profile.capability_denied",
                    format!("profile member '{member}' is outside the profile capability list"),
                ));
            }
        }
        let mut seen_members = std::collections::BTreeSet::new();
        for member in &revision.members {
            if !seen_members.insert(member.as_str()) {
                return Err(ApiError::new(
                    "profile.member_duplicate",
                    format!("profile member '{member}' is selected twice"),
                ));
            }
        }
        let mut seen_members = std::collections::BTreeSet::new();
        for member in &revision.members {
            if !seen_members.insert(member.as_str()) {
                return Err(ApiError::new(
                    "profile.member_duplicate",
                    format!("profile member '{member}' is selected twice"),
                ));
            }
        }
        for restriction in &revision.restrictions {
            if revision
                .grants
                .iter()
                .any(|grant| grant == "*" || grant == restriction)
            {
                return Err(ApiError::new(
                    "profile.restriction_conflict",
                    format!("profile restricts grant '{restriction}' but also grants it"),
                ));
            }
            if revision
                .capabilities
                .iter()
                .any(|capability| capability == "*" || capability == restriction)
            {
                return Err(ApiError::new(
                    "profile.restriction_conflict",
                    format!("profile restricts capability '{restriction}' but also enables it"),
                ));
            }
        }
        for skill in &revision.skills {
            if !self.config().skills.iter().any(|known| known == skill) {
                return Err(ApiError::new(
                    "profile.skill_unavailable",
                    format!("profile skill '{skill}' is not discovered by this Worker"),
                ));
            }
        }
        let mcp_policies = self.config().mcp_policies.read().map_err(|_| {
            ApiError::new(
                "mcp.policy_unavailable",
                "local MCP policy is temporarily unavailable",
            )
        })?;
        let mut connections = BTreeSet::new();
        for connection in &revision.mcp {
            if !connections.insert(connection) {
                return Err(ApiError::new(
                    "profile.mcp_duplicate",
                    format!("profile lists MCP connection '{connection}' more than once"),
                ));
            }
            let server = self.config().mcp.servers.get(connection).ok_or_else(|| {
                ApiError::new(
                    "profile.mcp_unavailable",
                    format!("MCP connection '{connection}' is not configured by this Worker"),
                )
            })?;
            if server.is_remote() {
                if !server.enabled {
                    return Err(ApiError::new(
                        "profile.mcp_disabled",
                        format!("MCP connection '{connection}' is disabled by the operator"),
                    ));
                }
                let grant = server.grant_for(connection);
                if !revision
                    .grants
                    .iter()
                    .any(|profile| profile == "*" || profile == &grant)
                {
                    return Err(ApiError::new(
                        "profile.mcp_grant_missing",
                        format!("profile does not grant MCP connection '{connection}'"),
                    ));
                }
                if revision.restrictions.iter().any(|value| value == &grant) {
                    return Err(ApiError::new(
                        "profile.mcp_grant_denied",
                        format!("profile restricts MCP connection '{connection}'"),
                    ));
                }
                if self
                    .config()
                    .mcp
                    .credential(server.credential.as_deref().unwrap_or_default())
                    .is_none()
                {
                    return Err(ApiError::new(
                        "profile.mcp_credential_missing",
                        format!("MCP connection '{connection}' has no owned credential binding"),
                    ));
                }
                continue;
            }
            let policy = mcp_policies.get(connection).ok_or_else(|| {
                ApiError::new(
                    "profile.mcp_unavailable",
                    format!("MCP connection '{connection}' is not configured by this Worker"),
                )
            })?;
            if !policy.enabled {
                return Err(ApiError::new(
                    "profile.mcp_disabled",
                    format!("MCP connection '{connection}' is disabled by the operator"),
                ));
            }
            let grant = format!("mcp:{connection}");
            if !instance_grants
                .iter()
                .any(|instance| instance == "*" || instance == &grant)
            {
                return Err(ApiError::new(
                    "profile.grant_escalation",
                    format!("MCP connection '{connection}' is outside instance authority"),
                ));
            }
            if !revision
                .grants
                .iter()
                .any(|profile| profile == "*" || profile == &grant)
            {
                return Err(ApiError::new(
                    "profile.mcp_grant_required",
                    format!("profile must grant MCP connection '{connection}'"),
                ));
            }
            if let Some(credential) = &policy.credential {
                if !revision
                    .secret_refs
                    .iter()
                    .any(|reference| reference == credential)
                {
                    return Err(ApiError::new(
                        "profile.credential_scope_denied",
                        format!(
                            "profile must reference the credential for MCP connection '{connection}'"
                        ),
                    ));
                }
            }
        }
        match revision.composition.as_str() {
            "single" | "sequential" | "parallel" | "loop" | "team" | "fan-out-fan-in"
            | "review-loop" => {}
            _ if revision.workflow.is_some() => {}
            other => {
                return Err(ApiError::new(
                    "profile.composition_invalid",
                    format!("unsupported composition '{other}'"),
                ))
            }
        }
        crate::agents::workflow::validate_workflow(revision)?;
        let profile_prefix = format!("{}\\u{{1f}}", "profile");
        let _ = (state, profile_prefix);
        Ok(())
    }

    /// Resolve the concrete provider recorded for one invocation.
    ///
    /// A composition with selected members uses the revision's logical model.
    /// An agent-only invocation uses the reference resolved for that agent at
    /// asset load, so the record describes the model that will actually run.
    /// `None` means package code registered an already-built agent without a
    /// declared reference; no provider claim is made in that case.
    pub(crate) fn resolve_invocation_model(
        &self,
        revision: &ProfileRevision,
        agent: &str,
        limits: &common::Limits,
        compaction: Option<&ContextCompaction>,
    ) -> Result<Option<ResolvedModel>> {
        let max_tokens = limits.max_tokens;
        let (provider, requirements) = if revision.members.is_empty() {
            let Some(provider) = self.config().agent_providers.get(agent).cloned() else {
                if compaction.is_some() {
                    return Err(ApiError::new(
                        "provider.compaction_unverified",
                        "context compaction requires a declared provider reference for this agent",
                    )
                    .with("agent", agent));
                }
                return Ok(None);
            };
            let requirement = self
                .config()
                .agent_requirements
                .get(agent)
                .cloned()
                .unwrap_or_default();
            (
                provider,
                ProviderRequirements {
                    modalities: vec!["text".into()],
                    tools: requirement.tools + requirement.toolsets,
                    max_tokens,
                },
            )
        } else {
            let mut tools = 0usize;
            let mut uses_profile_model = revision.composition == "team";
            for member in &revision.members {
                if let Some(requirement) = self.config().agent_requirements.get(member) {
                    // File-defined leaves and the team coordinator use the
                    // profile model. Manually registered leaves keep their
                    // own resolved reference and add no requirements.
                    uses_profile_model = true;
                    tools += requirement.tools + requirement.toolsets;
                }
            }
            if !uses_profile_model {
                if compaction.is_some() {
                    return Err(ApiError::new(
                        "provider.compaction_unverified",
                        "context compaction requires a declared provider reference for this composition",
                    ));
                }
                return Ok(None);
            }
            (
                revision.model.clone(),
                ProviderRequirements {
                    modalities: vec!["text".into()],
                    tools,
                    max_tokens,
                },
            )
        };
        if let Some(compaction) = compaction {
            // Validate the provider that actually runs, not only the one named
            // by the revision, before any task is recorded.
            self.providers()
                .validate_compaction(&provider, compaction)?;
        }
        let selected = self.config().providers.get(&provider).ok_or_else(|| {
            ApiError::new(
                "profile.model_unsupported",
                format!("model '{provider}' is not configured"),
            )
        })?;
        crate::execution::budget::validate_admission(limits, selected, &provider)?;
        for fallback in &selected.fallbacks {
            if let Some(target) = self.config().providers.get(fallback) {
                crate::execution::budget::validate_admission(limits, target, fallback)?;
            }
        }
        self.providers()
            .record(&provider, revision.revision, &requirements)
            .map(Some)
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty()
        || name.len() > 100
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(Error::new(
            "validation.profile_name",
            "profile name must be a 1-100 character identifier",
        ));
    }
    Ok(())
}

fn validate_secret_refs(refs: &[String]) -> Result<()> {
    for reference in refs {
        let looks_like_value = reference.starts_with("sk-")
            || reference.starts_with("Bearer ")
            || reference.starts_with("AKIA")
            || reference.len() > 256;
        let is_reference = reference.starts_with("secret://")
            || reference.starts_with("env:")
            || reference.starts_with("vault:");
        if looks_like_value || !is_reference {
            return Err(ApiError::new(
                "profile.secret_value_rejected",
                "profile secret_refs must use secret://, env:, or vault: references",
            ));
        }
    }
    Ok(())
}
