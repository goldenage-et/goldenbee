//! Authorized team roster management and immutable routing snapshots.

use super::{Gateway, State};
use crate::caller::Caller;
use common::{
    scopes, ApiError, Team, TeamMember, TeamMemberGrant, TeamRoster, TeamSpec, TeamVersionRequest,
};

impl Gateway {
    /// Create an inactive versioned roster.
    pub fn create_team(
        &self,
        caller: &Caller,
        name: &str,
        spec: TeamSpec,
    ) -> Result<Team, ApiError> {
        self.require_team_write(caller)?;
        self.require_member_visible(caller, &spec.master)?;
        self.require_members_visible(caller, &spec.members)?;
        let team = self.state.registry.create_team(
            &caller.principal.namespace,
            name,
            spec,
            self.state.registry.now(),
        )?;
        self.team_audit(caller, "team.create", &team, "accepted");
        Ok(team)
    }

    /// Append a roster version with optimistic concurrency.
    pub fn update_team(
        &self,
        caller: &Caller,
        name: &str,
        expected_version: u64,
        spec: TeamSpec,
    ) -> Result<Team, ApiError> {
        self.require_team_write(caller)?;
        self.require_member_visible(caller, &spec.master)?;
        self.require_members_visible(caller, &spec.members)?;
        let team = self.state.registry.update_team(
            &caller.principal.namespace,
            name,
            expected_version,
            spec,
            self.state.registry.now(),
        )?;
        self.team_audit(caller, "team.update", &team, "accepted");
        Ok(team)
    }

    /// Revalidate a roster against current Worker leases and capabilities.
    pub fn validate_team(
        &self,
        caller: &Caller,
        name: &str,
        request: &TeamVersionRequest,
    ) -> Result<Team, ApiError> {
        self.require_team_read(caller)?;
        let team = self.state.registry.validate_team(
            &caller.principal.namespace,
            name,
            request.version,
            self.state.registry.now(),
        )?;
        self.team_audit(caller, "team.validate", &team, "accepted");
        Ok(team)
    }

    /// Activate a revalidated roster.
    pub fn activate_team(
        &self,
        caller: &Caller,
        name: &str,
        request: &TeamVersionRequest,
    ) -> Result<Team, ApiError> {
        self.require_team_write(caller)?;
        let team = self.state.registry.activate_team(
            &caller.principal.namespace,
            name,
            request.version,
            self.state.registry.now(),
        )?;
        self.team_audit(caller, "team.activate", &team, "accepted");
        Ok(team)
    }

    /// Stop new team admissions. Active executions keep their frozen roster.
    pub fn deactivate_team(
        &self,
        caller: &Caller,
        name: &str,
        request: &TeamVersionRequest,
    ) -> Result<Team, ApiError> {
        self.require_team_write(caller)?;
        let team = self.state.registry.deactivate_team(
            &caller.principal.namespace,
            name,
            request.version,
            self.state.registry.now(),
        )?;
        self.team_audit(caller, "team.deactivate", &team, "accepted");
        Ok(team)
    }

    /// One roster visible to the caller namespace.
    pub fn team(&self, caller: &Caller, name: &str) -> Result<Team, ApiError> {
        self.require_team_read(caller)?;
        self.state
            .registry
            .team(&caller.principal.namespace, name)
            .ok_or_else(|| ApiError::new("team.not_found", "team is not visible"))
    }

    /// Rosters visible to the caller namespace.
    pub fn teams(&self, caller: &Caller) -> Result<Vec<Team>, ApiError> {
        self.require_team_read(caller)?;
        Ok(self.state.registry.teams(&caller.principal.namespace))
    }

    /// Resolve and freeze one active roster for a new execution.
    pub(super) fn resolve_team_roster(
        &self,
        state: &State,
        caller: &Caller,
        name: &str,
        requested_version: Option<u64>,
    ) -> Result<TeamRoster, ApiError> {
        let team = state
            .registry
            .team(&caller.principal.namespace, name)
            .ok_or_else(|| {
                ApiError::new("team.not_found", "team is not visible")
                    .with("team", name.to_string())
            })?;
        if !team.active {
            return Err(ApiError::new("team.inactive", "team is not active")
                .with("team", team.name.clone()));
        }
        if let Some(version) = requested_version {
            if version != team.version {
                return Err(ApiError::new(
                    "team.version_conflict",
                    "requested team version is not the current active version",
                )
                .with("team", team.name.clone())
                .with("requested_version", version.to_string())
                .with("current_version", team.version.to_string()));
            }
        }
        self.require_member_visible(caller, &team.master)?;
        self.require_members_visible(caller, &team.members)?;
        let mut members = Vec::with_capacity(team.members.len() + 1);
        for member in &team.members {
            let worker = state.registry.worker(&member.worker).ok_or_else(|| {
                ApiError::new(
                    "team.member_unavailable",
                    format!("Worker '{}' is not registered", member.worker),
                )
                .with("team", team.name.clone())
                .with("worker", member.worker.clone())
            })?;
            if worker.status == "decommissioned" || worker.status == "disabled" {
                return Err(ApiError::new(
                    "team.member_unavailable",
                    format!("Worker '{}' is unavailable", member.worker),
                )
                .with("team", team.name.clone())
                .with("worker", member.worker.clone()));
            }
            if !worker.reachable {
                return Err(ApiError::retryable(
                    "team.member_unavailable",
                    format!("Worker '{}' is not reachable", member.worker),
                )
                .with("team", team.name.clone())
                .with("worker", member.worker.clone()));
            }
            for capability in &member.capabilities {
                if !worker
                    .capabilities
                    .iter()
                    .any(|declared| &declared.id == capability)
                {
                    return Err(ApiError::new(
                        "team.capability_unavailable",
                        format!(
                            "Worker '{}' no longer advertises capability '{}'",
                            member.worker, capability
                        ),
                    )
                    .with("team", team.name.clone())
                    .with("worker", member.worker.clone())
                    .with("capability", capability.clone()));
                }
            }
            if let Some(locality) = &team.locality {
                if &worker.locality != locality {
                    return Err(ApiError::new(
                        "team.locality_incompatible",
                        format!(
                            "Worker '{}' locality changed to '{}'",
                            member.worker, worker.locality
                        ),
                    )
                    .with("team", team.name.clone())
                    .with("worker", member.worker.clone()));
                }
            }
            members.push(TeamMemberGrant {
                worker: worker.id,
                instance: worker.instance,
                endpoint: worker.endpoint,
                role: member.role.clone(),
                capabilities: member.capabilities.clone(),
                max_depth: member.max_depth,
            });
        }
        if !members.iter().any(|member| member.worker == team.master) {
            let worker = state.registry.worker(&team.master).ok_or_else(|| {
                ApiError::new(
                    "team.master_invalid",
                    format!("designated Master '{}' is not registered", team.master),
                )
                .with("team", team.name.clone())
                .with("worker", team.master.clone())
            })?;
            if worker.status == "decommissioned" || worker.status == "disabled" || !worker.reachable
            {
                return Err(ApiError::new(
                    "team.master_invalid",
                    format!("designated Master '{}' is not available", team.master),
                )
                .with("team", team.name.clone())
                .with("worker", team.master.clone()));
            }
            if let Some(locality) = &team.locality {
                if &worker.locality != locality {
                    return Err(ApiError::new(
                        "team.locality_incompatible",
                        format!("Master '{}' locality changed", team.master),
                    )
                    .with("team", team.name.clone())
                    .with("worker", team.master.clone()));
                }
            }
            members.push(TeamMemberGrant {
                worker: worker.id,
                instance: worker.instance,
                endpoint: worker.endpoint,
                role: "master".into(),
                capabilities: worker
                    .capabilities
                    .iter()
                    .map(|capability| capability.id.clone())
                    .collect(),
                max_depth: team.limits.max_depth,
            });
        }
        Ok(TeamRoster {
            name: team.name,
            version: team.version,
            master: team.master,
            members,
            limits: team.limits,
        })
    }

    fn require_team_read(&self, caller: &Caller) -> Result<(), ApiError> {
        caller
            .principal
            .require_any(&[scopes::TASK_READ, scopes::TEAM_READ])
    }

    fn require_team_write(&self, caller: &Caller) -> Result<(), ApiError> {
        caller
            .principal
            .require_any(&[scopes::ADMIN, scopes::TEAM_WRITE])
    }

    fn require_members_visible(
        &self,
        caller: &Caller,
        members: &[TeamMember],
    ) -> Result<(), ApiError> {
        for member in members {
            self.require_member_visible(caller, &member.worker)?;
        }
        Ok(())
    }

    fn require_member_visible(&self, caller: &Caller, worker: &str) -> Result<(), ApiError> {
        if !caller.allows(worker) {
            return Err(ApiError::new(
                "team.member_hidden",
                format!("Worker '{worker}' is not visible to this caller and cannot join a team"),
            )
            .with("worker", worker.to_string()));
        }
        Ok(())
    }

    fn team_audit(&self, caller: &Caller, action: &str, team: &Team, outcome: &str) {
        self.state.registry.audit(
            self.state.registry.now(),
            action,
            &format!("team:{}", team.name),
            &caller.principal.id,
            outcome,
            &format!("version={} members={}", team.version, team.members.len()),
        );
    }
}
