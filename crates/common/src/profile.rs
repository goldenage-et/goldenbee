//! Runtime profiles and exact-operation approvals.

use crate::limits::Limits;
use serde::{Deserialize, Serialize};

/// Native Runner context-compaction selection for one profile revision.
///
/// The only supported strategy in this release is the pinned native
/// `TruncationCompaction`. The budget must remain below the selected provider's
/// declared context window; that compatibility is checked on profile
/// activation and again at admission against the provider that actually runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompaction {
    /// Native compaction strategy. `truncation` is the supported value.
    #[serde(default = "default_compaction_strategy")]
    pub strategy: String,
    /// Estimated token budget that triggers compaction.
    #[serde(default)]
    pub context_budget: u64,
    /// Number of most-recent native events preserved verbatim.
    #[serde(default = "default_preserve_recent")]
    pub preserve_recent: usize,
    /// Number of native compaction attempts before the Runner stops retrying.
    #[serde(default = "default_compaction_retries")]
    pub max_retries: usize,
}

fn default_compaction_strategy() -> String {
    "truncation".into()
}

fn default_preserve_recent() -> usize {
    8
}

fn default_compaction_retries() -> usize {
    2
}

impl Default for ContextCompaction {
    fn default() -> Self {
        Self {
            strategy: default_compaction_strategy(),
            context_budget: 0,
            preserve_recent: default_preserve_recent(),
            max_retries: default_compaction_retries(),
        }
    }
}

/// Profile revision. Revisions are immutable; activation is separate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileRevision {
    /// Monotonic revision number.
    pub revision: u64,
    /// Agent instruction.
    pub instruction: String,
    /// Model reference.
    pub model: String,
    /// Capability IDs the profile may use.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Permission grants.
    #[serde(default)]
    pub grants: Vec<String>,
    /// Restrictions (must be a subset of binary/instance grants).
    #[serde(default)]
    pub restrictions: Vec<String>,
    /// Native composition: `single`, `sequential`, `parallel`, `loop`, `team`.
    #[serde(default = "default_composition")]
    pub composition: String,
    /// Child agent names for composition.
    #[serde(default)]
    pub members: Vec<String>,
    /// Skill names.
    #[serde(default)]
    pub skills: Vec<String>,
    /// MCP server references.
    #[serde(default)]
    pub mcp: Vec<String>,
    /// Workflow template reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// Profile limits.
    #[serde(default)]
    pub limits: Limits,
    /// Optional native Runner context compaction for this revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<ContextCompaction>,
    /// Secret references, never values.
    #[serde(default)]
    pub secret_refs: Vec<String>,
    /// Creation time.
    pub created_at: i64,
}

fn default_composition() -> String {
    "single".to_string()
}

/// A profile and its revisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// Profile name.
    pub name: String,
    /// Active revision number, or zero when inactive.
    pub active_revision: u64,
    /// Immutable revisions.
    pub revisions: Vec<ProfileRevision>,
}

impl Profile {
    /// Fetch a revision.
    pub fn revision(&self, number: u64) -> Option<&ProfileRevision> {
        self.revisions.iter().find(|r| r.revision == number)
    }

    /// Fetch the active revision.
    pub fn active(&self) -> Option<&ProfileRevision> {
        self.revision(self.active_revision)
    }
}

/// Create a profile with its first immutable revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProfileRequest {
    /// Profile name within the owning Worker namespace.
    pub name: String,
    /// Initial revision. The Worker assigns revision 1 and creation time.
    pub revision: ProfileRevision,
}

/// Append an immutable profile revision using optimistic concurrency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviseProfileRequest {
    /// Revision count the caller observed. A stale value is rejected.
    #[serde(alias = "version")]
    pub expected_revision: u64,
    /// Replacement revision body. The Worker assigns the next revision number.
    pub revision: ProfileRevision,
}

/// Version selector for profile validation or activation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileVersionRequest {
    /// Observed or required revision. Validation defaults to the active
    /// revision, then the latest revision when the profile is inactive.
    #[serde(default, alias = "expected_revision")]
    pub revision: Option<u64>,
}

/// Result of deleting an inactive profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletedProfile {
    /// Removed profile name.
    pub deleted: String,
}

/// Approval request for one exact protected operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Approval {
    /// Approval identity.
    pub id: String,
    /// Task that will execute the operation.
    pub task_id: String,
    /// Namespace.
    pub namespace: String,
    /// Operation name.
    pub operation: String,
    /// Exact target such as a URL or path.
    pub target: String,
    /// Digest binding all effect arguments.
    pub digest: String,
    /// Risk note.
    pub risk: String,
    /// Requesting principal.
    pub requested_by: String,
    /// Expiry time in milliseconds.
    pub expires_at: i64,
    /// `pending`, `approved`, `rejected`, `expired`, or `revoked`.
    pub status: String,
    /// Deciding principal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    /// Decision time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
    /// Receipt after the protected effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
    /// Answer supplied by the deciding caller for a user question.
    ///
    /// Present only for `user.ask` approvals. It is never an authority grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Creation time.
    pub created_at: i64,
}

/// Approval decision body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDecision {
    /// `approved` or `rejected`.
    pub decision: String,
    /// Exact digest the approver saw.
    pub digest: String,
    /// Optional answer for `user.ask` approvals.
    ///
    /// A permission approval ignores this field; the runtime never treats an
    /// answer as authorization for anything except the exact question digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}
