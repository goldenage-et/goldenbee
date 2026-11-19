//! Verified Worker release upgrade planning, activation, and recovery.
//!
//! `gb-instance upgrade` never replaces executable code silently. It compares
//! the active instance with a verified candidate release, reports every
//! compatibility and authority difference, settles admitted work according to
//! an explicit policy, snapshots persisted data, activates the candidate, and
//! verifies readiness plus a real capability invocation. A failed activation
//! restores the previous binary and the data snapshot; a restoration that
//! cannot be proven is quarantined rather than reported as success.

use crate::layout::{
    instance_paths, load_record, load_release, load_state, read_json, save_state, set_mode,
    token_for, write_file, write_json, InstanceError, InstanceRecord, InstanceState,
};
use crate::process;
use common::ReleaseMetadata;
use install::Installed;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Journal file written inside one instance directory during an upgrade.
pub const JOURNAL_FILE: &str = "upgrade.json";
const JOURNAL_SCHEMA: u32 = 1;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(45);
const TASK_TIMEOUT: Duration = Duration::from_secs(30);

/// Policy for admitted and queued work before executable code is replaced.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkPolicy {
    /// Stop admission, wait for admitted work to finish, and require that no
    /// non-terminal work remains.
    #[default]
    Drain,
    /// Refuse activation while any admitted or queued work exists, leaving the
    /// current binary and its work untouched.
    Pin,
    /// Drain admitted work and allow recoverable queued tasks to be resumed by
    /// the candidate release when the persisted schema is compatible.
    Migrate,
}

impl WorkPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Drain => "drain",
            Self::Pin => "pin",
            Self::Migrate => "migrate",
        }
    }
}

/// One upgrade request.
#[derive(Debug, Clone)]
pub struct UpgradeRequest {
    /// Instances root.
    pub root: PathBuf,
    /// Stable instance id.
    pub id: String,
    /// Verified candidate release directory under `packages/sha256/<digest>`.
    pub release: PathBuf,
    /// Permissions the operator explicitly authorizes for this upgrade.
    pub authorized_permissions: Vec<String>,
    /// Work settlement policy.
    pub work_policy: WorkPolicy,
    /// Input used to verify a real capability invocation. `None` uses
    /// `prompt:upgrade-readiness`.
    pub verify_input: Option<String>,
    /// Candidate listener. Defaults to the instance's previous address.
    pub addr: Option<String>,
    /// Additional narrow process environment supplied by the host composer.
    pub env: Vec<(String, String)>,
}

impl UpgradeRequest {
    /// Start an upgrade request for one verified release.
    pub fn new(
        root: impl Into<PathBuf>,
        id: impl Into<String>,
        release: impl Into<PathBuf>,
    ) -> Self {
        Self {
            root: root.into(),
            id: id.into(),
            release: release.into(),
            authorized_permissions: Vec::new(),
            work_policy: WorkPolicy::Drain,
            verify_input: None,
            addr: None,
            env: Vec::new(),
        }
    }
}

/// One scalar or set difference between the active and candidate release.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Difference {
    /// Active value or members.
    pub current: Vec<String>,
    /// Candidate value or members.
    pub candidate: Vec<String>,
    /// Candidate-only members.
    pub added: Vec<String>,
    /// Active-only members.
    pub removed: Vec<String>,
    /// Whether the two sides differ.
    pub changed: bool,
}

impl Difference {
    fn sets(
        current: impl IntoIterator<Item = String>,
        candidate: impl IntoIterator<Item = String>,
    ) -> Self {
        let current: BTreeSet<String> = current.into_iter().collect();
        let candidate: BTreeSet<String> = candidate.into_iter().collect();
        let added: Vec<String> = candidate.difference(&current).cloned().collect();
        let removed: Vec<String> = current.difference(&candidate).cloned().collect();
        Self {
            changed: !added.is_empty() || !removed.is_empty(),
            current: current.into_iter().collect(),
            candidate: candidate.into_iter().collect(),
            added,
            removed,
        }
    }

    fn scalar(current: impl Into<String>, candidate: impl Into<String>) -> Self {
        let current = current.into();
        let candidate = candidate.into();
        Self {
            changed: current != candidate,
            current: vec![current],
            candidate: vec![candidate],
            added: Vec::new(),
            removed: Vec::new(),
        }
    }
}

/// One verified release as seen by the instance planner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseView {
    /// Package name.
    pub package: String,
    /// Package version.
    pub version: String,
    /// Immutable release manifest digest.
    pub artifact_digest: String,
    /// Executable digest.
    pub binary_digest: String,
    /// Trusted signer identity recorded by the installer.
    pub signer: String,
    /// Target triple.
    pub target: String,
    /// Shared Worker runtime version required by the package.
    pub runtime: String,
    /// Pinned ADK version required by the package.
    pub adk: String,
    /// Package protocol declaration.
    pub protocol: String,
    /// Lowest supported protocol version.
    pub protocol_min: String,
    /// Highest supported protocol version.
    pub protocol_max: String,
    /// Source revision bound into signed metadata.
    pub source_revision: String,
    /// Actual capabilities reported by the candidate binary probe.
    pub capabilities: Vec<String>,
    /// Declared permissions from signed metadata.
    pub permissions: Vec<String>,
    /// Compiled hooks reported by the binary.
    pub hooks: Vec<String>,
    /// Native launcher modes reported by the binary.
    pub launcher_modes: Vec<String>,
    /// Persisted-data schema understood by the binary.
    pub data_schema: u32,
    /// Verified release directory.
    pub release_dir: PathBuf,
    /// Exact executable.
    pub binary: PathBuf,
}

/// Blocking or advisory finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Stable code.
    pub code: String,
    /// Actionable, redacted message.
    pub message: String,
}

/// One persisted profile revision and its compatibility with the candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileFinding {
    /// Profile name.
    pub name: String,
    /// Revision number.
    pub revision: u64,
    /// Whether this revision is active.
    pub active: bool,
    /// Candidate capabilities this revision requires.
    pub capabilities: Vec<String>,
    /// Candidate agent members this revision selects.
    pub members: Vec<String>,
    /// Candidate skills this revision references.
    pub skills: Vec<String>,
    /// Incompatibilities found against the candidate release.
    pub issues: Vec<Finding>,
}

/// One configured team assignment and its compatibility with the candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamFinding {
    /// Team name.
    pub name: String,
    /// Roster version.
    pub version: u64,
    /// Designated master identity.
    pub master: String,
    /// Member identities and their capability subsets.
    pub members: Vec<TeamMemberFinding>,
    /// Capabilities used by the roster that the candidate no longer provides.
    pub issues: Vec<Finding>,
}

/// One team member's narrowed capability subset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMemberFinding {
    /// Worker identity.
    pub worker: String,
    /// Role label. Labels never grant permissions.
    pub role: String,
    /// Capability subset.
    pub capabilities: Vec<String>,
}

/// One admitted or queued task visible before replacement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFinding {
    /// Task id.
    pub id: String,
    /// Current persisted state.
    pub state: String,
}

/// Work settlement view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkFinding {
    /// Distinct process health.
    pub process_health: String,
    /// Distinct lifecycle.
    pub lifecycle: String,
    /// Active invocations reported by the Worker.
    pub active: u32,
    /// Queued tasks reported by the Worker.
    pub queued: u32,
    /// `idle`, `executing`, or `unknown`.
    pub task_activity: String,
    /// Explicit policy supplied by the operator.
    pub policy: String,
    /// Planned or executed settlement action.
    pub action: String,
    /// Non-terminal persisted tasks.
    pub tasks: Vec<TaskFinding>,
}

/// One configuration path reference preserved by the upgrade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceFinding {
    /// `agents`, `prompts`, `skills`, or `storage`.
    pub kind: String,
    /// Resolved absolute path.
    pub path: PathBuf,
    /// Whether the path exists and is readable.
    pub exists: bool,
}

/// Operator configuration view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigFinding {
    /// Operator-selected configuration file.
    pub path: PathBuf,
    /// Directory used to resolve relative paths.
    pub config_dir: PathBuf,
    /// Whether this tool generated the configuration.
    pub generated: bool,
    /// Resolved external or instance-local references.
    pub references: Vec<ReferenceFinding>,
    /// The candidate release loaded this configuration and its assets/hooks.
    pub preflight_ok: bool,
    /// Preflight diagnostics when it failed.
    pub preflight_error: Option<String>,
}

/// Persisted schema comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaFinding {
    /// Active persisted schema.
    pub current: u32,
    /// Candidate persisted schema.
    pub candidate: u32,
    /// `compatible`, `migration`, or `incompatible`.
    pub status: String,
    /// A snapshot is available for data rollback.
    pub backup_available: bool,
}

/// Explicit permission authorization state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizationFinding {
    /// Permissions authorizing this upgrade.
    pub authorized: Vec<String>,
    /// Candidate-only permissions requiring explicit authorization.
    pub required: Vec<String>,
    /// Required permissions that the operator did not authorize.
    pub missing: Vec<String>,
    /// Whether every required permission was explicitly authorized.
    pub satisfied: bool,
}

/// Native launcher mode verification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LauncherFinding {
    /// Modes reported by the active release.
    pub current: Vec<String>,
    /// Modes reported by the candidate release.
    pub candidate: Vec<String>,
    /// Both `dev` and `serve` compositions were exercised for the candidate.
    pub verified: Vec<String>,
}

/// Signature and trust view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureFinding {
    /// Active signer.
    pub current: String,
    /// Candidate signer.
    pub candidate: String,
    /// Signer changed.
    pub changed: bool,
    /// Candidate is present in the verified installer cache.
    pub verified: bool,
}

/// Protocol compatibility view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolFinding {
    /// Active package protocol.
    pub current: String,
    /// Candidate package protocol.
    pub candidate: String,
    /// Candidate minimum.
    pub minimum: String,
    /// Candidate maximum.
    pub maximum: String,
}

/// Full dry-run and activation report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeReport {
    /// Report schema.
    pub schema: u32,
    /// Stable instance id.
    pub instance: String,
    /// Package name.
    pub package: String,
    /// Active release.
    pub current: ReleaseView,
    /// Candidate release.
    pub candidate: ReleaseView,
    /// Host/current/candidate platform differences.
    pub platform: Difference,
    /// Signer and verification state.
    pub signature: SignatureFinding,
    /// Protocol range.
    pub protocol: ProtocolFinding,
    /// Worker runtime compatibility difference.
    pub runtime: Difference,
    /// Capability difference from the real binary probes.
    pub capabilities: Difference,
    /// Permission difference requiring explicit authorization.
    pub permissions: Difference,
    /// Compiled hook difference.
    pub hooks: Difference,
    /// Persisted-data schema compatibility.
    pub schema_change: SchemaFinding,
    /// Profile revisions and incompatibilities.
    pub profiles: Vec<ProfileFinding>,
    /// Configured team assignments and incompatibilities.
    pub teams: Vec<TeamFinding>,
    /// Work settlement state.
    pub work: WorkFinding,
    /// Configuration and external path preservation.
    pub config: ConfigFinding,
    /// Native launcher modes.
    pub launcher: LauncherFinding,
    /// Explicit authorization state.
    pub authorization: AuthorizationFinding,
    /// Blocking findings. Empty means the candidate may activate.
    pub blocked: Vec<Finding>,
    /// Advisory findings that do not by themselves block activation.
    pub warnings: Vec<String>,
    /// An interrupted upgrade journal needs recovery first.
    pub recovery_needed: bool,
}

/// A completed activation's preservation and readiness evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationFinding {
    /// Worker lifecycle phase after activation.
    pub runtime_phase: String,
    /// Real capability operation mode: `task` or `inventory`.
    pub capability_operation: String,
    /// Capability task id when a real task ran.
    pub task_id: Option<String>,
    /// Capability task terminal state when a real task ran.
    pub task_state: Option<String>,
    /// Identity fields, configuration location, grants, limits, and paths
    /// remained unchanged.
    pub identity_preserved: bool,
    /// Profile names, revisions, and active revisions remained unchanged.
    pub profiles_preserved: bool,
    /// Team rosters remained unchanged.
    pub teams_preserved: bool,
    /// Every task visible before activation is still visible afterwards.
    pub history_preserved: bool,
    /// Workspace authorization remained unchanged.
    pub workspace_authorization_preserved: bool,
    /// Credential references remained unchanged.
    pub credential_refs_preserved: bool,
}

/// Durable upgrade journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeJournal {
    /// Journal schema.
    pub schema: u32,
    /// Stable instance id.
    pub instance: String,
    /// `activating`, `complete`, `rolled_back`, `recovered`, or `quarantined`.
    pub status: String,
    /// Start time.
    pub started_at: i64,
    /// Last update time.
    pub updated_at: i64,
    /// Work policy used for this upgrade.
    pub policy: WorkPolicy,
    /// Explicitly authorized permissions.
    pub authorized_permissions: Vec<String>,
    /// Record before activation.
    pub previous_record: InstanceRecord,
    /// Candidate record written during activation.
    pub candidate_record: InstanceRecord,
    /// Lifecycle state before replacement.
    pub previous_state: InstanceState,
    /// Data snapshot directory.
    pub backup: Option<PathBuf>,
    /// Digest over the backed-up data tree.
    pub backup_digest: Option<String>,
    /// Redacted failure.
    pub error: Option<String>,
    /// The previous binary record was restored.
    pub binary_restored: bool,
    /// Persisted data was restored from the snapshot.
    pub data_restored: bool,
    /// Data that could not be restored safely was moved here.
    pub quarantine: Option<PathBuf>,
    /// Successful verification evidence.
    pub verification: Option<VerificationFinding>,
}

/// Successful upgrade result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeOutcome {
    /// Final report.
    pub report: UpgradeReport,
    /// Durable journal.
    pub journal: UpgradeJournal,
    /// Readiness and preservation evidence.
    pub verification: VerificationFinding,
}

/// Successful interrupted-upgrade recovery result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryOutcome {
    /// Journal after recovery.
    pub journal: UpgradeJournal,
    /// Previous binary record restored.
    pub binary_restored: bool,
    /// Persisted data restored from the snapshot.
    pub data_restored: bool,
    /// Quarantine path when restoration could not be proven.
    pub quarantine: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default, PartialEq, Eq)]
struct PersistedState {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    profiles: BTreeMap<String, PersistedProfile>,
    #[serde(default)]
    teams: BTreeMap<String, PersistedTeam>,
    #[serde(default)]
    tasks: BTreeMap<String, PersistedTask>,
}

#[derive(Debug, Deserialize, Default, PartialEq, Eq)]
struct PersistedProfile {
    #[serde(default)]
    active_revision: u64,
    #[serde(default)]
    revisions: Vec<PersistedRevision>,
}

#[derive(Debug, Deserialize, Default, PartialEq, Eq)]
struct PersistedRevision {
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    skills: Vec<String>,
}

#[derive(Debug, Deserialize, Default, PartialEq, Eq)]
struct PersistedTeam {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: u64,
    #[serde(default)]
    master: String,
    #[serde(default)]
    members: Vec<PersistedTeamMember>,
}

#[derive(Debug, Deserialize, Default, PartialEq, Eq)]
struct PersistedTeamMember {
    #[serde(default)]
    worker: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Debug, Deserialize, Default, PartialEq, Eq)]
struct PersistedTask {
    #[serde(default)]
    state: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(crate) struct Probe {
    #[serde(default)]
    pub(crate) package: String,
    #[serde(default)]
    pub(crate) version: String,
    #[serde(default)]
    pub(crate) protocol: String,
    #[serde(default)]
    pub(crate) adk: String,
    #[serde(default)]
    pub(crate) registered: Vec<String>,
    #[serde(default)]
    pub(crate) hooks: Vec<String>,
    #[serde(default)]
    pub(crate) launcher_modes: Vec<String>,
    #[serde(default)]
    pub(crate) serve_ready: bool,
    #[serde(default)]
    pub(crate) data_schema: u32,
}

#[derive(Debug, Default)]
struct Preflight {
    capabilities: Vec<String>,
    configured: Vec<String>,
    skills: Vec<String>,
    references: Vec<ReferenceFinding>,
    config_dir: PathBuf,
    error: Option<String>,
}

struct PlanData {
    report: UpgradeReport,
    current_record: InstanceRecord,
    current_state: InstanceState,
    candidate_record: InstanceRecord,
    candidate_metadata: ReleaseMetadata,
    persisted: PersistedState,
}

/// Build a dry-run report without modifying the active instance.
pub fn plan(request: &UpgradeRequest) -> Result<UpgradeReport, InstanceError> {
    Ok(build_plan(request)?.report)
}

/// Apply a verified upgrade. Returns only after readiness and a real
/// capability invocation succeeded; otherwise restores the prior binary and
/// data snapshot.
pub fn apply(request: &UpgradeRequest) -> Result<UpgradeOutcome, InstanceError> {
    let paths = instance_paths(&request.root, &request.id)?;
    if let Some(journal) = load_journal(&paths)? {
        if !is_terminal_journal(&journal.status) {
            return Err(InstanceError::new(
                "upgrade.interrupted",
                format!(
                    "instance '{}' has an interrupted upgrade in status '{}'; run gb-instance recover before retrying",
                    request.id, journal.status
                ),
            ));
        }
    }

    let plan = build_plan(request)?;
    if !plan.report.blocked.is_empty() {
        return Err(blocked_error(&plan.report));
    }
    if !plan.report.authorization.satisfied {
        return Err(InstanceError::new(
            "upgrade.permission_unauthorized",
            format!(
                "candidate release requests additional access that was not explicitly authorized: {}",
                plan.report.authorization.missing.join(", ")
            ),
        ));
    }
    let paths = instance_paths(&request.root, &request.id)?;
    let mut state = load_state(&paths)?;
    let was_running = plan.report.work.process_health == "running"
        || (state.lifecycle == "running" && state.pid.is_some());
    let previous_state = state.clone();
    let previous_record = plan.current_record.clone();

    settle_work(&paths, &previous_record, &previous_state, &plan.report)?;
    state = load_state(&paths)?;
    if was_running {
        match process::stop(&request.root, &request.id) {
            Ok(outcome) => state = outcome.state,
            Err(error) => {
                return Err(InstanceError::new(
                    error.code,
                    format!(
                        "cannot stop the active instance before replacement: {}",
                        error.message
                    ),
                ))
            }
        }
        if request.work_policy == WorkPolicy::Drain {
            let remaining = nonterminal_tasks(&read_persisted(&previous_record.data_dir));
            if !remaining.is_empty() {
                let _ = process::start_with_env(
                    &request.root,
                    &request.id,
                    previous_state.addr.as_deref(),
                    &request.env,
                );
                return Err(InstanceError::new(
                    "upgrade.work_unsettled",
                    format!(
                        "{} task(s) remained non-terminal after the bounded drain: {}",
                        remaining.len(),
                        remaining.join(",")
                    ),
                ));
            }
        }
    }

    let backup = match backup_data(&paths, &previous_record, &previous_state) {
        Ok(backup) => backup,
        Err(error) => {
            if was_running {
                let _ = process::start_with_env(
                    &request.root,
                    &request.id,
                    previous_state.addr.as_deref(),
                    &request.env,
                );
            }
            return Err(error);
        }
    };

    let mut journal = UpgradeJournal {
        schema: JOURNAL_SCHEMA,
        instance: request.id.clone(),
        status: "activating".into(),
        started_at: now_ms(),
        updated_at: now_ms(),
        policy: request.work_policy,
        authorized_permissions: request.authorized_permissions.clone(),
        previous_record: previous_record.clone(),
        candidate_record: plan.candidate_record.clone(),
        previous_state: previous_state.clone(),
        backup: Some(backup.directory.join("data")),
        backup_digest: Some(backup.digest.clone()),
        error: None,
        binary_restored: false,
        data_restored: false,
        quarantine: None,
        verification: None,
    };
    write_journal(&paths, &journal)?;

    if let Err(error) = materialize_seeds(&paths, &plan.candidate_record, &plan.candidate_metadata)
    {
        let _ = restore_binary_and_data(
            &request.root,
            &paths,
            &previous_record,
            &previous_state,
            &backup.directory.join("data"),
            was_running,
            &request.env,
        );
        journal.status = "rolled_back".into();
        journal.error = Some(format!("{}: {}", error.code, error.message));
        journal.binary_restored = true;
        journal.data_restored = true;
        journal.updated_at = now_ms();
        write_journal(&paths, &journal)?;
        return Err(error);
    }

    state.lifecycle = "upgrading".into();
    state.last_error = None;
    save_state(&paths, &state)?;
    write_json(&paths.record, &plan.candidate_record, 0o600)?;

    let addr = request.addr.clone().or_else(|| previous_state.addr.clone());
    if let Err(error) =
        process::start_with_env(&request.root, &request.id, addr.as_deref(), &request.env)
    {
        return rollback_after_failure(
            request,
            &paths,
            plan.report,
            journal,
            backup,
            previous_record,
            previous_state,
            was_running,
            error,
        );
    }

    match verify_activation(request, &paths, &plan) {
        Ok(verification) => {
            let mut report = plan.report;
            report.work.action = "activated_after_drain".into();
            journal.status = "complete".into();
            journal.verification = Some(verification.clone());
            journal.updated_at = now_ms();
            write_journal(&paths, &journal)?;
            let mut state = load_state(&paths)?;
            state.lifecycle = "running".into();
            state.last_error = None;
            save_state(&paths, &state)?;
            Ok(UpgradeOutcome {
                report,
                journal,
                verification,
            })
        }
        Err(error) => rollback_after_failure(
            request,
            &paths,
            plan.report,
            journal,
            backup,
            previous_record,
            previous_state,
            was_running,
            error,
        ),
    }
}

/// Recover an interrupted upgrade from its durable journal. This restores the
/// previous binary record and, when available, the data snapshot. Unprovable
/// restoration quarantines the data instead of claiming success.
pub fn recover(root: &Path, id: &str) -> Result<RecoveryOutcome, InstanceError> {
    recover_with_env(root, id, &[])
}

/// Recover an interrupted upgrade while supplying narrow host process
/// environment entries to the restored Worker.
pub fn recover_with_env(
    root: &Path,
    id: &str,
    environment: &[(String, String)],
) -> Result<RecoveryOutcome, InstanceError> {
    let paths = instance_paths(root, id)?;
    let Some(mut journal) = load_journal(&paths)? else {
        return Err(InstanceError::new(
            "upgrade.no_recovery",
            format!("instance '{id}' has no upgrade journal to recover"),
        ));
    };
    if is_terminal_journal(&journal.status) {
        return Err(InstanceError::new(
            "upgrade.nothing_to_recover",
            format!(
                "instance '{id}' upgrade journal is already '{}'",
                journal.status
            ),
        ));
    }

    let _ = process::stop(root, id);
    let mut binary_restored = false;
    let mut data_restored = false;
    let mut quarantine = None;
    let mut failure = None;

    match write_json(&paths.record, &journal.previous_record, 0o600) {
        Ok(()) => binary_restored = true,
        Err(error) => failure = Some(error),
    }
    if let Some(backup) = &journal.backup {
        match restore_tree(backup, &journal.previous_record.data_dir) {
            Ok(()) => data_restored = true,
            Err(error) => {
                failure = Some(error);
                if let Ok(path) =
                    quarantine_tree(&paths, &journal.previous_record.data_dir, &journal.instance)
                {
                    quarantine = Some(path);
                }
            }
        }
    }

    if let Some(error) = failure {
        journal.status = "quarantined".into();
        journal.error = Some(format!("{}: {}", error.code, error.message));
        journal.binary_restored = binary_restored;
        journal.data_restored = false;
        journal.quarantine = quarantine.clone();
        journal.updated_at = now_ms();
        write_journal(&paths, &journal)?;
        let mut state = load_state(&paths)?;
        state.lifecycle = "quarantined".into();
        state.pid = None;
        state.addr = None;
        state.last_error = journal.error.clone();
        save_state(&paths, &state)?;
        return Err(InstanceError::new(
            "upgrade.quarantined",
            format!(
                "interrupted upgrade could not restore persisted data; failed data was quarantined at {}",
                quarantine
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| journal.previous_record.data_dir.display().to_string())
            ),
        ));
    }

    // Restore the prior lifecycle only after the binary and data are both back.
    let mut state = journal.previous_state.clone();
    if state.lifecycle == "running" || state.pid.is_some() {
        process::start_with_env(root, id, state.addr.as_deref(), environment)?;
    } else {
        state.pid = None;
        state.addr = None;
        save_state(&paths, &state)?;
    }

    journal.status = "recovered".into();
    journal.binary_restored = binary_restored;
    journal.data_restored = data_restored;
    journal.quarantine = quarantine.clone();
    journal.error = None;
    journal.updated_at = now_ms();
    write_journal(&paths, &journal)?;
    Ok(RecoveryOutcome {
        journal,
        binary_restored,
        data_restored,
        quarantine,
    })
}

fn build_plan(request: &UpgradeRequest) -> Result<PlanData, InstanceError> {
    let paths = instance_paths(&request.root, &request.id)?;
    let current_record = load_record(&paths)?;
    let current_state = load_state(&paths)?;
    let mut blocked = Vec::new();
    if current_state.lifecycle == "quarantined" {
        blocked.push(Finding {
            code: "upgrade.instance_quarantined".into(),
            message: "the instance is quarantined and must be recovered or replaced before upgrade"
                .into(),
        });
    }
    let persisted = read_persisted(&current_record.data_dir);
    let current_installed = load_release(&current_record.release_dir)?;
    let candidate_installed = load_release(&absolute(&request.release)?)?;
    if current_installed.package != candidate_installed.package {
        return Err(InstanceError::new(
            "upgrade.package_mismatch",
            format!(
                "instance '{}' runs package '{}'; candidate is '{}'",
                request.id, current_installed.package, candidate_installed.package
            ),
        ));
    }
    let current_metadata = release_metadata(&current_installed)?;
    let candidate_metadata = release_metadata(&candidate_installed)?;

    let current_probe = run_probe(&current_installed.binary).unwrap_or_else(|_| Probe {
        package: current_installed.package.clone(),
        version: current_installed.version.clone(),
        protocol: current_metadata.package.protocol.clone(),
        adk: current_metadata.package.adk.clone(),
        registered: current_installed.registered.clone(),
        hooks: current_metadata.package.hooks.clone(),
        launcher_modes: current_installed.launcher_modes.clone(),
        serve_ready: true,
        data_schema: persisted.schema,
    });
    let candidate_probe = match run_probe(&candidate_installed.binary) {
        Ok(probe) => probe,
        Err(message) => {
            blocked.push(Finding {
                code: "upgrade.candidate_probe_failed".into(),
                message,
            });
            Probe {
                package: candidate_installed.package.clone(),
                version: candidate_installed.version.clone(),
                protocol: candidate_metadata.package.protocol.clone(),
                adk: candidate_metadata.package.adk.clone(),
                registered: candidate_installed.registered.clone(),
                hooks: candidate_metadata.package.hooks.clone(),
                launcher_modes: candidate_installed.launcher_modes.clone(),
                serve_ready: true,
                data_schema: 0,
            }
        }
    };

    let preflight = preflight_config(request, &candidate_installed.binary);
    if let Some(error) = &preflight.error {
        blocked.push(Finding {
            code: "upgrade.preflight_failed".into(),
            message: error.clone(),
        });
    }
    let candidate_capabilities: Vec<String> = if preflight.capabilities.is_empty() {
        candidate_probe.registered.clone()
    } else {
        preflight.capabilities.clone()
    };
    // Profiles may select configured agents in addition to compiled
    // capabilities. The preflight composition reports both; only the compiled
    // set participates in the capability difference above.
    let mut profile_capabilities: BTreeSet<String> =
        candidate_capabilities.iter().cloned().collect();
    profile_capabilities.extend(preflight.configured.iter().cloned());
    let candidate_skills: BTreeSet<String> = preflight.skills.iter().cloned().collect();

    let current_permissions = declared_permissions(&current_metadata);
    let candidate_permissions = declared_permissions(&candidate_metadata);
    let permission_diff = Difference::sets(
        current_permissions.iter().cloned(),
        candidate_permissions.iter().cloned(),
    );
    let authorized: BTreeSet<&str> = request
        .authorized_permissions
        .iter()
        .map(String::as_str)
        .collect();
    let missing: Vec<String> = permission_diff
        .added
        .iter()
        .filter(|permission| !authorized.contains(permission.as_str()))
        .cloned()
        .collect();
    let authorization = AuthorizationFinding {
        authorized: request.authorized_permissions.clone(),
        required: permission_diff.added.clone(),
        missing: missing.clone(),
        satisfied: missing.is_empty(),
    };
    if !missing.is_empty() {
        blocked.push(Finding {
            code: "upgrade.permission_unauthorized".into(),
            message: format!(
                "candidate release requests additional access that must be explicitly authorized: {}",
                missing.join(", ")
            ),
        });
    }

    let current_schema = if persisted.schema == 0 {
        current_probe.data_schema
    } else {
        persisted.schema
    };
    let candidate_schema = candidate_probe.data_schema;
    let schema_status = if candidate_schema == 0 {
        "unknown"
    } else if candidate_schema == current_schema {
        "compatible"
    } else if candidate_schema > current_schema {
        "migration"
    } else {
        "incompatible"
    };
    if schema_status == "incompatible" {
        blocked.push(Finding {
            code: "upgrade.schema_incompatible".into(),
            message: format!(
                "candidate persisted schema {candidate_schema} cannot read the instance's schema {current_schema}"
            ),
        });
    }

    let profiles = profile_findings(
        &persisted,
        &profile_capabilities,
        &candidate_skills,
        &mut blocked,
    );
    let teams = team_findings(&persisted, &profile_capabilities, &mut blocked);
    let work = work_finding(
        &paths,
        &current_record,
        &current_state,
        &persisted,
        request.work_policy,
        &mut blocked,
    );

    let launcher = launcher_finding(&current_installed, &candidate_installed, &mut blocked);
    let platform = Difference::scalar(
        current_metadata.target.clone(),
        candidate_metadata.target.clone(),
    );
    if candidate_metadata.target != install::TrustPolicy::host_target() {
        blocked.push(Finding {
            code: "upgrade.platform_incompatible".into(),
            message: format!(
                "candidate target '{}' is not the host target '{}'",
                candidate_metadata.target,
                install::TrustPolicy::host_target()
            ),
        });
    }
    if platform.changed {
        blocked.push(Finding {
            code: "upgrade.platform_changed".into(),
            message: format!(
                "candidate target '{}' differs from the active target '{}'",
                candidate_metadata.target, current_metadata.target
            ),
        });
    }
    let protocol = ProtocolFinding {
        current: current_metadata.package.protocol.clone(),
        candidate: candidate_metadata.package.protocol.clone(),
        minimum: candidate_metadata.protocol_range().0.to_string(),
        maximum: candidate_metadata.protocol_range().1.to_string(),
    };
    let runtime = Difference::scalar(
        current_metadata.package.runtime.clone(),
        candidate_metadata.package.runtime.clone(),
    );
    if runtime.changed {
        blocked.push(Finding {
            code: "upgrade.runtime_changed".into(),
            message: format!(
                "candidate requires Worker runtime '{}' but the instance runs '{}'",
                candidate_metadata.package.runtime, current_metadata.package.runtime
            ),
        });
    }
    let capabilities = Difference::sets(
        current_probe.registered.clone(),
        candidate_capabilities.clone(),
    );
    let hooks = Difference::sets(current_probe.hooks.clone(), candidate_probe.hooks.clone());

    let mut warnings = Vec::new();
    let recovery_needed =
        load_journal(&paths)?.is_some_and(|journal| !is_terminal_journal(&journal.status));
    if recovery_needed {
        blocked.push(Finding {
            code: "upgrade.interrupted".into(),
            message:
                "an interrupted upgrade journal must be recovered before planning another upgrade"
                    .into(),
        });
    }
    if !authorization.required.is_empty() {
        warnings.push(format!(
            "candidate declares {} permission(s) that were not present in the active release",
            authorization.required.len()
        ));
    }
    if schema_status == "migration" {
        warnings.push(
            "candidate migrates persisted data; rollback restores the pre-upgrade snapshot and must be proven before completion"
                .into(),
        );
    }

    let report = UpgradeReport {
        schema: 1,
        instance: current_record.id.clone(),
        package: current_record.package.clone(),
        current: release_view(&current_installed, &current_metadata, &current_probe),
        candidate: release_view(&candidate_installed, &candidate_metadata, &candidate_probe),
        platform,
        signature: SignatureFinding {
            current: current_installed.signer.clone(),
            candidate: candidate_installed.signer.clone(),
            changed: current_installed.signer != candidate_installed.signer,
            verified: true,
        },
        protocol,
        runtime,
        capabilities,
        permissions: permission_diff,
        hooks,
        schema_change: SchemaFinding {
            current: current_schema,
            candidate: candidate_schema,
            status: schema_status.into(),
            // Activation always snapshots persisted data before replacing the
            // executable; an incompatible schema is blocked before that.
            backup_available: schema_status != "incompatible",
        },
        profiles,
        teams,
        work,
        config: ConfigFinding {
            path: current_record.config_path.clone(),
            config_dir: preflight.config_dir.clone(),
            generated: current_record.generated_config,
            references: preflight.references.clone(),
            preflight_ok: preflight.error.is_none(),
            preflight_error: preflight.error.clone(),
        },
        launcher,
        authorization,
        blocked,
        warnings,
        recovery_needed,
    };

    let mut candidate_record = current_record.clone();
    candidate_record.package = candidate_installed.package.clone();
    candidate_record.version = candidate_installed.version.clone();
    candidate_record.artifact_digest = candidate_installed.artifact_digest.clone();
    candidate_record.binary_digest =
        common::bytes_digest(&std::fs::read(&candidate_installed.binary).map_err(storage)?);
    candidate_record.release_dir = candidate_installed.directory.clone();
    candidate_record.binary = candidate_installed.binary.clone();
    for seed in &candidate_installed.seeds {
        if !candidate_record.seed_assets.contains(&seed.path) {
            candidate_record.seed_assets.push(seed.path.clone());
        }
    }

    Ok(PlanData {
        report,
        current_record,
        current_state,
        candidate_record,
        candidate_metadata,
        persisted,
    })
}

fn release_view(installed: &Installed, metadata: &ReleaseMetadata, probe: &Probe) -> ReleaseView {
    ReleaseView {
        package: installed.package.clone(),
        version: installed.version.clone(),
        artifact_digest: installed.artifact_digest.clone(),
        binary_digest: metadata.binary_digest.clone(),
        signer: installed.signer.clone(),
        target: metadata.target.clone(),
        runtime: metadata.package.runtime.clone(),
        adk: metadata.package.adk.clone(),
        protocol: metadata.package.protocol.clone(),
        protocol_min: metadata.protocol_range().0.to_string(),
        protocol_max: metadata.protocol_range().1.to_string(),
        source_revision: metadata.source_revision.clone(),
        capabilities: probe.registered.clone(),
        permissions: declared_permissions(metadata).into_iter().collect(),
        hooks: probe.hooks.clone(),
        launcher_modes: probe.launcher_modes.clone(),
        data_schema: probe.data_schema,
        release_dir: installed.directory.clone(),
        binary: installed.binary.clone(),
    }
}

fn declared_permissions(metadata: &ReleaseMetadata) -> BTreeSet<String> {
    let mut permissions: BTreeSet<String> = metadata
        .package
        .required_permissions
        .iter()
        .cloned()
        .collect();
    for capability in &metadata.package.capabilities {
        permissions.extend(capability.permissions.iter().cloned());
    }
    permissions
}

fn profile_findings(
    persisted: &PersistedState,
    candidate_capabilities: &BTreeSet<String>,
    candidate_skills: &BTreeSet<String>,
    blocked: &mut Vec<Finding>,
) -> Vec<ProfileFinding> {
    let mut findings = Vec::new();
    for (name, profile) in &persisted.profiles {
        for revision in &profile.revisions {
            let mut issues = Vec::new();
            for capability in &revision.capabilities {
                if capability == "*" {
                    continue;
                }
                if !candidate_capabilities.contains(capability) {
                    let issue = Finding {
                        code: "upgrade.profile_capability_missing".into(),
                        message: format!(
                            "profile '{name}' revision {} requires capability '{capability}' which the candidate binary does not register",
                            revision.revision
                        ),
                    };
                    blocked.push(issue.clone());
                    issues.push(issue);
                }
            }
            for member in &revision.members {
                if !candidate_capabilities.contains(member) {
                    let issue = Finding {
                        code: "upgrade.profile_member_missing".into(),
                        message: format!(
                            "profile '{name}' revision {} selects agent '{member}' which the candidate binary does not register",
                            revision.revision
                        ),
                    };
                    blocked.push(issue.clone());
                    issues.push(issue);
                }
            }
            for skill in &revision.skills {
                if !candidate_skills.is_empty() && !candidate_skills.contains(skill) {
                    let issue = Finding {
                        code: "upgrade.profile_skill_missing".into(),
                        message: format!(
                            "profile '{name}' revision {} requires skill '{skill}' which the candidate configuration does not discover",
                            revision.revision
                        ),
                    };
                    blocked.push(issue.clone());
                    issues.push(issue);
                }
            }
            findings.push(ProfileFinding {
                name: name.rsplit('\u{1f}').next().unwrap_or(name).to_string(),
                revision: revision.revision,
                active: profile.active_revision == revision.revision,
                capabilities: revision.capabilities.clone(),
                members: revision.members.clone(),
                skills: revision.skills.clone(),
                issues,
            });
        }
    }
    findings
}

fn team_findings(
    persisted: &PersistedState,
    candidate_capabilities: &BTreeSet<String>,
    blocked: &mut Vec<Finding>,
) -> Vec<TeamFinding> {
    let mut findings = Vec::new();
    for (name, team) in &persisted.teams {
        let mut issues = Vec::new();
        let mut members = Vec::new();
        for member in &team.members {
            for capability in &member.capabilities {
                if capability != "*" && !candidate_capabilities.contains(capability) {
                    let issue = Finding {
                        code: "upgrade.team_capability_missing".into(),
                        message: format!(
                            "team '{name}' member '{}' is assigned capability '{capability}' which the candidate binary does not register",
                            member.worker
                        ),
                    };
                    blocked.push(issue.clone());
                    issues.push(issue);
                }
            }
            members.push(TeamMemberFinding {
                worker: member.worker.clone(),
                role: member.role.clone(),
                capabilities: member.capabilities.clone(),
            });
        }
        findings.push(TeamFinding {
            name: if team.name.is_empty() {
                name.clone()
            } else {
                team.name.clone()
            },
            version: team.version,
            master: team.master.clone(),
            members,
            issues,
        });
    }
    findings
}

fn work_finding(
    paths: &crate::layout::InstancePaths,
    record: &InstanceRecord,
    state: &InstanceState,
    persisted: &PersistedState,
    policy: WorkPolicy,
    blocked: &mut Vec<Finding>,
) -> WorkFinding {
    let token = token_for(paths, record);
    let base = state.addr.as_ref().map(|addr| format!("http://{addr}"));
    let mut active = 0;
    let mut queued = 0;
    let mut process_health = match state.pid {
        Some(pid) if process::process_matches(pid, &record.config_path) => "running".to_string(),
        Some(_) => "exited".to_string(),
        None => "unknown".to_string(),
    };
    let mut task_activity = "unknown".to_string();
    if let Some(base) = &base {
        if let Ok(status) = process::get_json(&format!("{base}/v1/status"), token.as_deref()) {
            process_health = "running".into();
            if let Some(capacity) = status.get("capacity") {
                active = capacity.get("active").and_then(Value::as_u64).unwrap_or(0) as u32;
                queued = capacity.get("queued").and_then(Value::as_u64).unwrap_or(0) as u32;
            }
            task_activity = if active == 0 && queued == 0 {
                "idle".into()
            } else {
                "executing".into()
            };
        }
    }
    let tasks: Vec<TaskFinding> = persisted
        .tasks
        .iter()
        .filter(|(_, task)| !is_terminal_task_state(&task.state))
        .map(|(id, task)| TaskFinding {
            id: id.clone(),
            state: task.state.clone(),
        })
        .collect();
    let has_work = active > 0 || queued > 0 || !tasks.is_empty();
    let action = if !has_work {
        "none"
    } else {
        match policy {
            WorkPolicy::Drain => "drain",
            WorkPolicy::Pin => "pin",
            WorkPolicy::Migrate => "migrate",
        }
    }
    .to_string();
    if has_work && policy == WorkPolicy::Pin {
        blocked.push(Finding {
            code: "upgrade.work_pinned".into(),
            message: format!(
                "{} non-terminal task(s) are pinned to the active binary; finish or cancel them before upgrading",
                tasks.len().max(active as usize + queued as usize)
            ),
        });
    }
    WorkFinding {
        process_health,
        lifecycle: state.lifecycle.clone(),
        active,
        queued,
        task_activity,
        policy: policy.as_str().into(),
        action,
        tasks,
    }
}

fn launcher_finding(
    current: &Installed,
    candidate: &Installed,
    blocked: &mut Vec<Finding>,
) -> LauncherFinding {
    let temp = match temp_dir() {
        Ok(temp) => temp,
        Err(error) => {
            blocked.push(Finding {
                code: "upgrade.preflight_failed".into(),
                message: error.message,
            });
            return LauncherFinding::default();
        }
    };
    let _guard = TempGuard::new(temp.clone());
    let envs = isolated_environment(&temp);
    let mut verified = Vec::new();
    for mode in &candidate.launcher_modes {
        let args: Vec<&str> = match mode.as_str() {
            "dev" | "console" => vec!["dev", "--probe-only"],
            "serve" => vec!["serve", "--probe-only", "--addr", "127.0.0.1:0"],
            _ => continue,
        };
        match run_binary(&candidate.binary, &args, &temp, &envs) {
            Ok(output) if output.status == 0 => verified.push(mode.clone()),
            Ok(output) => blocked.push(Finding {
                code: "upgrade.launcher_failed".into(),
                message: format!(
                    "candidate launcher mode '{mode}' exited with {}: {}",
                    output.status,
                    common::redact(output.stderr.trim())
                ),
            }),
            Err(message) => blocked.push(Finding {
                code: "upgrade.launcher_failed".into(),
                message: format!("candidate launcher mode '{mode}' could not run: {message}"),
            }),
        }
    }
    LauncherFinding {
        current: current.launcher_modes.clone(),
        candidate: candidate.launcher_modes.clone(),
        verified,
    }
}

fn preflight_config(request: &UpgradeRequest, binary: &Path) -> Preflight {
    let paths = match instance_paths(&request.root, &request.id) {
        Ok(paths) => paths,
        Err(error) => {
            return Preflight {
                error: Some(format!("{}: {}", error.code, error.message)),
                ..Preflight::default()
            }
        }
    };
    let record = match load_record(&paths) {
        Ok(record) => record,
        Err(error) => {
            return Preflight {
                error: Some(format!("{}: {}", error.code, error.message)),
                ..Preflight::default()
            }
        }
    };
    let config_path = record.config_path.clone();
    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let text = match std::fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(_) => {
            return Preflight {
                config_dir,
                error: Some("instance.config_invalid: operator configuration is unreadable".into()),
                ..Preflight::default()
            }
        }
    };
    let mut value: toml::Value = match toml::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            return Preflight {
                config_dir,
                error: Some(format!(
                    "instance.config_invalid: operator configuration is not valid TOML: {error}"
                )),
                ..Preflight::default()
            }
        }
    };
    let temp = match temp_dir() {
        Ok(temp) => temp,
        Err(error) => {
            return Preflight {
                config_dir,
                error: Some(error.message),
                ..Preflight::default()
            }
        }
    };
    let _guard = TempGuard::new(temp.clone());
    let references = rewrite_config_paths(&mut value, &config_dir, &temp);
    if let Err(error) = std::fs::create_dir_all(temp.join("data")) {
        return Preflight {
            config_dir,
            references,
            error: Some(format!(
                "instance.storage_unavailable: temp preflight data directory could not be created: {error}"
            )),
            ..Preflight::default()
        };
    }
    let state = record.data_dir.join("state.json");
    if state.is_file() {
        let _ = std::fs::copy(&state, temp.join("data/state.json"));
    }
    let temp_config = temp.join("worker.toml");
    let encoded = match toml::to_string(&value) {
        Ok(encoded) => encoded,
        Err(error) => {
            return Preflight {
                config_dir,
                references,
                error: Some(format!(
                    "instance.config_invalid: preflight configuration could not be encoded: {error}"
                )),
                ..Preflight::default()
            }
        }
    };
    if let Err(error) = std::fs::write(&temp_config, encoded) {
        return Preflight {
            config_dir,
            references,
            error: Some(format!(
                "instance.storage_unavailable: preflight configuration could not be written: {error}"
            )),
            ..Preflight::default()
        };
    }
    let envs = isolated_environment(&temp);
    let args = vec![
        "capabilities",
        "--config",
        temp_config.to_str().unwrap_or_default(),
        "--json",
    ];
    match run_binary(binary, &args, &temp, &envs) {
        Ok(output) if output.status == 0 => {
            let parsed: Value = serde_json::from_str(output.stdout.trim()).unwrap_or(Value::Null);
            let capabilities = parsed
                .pointer("/installed/capabilities")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.get("id").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let configured = parsed
                .get("configured")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.get("id").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let skills = parsed
                .get("skills")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Preflight {
                capabilities,
                configured,
                skills,
                references,
                config_dir,
                error: None,
            }
        }
        Ok(output) => Preflight {
            config_dir,
            references,
            error: Some(format!(
                "candidate configuration preflight exited with {}: {}",
                output.status,
                common::redact(output.stderr.trim())
            )),
            ..Preflight::default()
        },
        Err(message) => Preflight {
            config_dir,
            references,
            error: Some(message),
            ..Preflight::default()
        },
    }
}

fn rewrite_config_paths(
    value: &mut toml::Value,
    config_dir: &Path,
    temp: &Path,
) -> Vec<ReferenceFinding> {
    let mut references = Vec::new();
    let table = match value.as_table_mut() {
        Some(table) => table,
        None => return references,
    };
    if let Some(paths) = table.get_mut("paths").and_then(toml::Value::as_table_mut) {
        for kind in ["agents", "prompts", "skills"] {
            if let Some(entry) = paths.get_mut(kind) {
                if let Some(text) = entry.as_str() {
                    let resolved = resolve_path(config_dir, text);
                    let exists = resolved.exists();
                    references.push(ReferenceFinding {
                        kind: kind.into(),
                        path: resolved.clone(),
                        exists,
                    });
                    *entry = toml::Value::String(resolved.display().to_string());
                }
            }
        }
    }
    if let Some(storage) = table.get_mut("storage").and_then(toml::Value::as_table_mut) {
        if let Some(entry) = storage.get_mut("directory") {
            if let Some(text) = entry.as_str() {
                let resolved = resolve_path(config_dir, text);
                let exists = resolved.exists();
                references.push(ReferenceFinding {
                    kind: "storage".into(),
                    path: resolved,
                    exists,
                });
            }
            *entry = toml::Value::String(temp.join("data").display().to_string());
        }
    }
    references
}

fn resolve_path(config_dir: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        config_dir.join(path)
    }
}

fn settle_work(
    paths: &crate::layout::InstancePaths,
    record: &InstanceRecord,
    state: &InstanceState,
    report: &UpgradeReport,
) -> Result<(), InstanceError> {
    if report.work.process_health != "running" {
        return Ok(());
    }
    if report.work.policy == WorkPolicy::Pin.as_str() {
        if report.work.active > 0 || report.work.queued > 0 || !report.work.tasks.is_empty() {
            return Err(InstanceError::new(
                "upgrade.work_pinned",
                "active or queued work is pinned to the current binary; finish or cancel it first",
            ));
        }
        return Ok(());
    }
    let token = token_for(paths, record);
    let Some(base) = state.addr.as_ref().map(|addr| format!("http://{addr}")) else {
        return Ok(());
    };
    let Some(token) = token else {
        // SIGTERM still triggers the Worker's own bounded graceful drain.
        return Ok(());
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|error| InstanceError::new("upgrade.drain_failed", error.to_string()))?;
    let response = client
        .post(format!("{base}/v1/drain"))
        .bearer_auth(token)
        .send()
        .map_err(|error| {
            InstanceError::new(
                "upgrade.drain_failed",
                format!("drain request failed: {error}"),
            )
        })?;
    let status = response.status();
    let body: Value = response.json().unwrap_or(Value::Null);
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::UNAUTHORIZED {
        // A caller-scoped token may lack instance:admin. The host supervisor
        // still stops the process with SIGTERM, which invokes the Worker's own
        // bounded drain; activation re-checks remaining work before replacing
        // code.
        return Ok(());
    }
    if !status.is_success() {
        return Err(InstanceError::new(
            "upgrade.drain_failed",
            format!(
                "drain request was refused with {}: {}",
                status,
                common::redact(&body.to_string())
            ),
        ));
    }
    let timed_out = body
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let unfinished = body.get("unfinished").and_then(Value::as_u64).unwrap_or(0);
    if (timed_out || unfinished > 0) && report.work.policy != WorkPolicy::Migrate.as_str() {
        return Err(InstanceError::new(
            "upgrade.work_unsettled",
            format!(
                "drain finished with timed_out={timed_out} and {unfinished} non-terminal task(s); policy '{}' does not allow replacing the executable",
                report.work.policy
            ),
        ));
    }
    Ok(())
}

fn verify_activation(
    request: &UpgradeRequest,
    paths: &crate::layout::InstancePaths,
    plan: &PlanData,
) -> Result<VerificationFinding, InstanceError> {
    let record = load_record(paths)?;
    let state = load_state(paths)?;
    let token = token_for(paths, &record);
    let Some(token) = token else {
        return Err(InstanceError::new(
            "upgrade.verify_failed",
            "activated instance has no management token for readiness verification",
        ));
    };
    let Some(addr) = state.addr.as_ref() else {
        return Err(InstanceError::new(
            "upgrade.verify_failed",
            "activated instance did not record a listener address",
        ));
    };
    let base = format!("http://{addr}");
    let status =
        process::get_json(&format!("{base}/v1/status"), Some(&token)).map_err(|error| {
            InstanceError::new(
                "upgrade.verify_failed",
                format!("readiness endpoint did not answer: {error}"),
            )
        })?;
    let phase = status
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if phase != "ready" {
        return Err(InstanceError::new(
            "upgrade.verify_failed",
            format!("activated instance reported phase '{phase}', not 'ready'"),
        ));
    }
    let package = status
        .get("package")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let version = status
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if package != plan.candidate_record.package || version != plan.candidate_record.version {
        return Err(InstanceError::new(
            "upgrade.verify_failed",
            format!(
                "activated instance reports {package} {version}, expected {} {}",
                plan.candidate_record.package, plan.candidate_record.version
            ),
        ));
    }

    let before = &plan.persisted;
    let after = read_persisted(&plan.current_record.data_dir);
    let active_profile = load_state(paths)?.active_profile;

    // Candidate startup may create a missing built-in default profile in a
    // namespace. Every profile that existed before activation must still exist
    // with identical revisions and active revision.
    let profiles_preserved = before
        .profiles
        .iter()
        .all(|(name, profile)| after.profiles.get(name) == Some(profile));
    let teams_preserved = before.teams == after.teams;
    let history_preserved = before.tasks.keys().all(|id| after.tasks.contains_key(id));
    let identity_preserved = identity_equal(&plan.current_record, &record);
    let workspace_authorization_preserved = plan.current_record.workspace_root
        == record.workspace_root
        && plan.current_record.workspace_write == record.workspace_write;
    let credential_refs_preserved = plan.current_record.secret_refs == record.secret_refs;
    if !profiles_preserved {
        return Err(InstanceError::new(
            "upgrade.verify_failed",
            "candidate activation changed or removed persisted profile revisions",
        ));
    }
    if !teams_preserved {
        return Err(InstanceError::new(
            "upgrade.verify_failed",
            "candidate activation changed configured team assignments",
        ));
    }
    if !history_preserved {
        return Err(InstanceError::new(
            "upgrade.verify_failed",
            "candidate activation lost persisted task history",
        ));
    }
    if !identity_preserved || !workspace_authorization_preserved || !credential_refs_preserved {
        return Err(InstanceError::new(
            "upgrade.verify_failed",
            "candidate activation changed instance identity, workspace authorization, or credential references",
        ));
    }
    if active_profile != plan.current_state.active_profile {
        return Err(InstanceError::new(
            "upgrade.verify_failed",
            "candidate activation silently changed the active profile",
        ));
    }

    let verify_input = request
        .verify_input
        .clone()
        .unwrap_or_else(|| "prompt:upgrade-readiness".into());
    let agent = default_agent(&record.config_path)
        .or_else(|| plan.report.candidate.capabilities.first().cloned())
        .ok_or_else(|| {
            InstanceError::new(
                "upgrade.verify_failed",
                "no configured agent is available for capability verification",
            )
        })?;
    let (operation, task_id, task_state) = match run_capability_task(
        &base,
        &token,
        &agent,
        &verify_input,
        plan.report
            .candidate
            .capabilities
            .first()
            .map(String::as_str),
    ) {
        Ok((task_id, state)) => ("task".to_string(), Some(task_id), Some(state)),
        Err(error) if error.code == "upgrade.capability_denied" => {
            return Err(InstanceError::new(
                "upgrade.verify_failed",
                "instance token cannot execute the capability verification task",
            ))
        }
        Err(error) => return Err(error),
    };

    Ok(VerificationFinding {
        runtime_phase: phase,
        capability_operation: operation,
        task_id,
        task_state,
        identity_preserved,
        profiles_preserved,
        teams_preserved,
        history_preserved,
        workspace_authorization_preserved,
        credential_refs_preserved,
    })
}

fn run_capability_task(
    base: &str,
    token: &str,
    agent: &str,
    input: &str,
    _capability: Option<&str>,
) -> Result<(String, String), InstanceError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| InstanceError::new("upgrade.verify_failed", error.to_string()))?;
    let key = format!("upgrade-{}", uuid::Uuid::new_v4().simple());
    let response = client
        .post(format!("{base}/v1/tasks"))
        .bearer_auth(token)
        .json(&json!({
            "agent": agent,
            "session_id": key,
            "input": input,
            "idempotency_key": key,
        }))
        .send()
        .map_err(|error| {
            InstanceError::new(
                "upgrade.verify_failed",
                format!("capability task submission failed: {error}"),
            )
        })?;
    let status = response.status();
    let body: Value = response.json().unwrap_or(Value::Null);
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(InstanceError::new(
            "upgrade.capability_denied",
            format!(
                "capability verification was refused: {}",
                common::redact(&body.to_string())
            ),
        ));
    }
    if !(status == reqwest::StatusCode::OK || status == reqwest::StatusCode::CREATED) {
        return Err(InstanceError::new(
            "upgrade.verify_failed",
            format!(
                "capability task submission was refused with {status}: {}",
                common::redact(&body.to_string())
            ),
        ));
    }
    let task_id = body
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            InstanceError::new(
                "upgrade.verify_failed",
                "capability task submission returned no task id",
            )
        })?
        .to_string();
    let deadline = Instant::now() + TASK_TIMEOUT;
    loop {
        let task: Value = client
            .get(format!("{base}/v1/tasks/{task_id}"))
            .bearer_auth(token)
            .send()
            .and_then(|response| response.json())
            .map_err(|error| {
                InstanceError::new(
                    "upgrade.verify_failed",
                    format!("capability task could not be read: {error}"),
                )
            })?;
        let state = task
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match state.as_str() {
            "succeeded" => return Ok((task_id, state)),
            "failed" | "cancelled" | "unknown" => {
                return Err(InstanceError::new(
                    "upgrade.verify_failed",
                    format!("capability verification task ended in state '{state}'"),
                ))
            }
            _ if Instant::now() >= deadline => {
                return Err(InstanceError::new(
                    "upgrade.verify_failed",
                    format!("capability verification task did not finish; last state '{state}'"),
                ))
            }
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rollback_after_failure(
    request: &UpgradeRequest,
    paths: &crate::layout::InstancePaths,
    report: UpgradeReport,
    mut journal: UpgradeJournal,
    backup: Backup,
    previous_record: InstanceRecord,
    previous_state: InstanceState,
    was_running: bool,
    cause: InstanceError,
) -> Result<UpgradeOutcome, InstanceError> {
    let _ = process::stop(&request.root, &request.id);
    let mut binary_restored = false;
    let mut data_restored = false;
    let mut quarantine = None;
    let mut rollback_failure = None;

    match write_json(&paths.record, &previous_record, 0o600) {
        Ok(()) => binary_restored = true,
        Err(error) => rollback_failure = Some(error),
    }
    match restore_tree(&backup.directory.join("data"), &previous_record.data_dir) {
        Ok(()) => data_restored = true,
        Err(error) => {
            rollback_failure.get_or_insert(error);
            if let Ok(path) = quarantine_tree(paths, &previous_record.data_dir, &request.id) {
                quarantine = Some(path);
            }
        }
    }

    if binary_restored && data_restored {
        if was_running {
            if let Err(error) = process::start_with_env(
                &request.root,
                &request.id,
                previous_state.addr.as_deref(),
                &request.env,
            ) {
                let mut state = previous_state.clone();
                state.lifecycle = "faulted".into();
                state.pid = None;
                state.addr = None;
                state.last_error = Some(format!("{}: {}", error.code, error.message));
                save_state(paths, &state)?;
                journal.status = "rolled_back".into();
                journal.error = Some(format!(
                    "{}: {}; previous binary restored but restart failed: {}",
                    cause.code, cause.message, error.message
                ));
                journal.binary_restored = true;
                journal.data_restored = true;
                journal.updated_at = now_ms();
                write_journal(paths, &journal)?;
                return Err(InstanceError::new(
                    "upgrade.rollback_failed",
                    journal.error.clone().unwrap_or_default(),
                ));
            }
        } else {
            let mut state = previous_state.clone();
            state.lifecycle = "stopped".into();
            state.pid = None;
            state.addr = None;
            save_state(paths, &state)?;
        }
        journal.status = "rolled_back".into();
        journal.error = Some(format!("{}: {}", cause.code, cause.message));
        journal.binary_restored = true;
        journal.data_restored = true;
        journal.quarantine = None;
        journal.updated_at = now_ms();
        write_journal(paths, &journal)?;
        let mut report = report;
        report.work.action = "rolled_back".into();
        return Err(InstanceError::new(
            "upgrade.failed_rolled_back",
            format!(
                "{}: {}; previous binary restored ({}) and persisted data restored from snapshot ({}). Journal: {}",
                cause.code,
                cause.message,
                binary_restored,
                data_restored,
                paths.dir.join(JOURNAL_FILE).display()
            ),
        ));
    }

    journal.status = "quarantined".into();
    journal.error = Some(format!(
        "{}: {}; binary_restored={binary_restored} data_restored={data_restored}{}",
        cause.code,
        cause.message,
        rollback_failure
            .as_ref()
            .map(|error| format!("; rollback failure {}: {}", error.code, error.message))
            .unwrap_or_default()
    ));
    journal.binary_restored = binary_restored;
    journal.data_restored = data_restored;
    journal.quarantine = quarantine.clone();
    journal.updated_at = now_ms();
    write_journal(paths, &journal)?;
    let mut state = load_state(paths)?;
    state.lifecycle = "quarantined".into();
    state.pid = None;
    state.addr = None;
    state.last_error = journal.error.clone();
    save_state(paths, &state)?;
    Err(InstanceError::new(
        "upgrade.quarantined",
        format!(
            "{}: {}; rollback could not be proven, failed data is quarantined at {}",
            cause.code,
            cause.message,
            quarantine
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| previous_record.data_dir.display().to_string())
        ),
    ))
}

struct Backup {
    directory: PathBuf,
    digest: String,
}

fn backup_data(
    paths: &crate::layout::InstancePaths,
    record: &InstanceRecord,
    _state: &InstanceState,
) -> Result<Backup, InstanceError> {
    let directory = paths.dir.join("backups").join(format!(
        "upgrade-{}-{}",
        now_ms(),
        uuid::Uuid::new_v4().simple()
    ));
    let data = directory.join("data");
    std::fs::create_dir_all(&directory).map_err(storage)?;
    set_mode(&directory, 0o700)?;
    if record.data_dir.is_dir() {
        copy_tree(&record.data_dir, &data)?;
    } else {
        std::fs::create_dir_all(&data).map_err(storage)?;
    }
    let record_bytes = serde_json::to_vec_pretty(record)
        .map_err(|error| InstanceError::new("upgrade.storage_unavailable", error.to_string()))?;
    write_file(&directory.join("instance.json"), &record_bytes, 0o600)?;
    let digest = tree_digest(&data)?;
    write_file(
        &directory.join("digest"),
        format!("{digest}\n").as_bytes(),
        0o600,
    )?;
    Ok(Backup { directory, digest })
}

fn materialize_seeds(
    paths: &crate::layout::InstancePaths,
    record: &InstanceRecord,
    metadata: &ReleaseMetadata,
) -> Result<(), InstanceError> {
    let release = load_release(&record.release_dir)?;
    for seed in &release.seeds {
        validate_relative(&seed.path)?;
        let source = record.release_dir.join("seeds").join(&seed.path);
        let bytes = std::fs::read(&source).map_err(|_| {
            InstanceError::new(
                "upgrade.release_invalid",
                format!("seed '{}' is missing from the verified release", seed.path),
            )
        })?;
        if common::bytes_digest(&bytes) != seed.digest {
            return Err(InstanceError::new(
                "upgrade.release_invalid",
                format!("seed '{}' no longer matches its signed digest", seed.path),
            ));
        }
        let destination = paths.dir.join(&seed.path);
        if !destination.exists() {
            write_file(&destination, &bytes, 0o644)?;
        }
    }
    let _ = metadata;
    Ok(())
}

fn restore_binary_and_data(
    root: &Path,
    paths: &crate::layout::InstancePaths,
    record: &InstanceRecord,
    state: &InstanceState,
    backup: &Path,
    was_running: bool,
    environment: &[(String, String)],
) -> Result<(), InstanceError> {
    write_json(&paths.record, record, 0o600)?;
    restore_tree(backup, &record.data_dir)?;
    if was_running {
        process::start_with_env(root, &record.id, state.addr.as_deref(), environment)?;
    } else {
        let mut restored = state.clone();
        restored.pid = None;
        restored.addr = None;
        save_state(paths, &restored)?;
    }
    Ok(())
}

fn restore_tree(source: &Path, destination: &Path) -> Result<(), InstanceError> {
    if !source.is_dir() {
        return Err(InstanceError::new(
            "upgrade.no_data_backup",
            "upgrade data snapshot is missing or unreadable",
        ));
    }
    if destination.exists() {
        std::fs::remove_dir_all(destination).map_err(storage)?;
    }
    copy_tree(source, destination)
}

fn quarantine_tree(
    paths: &crate::layout::InstancePaths,
    data_dir: &Path,
    id: &str,
) -> Result<PathBuf, InstanceError> {
    let target = paths.dir.join("quarantine").join(format!(
        "{}-{}-{}",
        id,
        now_ms(),
        uuid::Uuid::new_v4().simple()
    ));
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(storage)?;
    }
    if data_dir.exists() {
        if std::fs::rename(data_dir, &target).is_err() {
            // External data directories can live on another filesystem where
            // an atomic rename is unavailable. Copy first, then remove only
            // after the quarantine bytes exist.
            copy_tree(data_dir, &target)?;
            std::fs::remove_dir_all(data_dir).map_err(storage)?;
        }
    } else {
        std::fs::create_dir_all(&target).map_err(storage)?;
    }
    Ok(target)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), InstanceError> {
    std::fs::create_dir_all(destination).map_err(storage)?;
    for entry in std::fs::read_dir(source).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        let kind = entry.file_type().map_err(storage)?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target).map_err(storage)?;
        }
    }
    Ok(())
}

fn tree_digest(root: &Path) -> Result<String, InstanceError> {
    let mut bytes = Vec::new();
    collect_tree(root, root, &mut bytes)?;
    Ok(common::bytes_digest(&bytes))
}

fn collect_tree(root: &Path, path: &Path, bytes: &mut Vec<u8>) -> Result<(), InstanceError> {
    let mut entries: Vec<_> = std::fs::read_dir(path)
        .map_err(storage)?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let kind = entry.file_type().map_err(storage)?;
        let child = entry.path();
        let relative = child.strip_prefix(root).unwrap_or(&child);
        bytes.extend(relative.to_string_lossy().as_bytes());
        bytes.push(0);
        if kind.is_dir() {
            collect_tree(root, &child, bytes)?;
        } else if kind.is_file() {
            bytes.extend(std::fs::read(&child).map_err(storage)?);
        }
        bytes.push(0);
    }
    Ok(())
}

fn identity_equal(left: &InstanceRecord, right: &InstanceRecord) -> bool {
    left.id == right.id
        && left.package == right.package
        && left.config_path == right.config_path
        && left.generated_config == right.generated_config
        && left.data_dir == right.data_dir
        && left.grants == right.grants
        && left.limits == right.limits
        && left.created_at == right.created_at
}

fn default_agent(config: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value
        .get("default_agent")
        .and_then(toml::Value::as_str)
        .map(str::to_string)
}

fn read_persisted(data_dir: &Path) -> PersistedState {
    std::fs::read(data_dir.join("state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn release_metadata(installed: &Installed) -> Result<ReleaseMetadata, InstanceError> {
    let bytes = std::fs::read(installed.directory.join("metadata.json")).map_err(|_| {
        InstanceError::new(
            "upgrade.release_invalid",
            "verified release has no readable signed metadata",
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        InstanceError::new(
            "upgrade.release_invalid",
            "verified release metadata is malformed",
        )
    })
}

/// Probe a verified executable's compiled registration in an isolated
/// environment. Restore uses the same evidence upgrade planning trusts.
pub(crate) fn probe_release(binary: &Path) -> Result<Probe, String> {
    run_probe(binary)
}

/// Exercise one native Launcher composition mode without starting a console or
/// server. Restore reports this separately from any Gateway access check.
pub(crate) fn launcher_probe(binary: &Path, mode: &str) -> Result<(), String> {
    let temp = temp_dir().map_err(|error| format!("{}: {}", error.code, error.message))?;
    let _guard = TempGuard::new(temp.clone());
    let envs = isolated_environment(&temp);
    let args: Vec<&str> = match mode {
        "dev" | "console" => vec!["dev", "--probe-only"],
        "serve" => vec!["serve", "--probe-only", "--addr", "127.0.0.1:0"],
        _ => return Err(format!("unsupported launcher mode '{mode}'")),
    };
    match run_binary(binary, &args, &temp, &envs) {
        Ok(output) if output.status == 0 => Ok(()),
        Ok(output) => Err(format!(
            "launcher mode '{mode}' exited with {}: {}",
            output.status,
            common::redact(output.stderr.trim())
        )),
        Err(message) => Err(format!("launcher mode '{mode}' could not run: {message}")),
    }
}

fn run_probe(binary: &Path) -> Result<Probe, String> {
    let temp = temp_dir().map_err(|error| format!("{}: {}", error.code, error.message))?;
    let _guard = TempGuard::new(temp.clone());
    let envs = isolated_environment(&temp);
    let output = run_binary(binary, &["probe", "--json"], &temp, &envs)?;
    if output.status != 0 {
        return Err(format!(
            "candidate capability probe exited with {}: {}",
            output.status,
            common::redact(output.stderr.trim())
        ));
    }
    serde_json::from_str(output.stdout.trim())
        .map_err(|_| "candidate capability probe did not emit machine-readable JSON".to_string())
}

struct CommandOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run_binary(
    binary: &Path,
    args: &[&str],
    cwd: &Path,
    envs: &[(String, String)],
) -> Result<CommandOutput, String> {
    let stdout_path = cwd.join(format!("stdout-{}", uuid::Uuid::new_v4().simple()));
    let stderr_path = cwd.join(format!("stderr-{}", uuid::Uuid::new_v4().simple()));
    let stdout = std::fs::File::create(&stdout_path).map_err(|error| error.to_string())?;
    let stderr = std::fs::File::create(&stderr_path).map_err(|error| error.to_string())?;
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("candidate binary could not start: {error}"))?;
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("candidate command exceeded its bounded timeout".into());
            }
            Err(error) => return Err(format!("candidate command could not be observed: {error}")),
        }
    };
    let stdout = std::fs::read_to_string(&stdout_path).unwrap_or_default();
    let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    let _ = std::fs::remove_file(stdout_path);
    let _ = std::fs::remove_file(stderr_path);
    Ok(CommandOutput {
        status: status.code().unwrap_or(1),
        stdout,
        stderr,
    })
}

fn isolated_environment(temp: &Path) -> Vec<(String, String)> {
    let home = temp.join("home");
    let _ = std::fs::create_dir_all(&home);
    let mut envs = vec![
        ("HOME".into(), home.display().to_string()),
        ("TMPDIR".into(), temp.display().to_string()),
        (
            "PATH".into(),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
        ),
        ("LANG".into(), "C".into()),
        ("MALLOC_ARENA_MAX".into(), "1".into()),
    ];
    if let Ok(value) = std::env::var("GOLDENBEE_GECKODRIVER") {
        envs.push(("GOLDENBEE_GECKODRIVER".into(), value));
    }
    envs
}

fn write_journal(
    paths: &crate::layout::InstancePaths,
    journal: &UpgradeJournal,
) -> Result<(), InstanceError> {
    write_json(&paths.dir.join(JOURNAL_FILE), journal, 0o600)
}

fn load_journal(
    paths: &crate::layout::InstancePaths,
) -> Result<Option<UpgradeJournal>, InstanceError> {
    let path = paths.dir.join(JOURNAL_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    read_json(&path).map(Some).map_err(|_| {
        InstanceError::new(
            "upgrade.journal_invalid",
            "upgrade journal is unreadable or malformed",
        )
    })
}

fn is_terminal_journal(status: &str) -> bool {
    matches!(
        status,
        "complete" | "rolled_back" | "recovered" | "quarantined"
    )
}

fn nonterminal_tasks(persisted: &PersistedState) -> Vec<String> {
    persisted
        .tasks
        .iter()
        .filter(|(_, task)| !is_terminal_task_state(&task.state))
        .map(|(id, _)| id.clone())
        .collect()
}

fn is_terminal_task_state(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "cancelled" | "unknown")
}

fn blocked_error(report: &UpgradeReport) -> InstanceError {
    let detail = report
        .blocked
        .iter()
        .map(|finding| format!("{}: {}", finding.code, finding.message))
        .collect::<Vec<_>>()
        .join("; ");
    InstanceError::new(
        "upgrade.incompatible",
        format!("candidate release cannot activate: {detail}"),
    )
}

fn temp_dir() -> Result<PathBuf, InstanceError> {
    let path = std::env::temp_dir().join(format!(
        "gb-instance-upgrade-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&path).map_err(storage)?;
    set_mode(&path, 0o700)?;
    Ok(path)
}

struct TempGuard {
    path: PathBuf,
}

impl TempGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn validate_relative(path: &str) -> Result<(), InstanceError> {
    let valid = !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !valid {
        return Err(InstanceError::new(
            "upgrade.release_invalid",
            format!("seed path '{path}' is not relative to the instance root"),
        ));
    }
    Ok(())
}

fn absolute(path: &Path) -> Result<PathBuf, InstanceError> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(storage)?.join(path)
    };
    Ok(std::fs::canonicalize(&path).unwrap_or(path))
}

fn storage(error: std::io::Error) -> InstanceError {
    InstanceError::new(
        "upgrade.storage_unavailable",
        format!("upgrade storage operation failed: {error}"),
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
