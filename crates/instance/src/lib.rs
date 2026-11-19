//! On-host Worker Instance provisioning and local lifecycle management.
//!
//! This crate owns host layout (`instance.json`, `state.json`, generated
//! configuration, durable data, workspace, token), atomic creation, process
//! start/stop, and inspection. It does not reimplement the Worker CLI: profile
//! commands are proxied to the installed binary's existing `profile`
//! subcommands, and start/stop only supervises that verified executable.
//!
//! Instances are local operator tooling. Production restart remains the host
//! supervisor's responsibility; this crate is not a daemon or a registry.

pub mod backup;
mod cli;
mod layout;
mod process;
mod trust;
pub mod upgrade;

pub use backup::{
    create as create_backup, migrate as migrate_backup, restore as restore_backup,
    verify as verify_backup, BackupOutcome, BackupRequest, BlockedTask, Check, MigrateRequest,
    MigrationJournal, MigrationOutcome, MigrationStep, RestoreReport, RestoreRequest, VerifyReport,
    MANIFEST_FILE, MIGRATION_JOURNAL,
};
pub use layout::{
    create, inspect, profile, CreateOutcome, CreateRequest, Inspection, InstanceError,
    InstanceLimits, InstanceRecord, InstanceState, ProfileOutcome, TrustStatus,
};
pub use process::{start, start_with_env, stop, StartOutcome, StopOutcome};
pub use trust::{apply_trust, ApplyTrustOutcome};
pub use upgrade::{
    apply as apply_upgrade, plan as plan_upgrade, recover as recover_upgrade, recover_with_env,
    AuthorizationFinding, ConfigFinding, Difference, Finding, LauncherFinding, ProfileFinding,
    ProtocolFinding, RecoveryOutcome, ReleaseView, SchemaFinding, SignatureFinding, TeamFinding,
    UpgradeJournal, UpgradeOutcome, UpgradeReport, UpgradeRequest, VerificationFinding,
    WorkFinding, WorkPolicy,
};

/// Run one `gb-instance` command. Returns the desired process exit code.
pub fn run(args: &[String]) -> Result<u8, InstanceError> {
    cli::run(args)
}
