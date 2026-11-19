//! Versioned Gateway team rosters and admission validation.
//!
//! A team is Gateway-owned configuration, not a process or a Worker engine.
//! Creating or updating a roster validates the designated Master and every
//! member against the current registry. Activation revalidates so a membership
//! change between setup and activation cannot silently authorize stale work.

use common::{ApiError, Team, TeamSpec, LOCALITY_LOCAL, LOCALITY_REMOTE};

use super::model::Document;
use super::Registry;

impl Registry {
    /// Create a version-1 inactive roster in a namespace.
    pub fn create_team(
        &self,
        namespace: &str,
        name: &str,
        spec: TeamSpec,
        now: i64,
    ) -> Result<Team, ApiError> {
        validate_team_name(name)?;
        let mut team = Team {
            namespace: namespace.to_string(),
            name: name.to_string(),
            version: 1,
            master: spec.master,
            members: spec.members,
            limits: spec.limits,
            locality: spec.locality,
            active: false,
            created_at: now,
            updated_at: now,
        };
        self.mutate(|document| {
            let key = team_key(namespace, name);
            if document.teams.contains_key(&key) {
                return Err(ApiError::new("team.conflict", "team already exists"));
            }
            normalize_and_validate(&mut team, document, now)?;
            document.teams.insert(key, team.clone());
            Ok(team.clone())
        })
    }

    /// Append a version when the caller observed the current one.
    pub fn update_team(
        &self,
        namespace: &str,
        name: &str,
        expected_version: u64,
        spec: TeamSpec,
        now: i64,
    ) -> Result<Team, ApiError> {
        self.mutate(|document| {
            let key = team_key(namespace, name);
            let current = document.teams.get(&key).ok_or_else(not_found)?;
            if current.version != expected_version {
                return Err(ApiError::new(
                    "team.version_conflict",
                    "team was updated by another writer",
                )
                .with("expected_version", expected_version.to_string())
                .with("current_version", current.version.to_string()));
            }
            let mut next = Team {
                namespace: current.namespace.clone(),
                name: current.name.clone(),
                version: current.version + 1,
                master: spec.master,
                members: spec.members,
                limits: spec.limits,
                locality: spec.locality,
                active: current.active,
                created_at: current.created_at,
                updated_at: now,
            };
            normalize_and_validate(&mut next, document, now)?;
            document.teams.insert(key, next.clone());
            Ok(next)
        })
    }

    /// Revalidate a roster version without changing it.
    pub fn validate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        now: i64,
    ) -> Result<Team, ApiError> {
        self.mutate(|document| {
            let key = team_key(namespace, name);
            let mut team = document.teams.get(&key).cloned().ok_or_else(not_found)?;
            if version.is_some_and(|version| version != team.version) {
                return Err(ApiError::new(
                    "team.version_conflict",
                    "team version changed before validation",
                ));
            }
            normalize_and_validate(&mut team, document, now)?;
            Ok(team)
        })
    }

    /// Activate a roster after revalidating live membership and capability.
    pub fn activate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        now: i64,
    ) -> Result<Team, ApiError> {
        self.mutate(|document| {
            let key = team_key(namespace, name);
            let mut team = document.teams.get(&key).cloned().ok_or_else(not_found)?;
            if version.is_some_and(|version| version != team.version) {
                return Err(ApiError::new(
                    "team.version_conflict",
                    "team version changed before activation",
                ));
            }
            require_reachable(&team, document, now)?;
            normalize_and_validate(&mut team, document, now)?;
            team.active = true;
            team.updated_at = now;
            document.teams.insert(key, team.clone());
            Ok(team)
        })
    }

    /// Stop new team admissions without rewriting active work.
    pub fn deactivate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        now: i64,
    ) -> Result<Team, ApiError> {
        self.mutate(|document| {
            let key = team_key(namespace, name);
            let mut team = document.teams.get(&key).cloned().ok_or_else(not_found)?;
            if version.is_some_and(|version| version != team.version) {
                return Err(ApiError::new(
                    "team.version_conflict",
                    "team version changed before deactivation",
                ));
            }
            team.active = false;
            team.updated_at = now;
            document.teams.insert(key, team.clone());
            Ok(team)
        })
    }

    /// One roster in a namespace.
    pub fn team(&self, namespace: &str, name: &str) -> Option<Team> {
        self.lock()
            .document
            .teams
            .get(&team_key(namespace, name))
            .cloned()
    }

    /// All rosters in a namespace.
    pub fn teams(&self, namespace: &str) -> Vec<Team> {
        let prefix = format!("{namespace}\u{1f}");
        self.lock()
            .document
            .teams
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .map(|(_, team)| team.clone())
            .collect()
    }
}

fn not_found() -> ApiError {
    ApiError::new("team.not_found", "team is not visible")
}

fn team_key(namespace: &str, name: &str) -> String {
    format!("{namespace}\u{1f}{name}")
}

fn validate_team_name(name: &str) -> Result<(), ApiError> {
    if name.trim().is_empty()
        || name.len() > 100
        || name
            .chars()
            .any(|character| character.is_control() || character == '/' || character == '\\')
    {
        return Err(ApiError::new(
            "validation.team_name",
            "team name must be a 1-100 character identifier",
        ));
    }
    Ok(())
}

/// Apply safe ceilings and validate the roster against current Workers.
fn normalize_and_validate(team: &mut Team, document: &Document, now: i64) -> Result<(), ApiError> {
    if team.members.is_empty() {
        return Err(ApiError::new(
            "team.empty",
            "team requires a designated Master and at least one subworker",
        ));
    }
    if team.limits.max_depth == 0 {
        team.limits.max_depth = 2;
    }
    if team.limits.max_depth > 16 {
        return Err(ApiError::new(
            "team.depth_exceeded",
            "team max_depth must not exceed 16",
        ));
    }
    let subworker_count = team
        .members
        .iter()
        .filter(|member| member.worker != team.master)
        .count();
    if subworker_count == 0 {
        return Err(ApiError::new(
            "team.subworker_required",
            "a team requires at least one subworker distinct from the Master",
        ));
    }
    if team.limits.max_fanout == 0 {
        team.limits.max_fanout = subworker_count as u32;
    }
    if subworker_count > team.limits.max_fanout as usize {
        return Err(ApiError::new(
            "team.fanout_exceeded",
            "team subworkers exceed the configured fanout limit",
        )
        .with("subworkers", subworker_count.to_string())
        .with("max_fanout", team.limits.max_fanout.to_string()));
    }
    if let Some(locality) = &team.locality {
        if !matches!(locality.as_str(), LOCALITY_LOCAL | LOCALITY_REMOTE) {
            return Err(ApiError::new(
                "team.locality_invalid",
                "team locality must be 'local' or 'remote'",
            ));
        }
    }

    let mut seen_workers = std::collections::BTreeSet::new();
    let mut seen_instances = std::collections::BTreeSet::new();
    let mut master_seen = false;
    for member in &mut team.members {
        if !seen_workers.insert(member.worker.clone()) {
            return Err(ApiError::new(
                "team.member_duplicate",
                format!("Worker '{}' appears more than once", member.worker),
            ));
        }
        let record = document.workers.get(&member.worker).ok_or_else(|| {
            ApiError::new(
                "team.member_unavailable",
                format!("Worker '{}' is not registered", member.worker),
            )
            .with("worker", member.worker.clone())
        })?;
        if matches!(record.status.as_str(), "decommissioned" | "disabled") {
            return Err(ApiError::new(
                "team.member_unavailable",
                format!("Worker '{}' is not available", member.worker),
            )
            .with("worker", member.worker.clone()));
        }
        if !seen_instances.insert(record.instance.clone()) {
            return Err(ApiError::new(
                "team.member_duplicate",
                format!(
                    "Worker instance '{}' is bound to more than one member",
                    record.instance
                ),
            ));
        }
        if let Some(expected) = &member.instance {
            if expected != &record.instance {
                return Err(ApiError::new(
                    "team.member_instance_mismatch",
                    format!(
                        "Worker '{}' reported instance '{}' not '{}'",
                        member.worker, record.instance, expected
                    ),
                )
                .with("worker", member.worker.clone()));
            }
        }
        if let Some(locality) = &team.locality {
            if &record.locality != locality {
                return Err(ApiError::new(
                    "team.locality_incompatible",
                    format!(
                        "Worker '{}' locality '{}' does not match team locality '{}'",
                        member.worker, record.locality, locality
                    ),
                )
                .with("worker", member.worker.clone()));
            }
        }
        if member.capabilities.is_empty() {
            return Err(ApiError::new(
                "team.member_capability_required",
                format!(
                    "Worker '{}' must authorize at least one team capability",
                    member.worker
                ),
            )
            .with("worker", member.worker.clone()));
        }
        for capability in &member.capabilities {
            if !record
                .capabilities
                .iter()
                .any(|declared| &declared.id == capability)
            {
                return Err(ApiError::new(
                    "team.capability_unavailable",
                    format!(
                        "Worker '{}' does not advertise capability '{}'",
                        member.worker, capability
                    ),
                )
                .with("worker", member.worker.clone())
                .with("capability", capability.clone()));
            }
        }
        if member.max_depth == 0 {
            member.max_depth = team.limits.max_depth;
        }
        if member.max_depth > team.limits.max_depth {
            return Err(ApiError::new(
                "team.depth_exceeded",
                format!(
                    "Worker '{}' depth {} exceeds the team limit {}",
                    member.worker, member.max_depth, team.limits.max_depth
                ),
            )
            .with("worker", member.worker.clone()));
        }
        if member.worker == team.master {
            master_seen = true;
        }
    }
    if !master_seen {
        // The proposal permits a separate Master plus a subworker list. Infer
        // the Master binding from the registry and authorize every effective
        // capability the operator approved for that instance.
        let master = document.workers.get(&team.master).ok_or_else(|| {
            ApiError::new(
                "team.master_invalid",
                format!("designated Master '{}' is not registered", team.master),
            )
            .with("worker", team.master.clone())
        })?;
        if matches!(master.status.as_str(), "decommissioned" | "disabled") {
            return Err(ApiError::new(
                "team.master_invalid",
                format!("designated Master '{}' is not available", team.master),
            )
            .with("worker", team.master.clone()));
        }
        if master.capabilities.is_empty() {
            return Err(ApiError::new(
                "team.master_invalid",
                "the designated Master advertises no capability",
            )
            .with("worker", team.master.clone()));
        }
        if !seen_instances.insert(master.instance.clone()) {
            return Err(ApiError::new(
                "team.member_duplicate",
                format!(
                    "Worker instance '{}' appears more than once",
                    master.instance
                ),
            ));
        }
        if let Some(locality) = &team.locality {
            if &master.locality != locality {
                return Err(ApiError::new(
                    "team.locality_incompatible",
                    format!(
                        "Master '{}' locality '{}' does not match team locality '{}'",
                        team.master, master.locality, locality
                    ),
                )
                .with("worker", team.master.clone()));
            }
        }
    }
    let _ = now;
    Ok(())
}

/// Require every roster member to hold a live, reachable lease.
fn require_reachable(team: &Team, document: &Document, now: i64) -> Result<(), ApiError> {
    if !team
        .members
        .iter()
        .any(|member| member.worker == team.master)
    {
        let master = document.workers.get(&team.master).ok_or_else(|| {
            ApiError::new(
                "team.master_invalid",
                format!("designated Master '{}' is not registered", team.master),
            )
            .with("worker", team.master.clone())
        })?;
        if !master.reachable(now) {
            return Err(ApiError::new(
                "team.member_unavailable",
                format!("designated Master '{}' is not reachable", team.master),
            )
            .with("worker", team.master.clone()));
        }
    }
    for member in &team.members {
        let record = document.workers.get(&member.worker).ok_or_else(|| {
            ApiError::new(
                "team.member_unavailable",
                format!("Worker '{}' is not registered", member.worker),
            )
            .with("worker", member.worker.clone())
        })?;
        if !record.reachable(now) {
            return Err(ApiError::new(
                "team.member_unavailable",
                format!("Worker '{}' is not reachable", member.worker),
            )
            .with("worker", member.worker.clone()));
        }
    }
    Ok(())
}
