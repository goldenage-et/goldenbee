//! Versioned team rosters, admission inputs, and immutable routing snapshots.
//!
//! These are wire representations shared by Gateway and Worker. They carry
//! configuration and signed roster data only; presence in a Client request does
//! not grant roster, routing, or execution authority.

use crate::limits::Limits;
use serde::{Deserialize, Serialize};

/// Versioned team roster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    /// Owning Gateway namespace.
    #[serde(default)]
    pub namespace: String,
    /// Team name.
    pub name: String,
    /// Monotonic version.
    pub version: u64,
    /// Designated Master Worker identity.
    pub master: String,
    /// Team members, including the designated Master.
    pub members: Vec<TeamMember>,
    /// Team-wide delegation limits.
    pub limits: Limits,
    /// Required member locality (`local` or `remote`), when the operator pins it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locality: Option<String>,
    /// Active flag. Only active rosters admit new executions.
    pub active: bool,
    /// Creation time.
    pub created_at: i64,
    /// Last update time.
    #[serde(default)]
    pub updated_at: i64,
}

/// One team member and its narrowed authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    /// Gateway Worker identity.
    pub worker: String,
    /// Role label. Labels never grant permissions.
    #[serde(default)]
    pub role: String,
    /// Capability subset authorized for this member.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Maximum child depth permitted from this member. Zero means the team cap.
    #[serde(default)]
    pub max_depth: u32,
    /// Expected Worker instance identity, when the operator pins it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

/// Create/update roster input shared by Gateway adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSpec {
    /// Designated Master Worker identity. It must also be a member.
    pub master: String,
    /// Team members, including the designated Master.
    #[serde(default)]
    pub members: Vec<TeamMember>,
    /// Team-wide delegation limits. Unset depth/fanout values receive safe caps.
    #[serde(default)]
    pub limits: Limits,
    /// Required member locality, when the operator pins it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locality: Option<String>,
}

/// Create request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTeamRequest {
    /// Team name within the caller namespace.
    #[serde(alias = "id", alias = "team_id")]
    pub name: String,
    /// Roster definition.
    #[serde(flatten)]
    pub team: TeamSpec,
}

/// Update request body with optimistic concurrency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateTeamRequest {
    /// Version the caller observed. Zero means the HTTP adapter must resolve an
    /// `If-Match` header instead.
    #[serde(default, alias = "version")]
    pub expected_version: u64,
    /// Replacement roster definition.
    #[serde(flatten)]
    pub team: TeamSpec,
}

/// Version selector for validate/activate/deactivate operations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamVersionRequest {
    /// Observed or required roster version. Omitted means the current version.
    #[serde(default, alias = "expected_version")]
    pub version: Option<u64>,
}

/// Immutable roster snapshot carried by a Gateway delegation token.
///
/// The snapshot is issued by Gateway after roster validation. A Worker must
/// revalidate it before execution and before every delegated child.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamRoster {
    /// Team name.
    pub name: String,
    /// Frozen roster version.
    pub version: u64,
    /// Designated Master Worker identity.
    pub master: String,
    /// Resolved member bindings.
    pub members: Vec<TeamMemberGrant>,
    /// Frozen team-wide delegation limits.
    pub limits: Limits,
}

/// Resolved member identity and bounded authority in a roster snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMemberGrant {
    /// Gateway Worker identity.
    pub worker: String,
    /// Verified Worker instance identity.
    pub instance: String,
    /// Approved base endpoint used by the Master.
    pub endpoint: String,
    /// Role label. Labels never grant permissions.
    #[serde(default)]
    pub role: String,
    /// Capability subset authorized for this member.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Maximum child depth permitted from this member. Zero means the team cap.
    #[serde(default)]
    pub max_depth: u32,
}

impl TeamRoster {
    /// One resolved member by Gateway identity.
    pub fn member(&self, worker: &str) -> Option<&TeamMemberGrant> {
        self.members.iter().find(|member| member.worker == worker)
    }

    /// The designated Master binding.
    pub fn master_member(&self) -> Option<&TeamMemberGrant> {
        self.member(&self.master)
    }

    /// Every capability authorized anywhere in this immutable roster.
    pub fn authorized_capabilities(&self) -> Vec<String> {
        let mut capabilities = Vec::new();
        for member in &self.members {
            for capability in &member.capabilities {
                if !capabilities.contains(capability) {
                    capabilities.push(capability.clone());
                }
            }
        }
        capabilities
    }
}
