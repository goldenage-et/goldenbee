//! Portable Worker Instance backup, restore validation, and migration drills.
//!
//! A backup archives one instance's host metadata, operator configuration,
//! external asset directories, and the complete durable data directory with
//! per-file digests. Restore is staged, validated against the verified package
//! release, and committed atomically. A missing package, configuration path,
//! external asset, secret reference, or checkpoint blocks readiness and
//! quarantines unfinished recovery instead of rebuilding or falling back to
//! bundled defaults.
//!
//! SQLite stores are copied with their `-wal` and `-shm` companions only while
//! the instance is stopped. This is a cold, consistency-checked copy, not an
//! online SQLite backup API; the operator must stop the instance or take an
//! equivalent filesystem snapshot. A native store that cannot satisfy this
//! contract is reported rather than described as consistent.

use crate::layout::{
    instance_paths, load_record, load_release, load_state, read_json, set_mode, write_file,
    write_json, InstanceError, InstancePaths, InstanceRecord, InstanceState,
};
use crate::upgrade;
use common::backup::{
    BackupArtifact, BackupExternalAsset, BackupFile, BackupManifest, BackupProfile, BackupRelease,
    BackupTask, WorkerBackup,
};
use common::ReleaseMetadata;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Manifest file written at the root of every backup.
pub const MANIFEST_FILE: &str = "manifest.json";
/// Durable journal for a backup-copy migration drill.
pub const MIGRATION_JOURNAL: &str = "migration.json";
const JOURNAL_SCHEMA: u32 = 1;
const STAGING_PREFIX: &str = ".backup-staging-";
const RESTORE_PREFIX: &str = ".restore-staging-";
const MIGRATION_PREFIX: &str = ".migration-staging-";

/// Create one backup request.
#[derive(Debug, Clone)]
pub struct BackupRequest {
    /// Instances root.
    pub root: PathBuf,
    /// Stable instance id.
    pub instance: String,
    /// New backup directory. It must not already exist.
    pub output: PathBuf,
}

/// Created backup.
#[derive(Debug, Clone)]
pub struct BackupOutcome {
    /// Written manifest.
    pub manifest: BackupManifest,
    /// Backup directory.
    pub path: PathBuf,
    /// Aggregate digest over the manifest file list.
    pub digest: String,
}

/// One verification check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    /// Stable check name.
    pub name: String,
    /// `ok`, `warning`, `blocked`, or `skipped`.
    pub status: String,
    /// Redacted explanation.
    pub detail: String,
}

impl Check {
    fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: "ok".into(),
            detail: detail.into(),
        }
    }
    fn blocked(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: "blocked".into(),
            detail: detail.into(),
        }
    }
    fn warning(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: "warning".into(),
            detail: detail.into(),
        }
    }
    fn skipped(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: "skipped".into(),
            detail: detail.into(),
        }
    }
}

/// Verification result for one backup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    /// Whether every required check passed.
    pub ready: bool,
    /// Parsed manifest.
    pub manifest: BackupManifest,
    /// Per-check evidence.
    pub checks: Vec<Check>,
}

/// Restore request. Paths are explicit so a clean host never inherits an
/// unreviewed source path.
#[derive(Debug, Clone)]
pub struct RestoreRequest {
    /// Backup directory.
    pub backup: PathBuf,
    /// Target instances root.
    pub root: PathBuf,
    /// Target instance id. Defaults to the backed-up identity.
    pub instance: Option<String>,
    /// Verified package release directory. When absent the installer cache next
    /// to `root` is searched for the recorded digest.
    pub release: Option<PathBuf>,
    /// Target operator configuration path.
    pub config: Option<PathBuf>,
    /// Target durable data directory.
    pub data: Option<PathBuf>,
    /// Target workspace root.
    pub workspace: Option<PathBuf>,
    /// Source absolute path to target absolute path mappings for external
    /// agent, prompt, skill, configuration, data, and workspace paths.
    pub path_map: BTreeMap<String, PathBuf>,
    /// External secret references the operator attests are provisioned.
    pub available_secrets: Vec<String>,
    /// Secret references revoked for this restore.
    pub revoked_secrets: Vec<String>,
    /// Exercise `dev --probe-only` and `serve --probe-only` for the restored
    /// release. Enabled by default.
    pub verify_launcher: bool,
    /// Gateway base URL used for the distinct Gateway access check. When the
    /// deployment enabled a Gateway, absence of this value blocks readiness.
    pub gateway_url: Option<String>,
    /// Bearer credential used only for the Gateway access check. Never logged.
    pub gateway_token: Option<String>,
}

impl RestoreRequest {
    /// Start a restore into one root.
    pub fn new(backup: impl Into<PathBuf>, root: impl Into<PathBuf>) -> Self {
        Self {
            backup: backup.into(),
            root: root.into(),
            instance: None,
            release: None,
            config: None,
            data: None,
            workspace: None,
            path_map: BTreeMap::new(),
            available_secrets: Vec::new(),
            revoked_secrets: Vec::new(),
            verify_launcher: true,
            gateway_url: None,
            gateway_token: None,
        }
    }
}

/// One unfinished task that could not be resumed safely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedTask {
    /// Task identity.
    pub task_id: String,
    /// Redacted reason list.
    pub reasons: Vec<String>,
}

/// Restore result. A blocked preflight leaves the target root untouched; a
/// post-commit verification failure leaves a quarantined instance that will not
/// start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreReport {
    /// `ready`, `blocked`, or `quarantined`.
    pub status: String,
    /// Whether the instance may be started.
    pub ready: bool,
    /// Restored instance identity.
    pub instance: String,
    /// Target instance directory.
    pub instance_dir: PathBuf,
    /// Required checks.
    pub checks: Vec<Check>,
    /// Unfinished tasks that are quarantined instead of resumed.
    pub blocked_tasks: Vec<BlockedTask>,
    /// Distinct native Launcher verification result.
    pub launcher: Check,
    /// Distinct Gateway access verification result.
    pub gateway: Check,
}

/// One migration step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStep {
    /// Step name.
    pub name: String,
    /// `complete`, `blocked`, `interrupted`, or `irreversible`.
    pub status: String,
    /// Redacted detail.
    pub detail: String,
}

/// Durable migration journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationJournal {
    /// Journal schema.
    pub schema: u32,
    /// Source backup.
    pub source: PathBuf,
    /// Target copy.
    pub target: PathBuf,
    /// `running`, `complete`, `blocked`, or `irreversible`.
    pub status: String,
    /// Completed step names, in order.
    pub completed: Vec<String>,
    /// Last update time.
    pub updated_at: i64,
    /// Redacted failure.
    #[serde(default)]
    pub error: Option<String>,
    /// Steps observed by the latest run.
    pub steps: Vec<MigrationStep>,
}

/// Migration result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationOutcome {
    /// Target copy.
    pub path: PathBuf,
    /// Whether an existing journal was resumed.
    pub resumed: bool,
    /// Final journal.
    pub journal: MigrationJournal,
    /// Verification of the migrated copy.
    pub verification: VerifyReport,
    /// Whether an irreversible change would be required. The source copy is
    /// never mutated in that case.
    pub irreversible: bool,
}

/// Migration request.
#[derive(Debug, Clone)]
pub struct MigrateRequest {
    /// Source backup directory. It is never modified.
    pub source: PathBuf,
    /// New target copy.
    pub target: PathBuf,
    /// Highest Worker data schema this tool understands. A source above this
    /// value is reported as irreversible instead of being rewritten.
    pub supported_data_schema: u32,
}

impl MigrateRequest {
    /// Start a migration drill using the current manifest schema.
    pub fn new(source: impl Into<PathBuf>, target: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            supported_data_schema: 4,
        }
    }
}

/// Create one portable backup for a stopped instance.
pub fn create(request: &BackupRequest) -> Result<BackupOutcome, InstanceError> {
    let paths = instance_paths(&request.root, &request.instance)?;
    let record = load_record(&paths)?;
    let state = load_state(&paths)?;
    if state.pid.is_some() || matches!(state.lifecycle.as_str(), "running" | "upgrading") {
        return Err(InstanceError::new(
            "backup.instance_running",
            "stop the instance before taking a cold backup; a live data directory is not a consistent snapshot",
        ));
    }
    let output = absolute(&request.output)?;
    if path_within(&output, &paths.dir) || path_within(&output, &record.data_dir) {
        return Err(InstanceError::new(
            "backup.path_invalid",
            "backup output must not be inside the instance or its durable data directory",
        ));
    }
    let installed = load_release(&record.release_dir)?;
    let metadata = release_metadata(&installed)?;
    if installed.artifact_digest != record.artifact_digest {
        return Err(InstanceError::new(
            "backup.release_mismatch",
            "the instance record and verified cache disagree on the release digest",
        ));
    }
    if !record.config_path.is_file() {
        return Err(InstanceError::new(
            "backup.config_missing",
            "the operator configuration file is missing",
        ));
    }

    let parent = request
        .output
        .parent()
        .ok_or_else(|| InstanceError::new("backup.path_invalid", "backup output has no parent"))?;
    std::fs::create_dir_all(parent).map_err(storage)?;
    if request.output.exists() {
        return Err(InstanceError::new(
            "backup.exists",
            "backup output already exists; choose a new directory",
        ));
    }
    let stage = parent.join(format!(
        "{STAGING_PREFIX}{}-{}",
        request.instance,
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&stage).map_err(storage)?;
    set_mode(&stage, 0o700)?;
    let guard = StageGuard::new(stage.clone());
    let mut files = Vec::new();

    copy_instance_tree(&paths, &record, &stage, &mut files)?;

    let config_archive = if path_within(&record.config_path, &paths.dir) {
        format!(
            "instance/{}",
            relative(&record.config_path, &paths.dir)?.display()
        )
    } else {
        let archived = format!(
            "config/{}",
            record
                .config_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("worker.toml")
        );
        copy_one(
            &record.config_path,
            &stage.join(&archived),
            "config",
            &mut files,
        )?;
        archived
    };

    let data_archive = if path_within(&record.data_dir, &paths.dir) {
        format!(
            "instance/{}",
            relative(&record.data_dir, &paths.dir)?.display()
        )
    } else {
        let archived = "data".to_string();
        copy_tree(
            &record.data_dir,
            &stage.join(&archived),
            "native-data",
            &mut files,
            &stage,
            &[],
        )?;
        archived
    };

    let config = read_toml(&record.config_path)?;
    let mut external_assets = Vec::new();
    for (kind, value) in [
        ("agents", config_path_value(&config, "agents")),
        ("prompts", config_path_value(&config, "prompts")),
        ("skills", config_path_value(&config, "skills")),
    ] {
        let Some(value) = value else { continue };
        let source = resolve_config_path(&record.config_path, &value);
        if !source.is_dir() {
            return Err(InstanceError::new(
                "backup.asset_missing",
                format!(
                    "external {kind} directory '{}' is missing",
                    source.display()
                ),
            ));
        }
        let within = path_within(&source, &paths.dir);
        let archived = if within {
            format!("instance/{}", relative(&source, &paths.dir)?.display())
        } else {
            format!("external/{kind}")
        };
        if !within {
            copy_tree(
                &source,
                &stage.join(&archived),
                "external-asset",
                &mut files,
                &stage,
                &[],
            )?;
        }
        let digest = tree_digest(&stage.join(&archived)).unwrap_or_default();
        let archive_files = files
            .iter()
            .filter(|file| file.path == archived || file.path.starts_with(&format!("{archived}/")))
            .map(|file| file.path.clone())
            .collect();
        external_assets.push(BackupExternalAsset {
            kind: kind.into(),
            original_path: source.display().to_string(),
            archived_path: Some(archived),
            files: archive_files,
            digest,
        });
    }

    let persisted = read_state_json(&record.data_dir)?;
    let data_schema = persisted
        .as_ref()
        .and_then(|value| value.get("schema"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let worker = WorkerBackup {
        instance: record.id.clone(),
        source_instance: paths.dir.display().to_string(),
        release: BackupRelease {
            package: record.package.clone(),
            version: record.version.clone(),
            artifact_digest: record.artifact_digest.clone(),
            binary_digest: record.binary_digest.clone(),
            signer: record.signer.clone(),
            target: metadata.target.clone(),
            runtime: metadata.package.runtime.clone(),
            adk: metadata.package.adk.clone(),
            protocol_min: metadata.protocol_range().0.to_string(),
            protocol_max: metadata.protocol_range().1.to_string(),
            source_revision: metadata.source_revision.clone(),
            hooks: metadata.package.hooks.clone(),
            launcher_modes: installed.launcher_modes.clone(),
        },
        data_schema,
        config_path: record.config_path.display().to_string(),
        config_archived: config_archive,
        data_dir: record.data_dir.display().to_string(),
        data_archived: data_archive,
        workspace_root: record.workspace_root.display().to_string(),
        workspace_write: record.workspace_write,
        grants: record.grants.clone(),
        limits: common::Limits {
            max_fanout: record.limits.max_active as u32,
            max_depth: record.limits.per_principal as u32,
            max_tokens: 0,
            max_model_calls: record.limits.max_queue as u32,
            ..common::Limits::default()
        },
        secret_refs: record.secret_refs.clone(),
        seed_assets: record.seed_assets.clone(),
        profiles: persisted
            .as_ref()
            .map(profiles_from_state)
            .unwrap_or_default(),
        active_profile: state.active_profile.clone(),
        tasks: persisted
            .as_ref()
            .map(|value| tasks_from_state(value, &record.artifact_digest))
            .unwrap_or_default(),
        artifacts: persisted
            .as_ref()
            .map(artifacts_from_state)
            .unwrap_or_default(),
        external_assets,
        launcher_modes: installed.launcher_modes.clone(),
        gateway_enabled: config.get("gateway").is_some(),
    };

    let mut manifest = BackupManifest::new(
        format!("worker-{}-{}", record.id, uuid::Uuid::new_v4().simple()),
        host_name(),
    );
    manifest.worker = Some(worker);
    manifest.files = files;
    manifest
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    let digest = manifest_digest(&manifest)?;
    write_json(&stage.join(MANIFEST_FILE), &manifest, 0o600)?;
    sync_tree(&stage)?;
    std::fs::rename(&stage, &request.output).map_err(storage)?;
    sync_dir(parent)?;
    drop(guard);
    Ok(BackupOutcome {
        manifest,
        path: request.output.clone(),
        digest,
    })
}

/// Verify a backup without applying it.
pub fn verify(backup: &Path) -> Result<VerifyReport, InstanceError> {
    let manifest = read_manifest(backup)?;
    let mut checks = Vec::new();
    if !manifest.schema_supported() {
        checks.push(Check::blocked(
            "schema",
            format!(
                "manifest schema {} is newer than supported {}",
                manifest.schema,
                common::BACKUP_SCHEMA
            ),
        ));
        return Ok(VerifyReport {
            ready: false,
            manifest,
            checks,
        });
    }
    checks.push(Check::ok(
        "schema",
        format!(
            "manifest schema {} and format are supported",
            manifest.schema
        ),
    ));
    checks.push(Check::ok(
        "owner",
        if manifest.worker.is_some() && manifest.gateway.is_some() {
            "manifest carries separate Worker and Gateway owner sections"
        } else if manifest.worker.is_some() {
            "manifest carries a Worker owner section"
        } else if manifest.gateway.is_some() {
            "manifest carries a Gateway owner section"
        } else {
            "manifest carries no owner state"
        },
    ));
    if manifest.worker.is_none() && manifest.gateway.is_none() {
        checks.push(Check::blocked(
            "owner",
            "manifest contains neither Worker nor Gateway state",
        ));
    }

    let mut seen = BTreeSet::new();
    let mut content_ok = true;
    for file in &manifest.files {
        if !safe_relative(&file.path) {
            content_ok = false;
            checks.push(Check::blocked(
                "content",
                format!("archived path '{}' is not a safe relative path", file.path),
            ));
            continue;
        }
        if !seen.insert(file.path.as_str()) {
            content_ok = false;
            checks.push(Check::blocked(
                "content",
                format!("archived path '{}' appears twice", file.path),
            ));
            continue;
        }
        let path = backup.join(&file.path);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                content_ok = false;
                checks.push(Check::blocked(
                    "content",
                    format!("archived file '{}' is missing", file.path),
                ));
                continue;
            }
        };
        if bytes.len() as u64 != file.size {
            content_ok = false;
            checks.push(Check::blocked(
                "content",
                format!("archived file '{}' has an unexpected size", file.path),
            ));
            continue;
        }
        if common::bytes_digest(&bytes) != file.digest {
            content_ok = false;
            checks.push(Check::blocked(
                "content",
                format!("archived file '{}' failed its digest check", file.path),
            ));
        }
    }
    if content_ok {
        checks.push(Check::ok(
            "content",
            format!(
                "{} archived files match their digests",
                manifest.files.len()
            ),
        ));
    }

    if let Some(worker) = &manifest.worker {
        validate_worker_manifest(worker, &manifest, &mut checks);
    }
    if let Some(gateway) = &manifest.gateway {
        validate_gateway_manifest(gateway, &manifest, &mut checks);
    }
    let ready = checks.iter().all(|check| check.status != "blocked");
    Ok(VerifyReport {
        ready,
        manifest,
        checks,
    })
}

/// Restore one Worker backup onto a clean host after all required references
/// validate.
pub fn restore(request: &RestoreRequest) -> Result<RestoreReport, InstanceError> {
    let verification = verify(&request.backup)?;
    if !verification.ready {
        return Ok(RestoreReport {
            status: "blocked".into(),
            ready: false,
            instance: verification
                .manifest
                .worker
                .as_ref()
                .map(|worker| worker.instance.clone())
                .unwrap_or_default(),
            instance_dir: request.root.clone(),
            checks: verification.checks,
            blocked_tasks: Vec::new(),
            launcher: Check::skipped("launcher", "backup did not validate"),
            gateway: Check::skipped("gateway", "backup did not validate"),
        });
    }
    let manifest = verification.manifest;
    let worker = manifest.worker.as_ref().ok_or_else(|| {
        InstanceError::new(
            "restore.not_worker_backup",
            "this backup has no Worker owner section; use the Gateway adapter",
        )
    })?;
    let id = request
        .instance
        .clone()
        .unwrap_or_else(|| worker.instance.clone());
    crate::layout::validate_id(&id)?;
    let root = absolute(&request.root)?;
    std::fs::create_dir_all(&root).map_err(storage)?;
    set_mode(&root, 0o700)?;
    let target_dir = root.join(&id);
    if target_dir.exists() {
        return Err(InstanceError::new(
            "restore.exists",
            format!(
                "instance '{}' already exists; restore requires a clean instance directory",
                id
            ),
        ));
    }

    let mut checks = verification.checks;
    let release_dir = resolve_release(&root, request.release.as_deref(), worker)?;
    let installed = load_release(&release_dir)?;
    let metadata = release_metadata(&installed)?;
    checks.push(package_check(worker, &installed, &metadata)?);
    let probe = upgrade::probe_release(&installed.binary).map_err(|message| {
        InstanceError::new(
            "restore.release_probe_failed",
            format!("verified package probe failed: {message}"),
        )
    })?;
    checks.push(Check::ok(
        "release-probe",
        format!(
            "verified package reports data schema {} and launcher modes {}",
            probe.data_schema,
            probe.launcher_modes.join(",")
        ),
    ));
    if worker.data_schema > probe.data_schema {
        checks.push(Check::blocked(
            "schema",
            format!(
                "backup data schema {} is newer than the release's supported {}; an older binary must not mutate it",
                worker.data_schema, probe.data_schema
            ),
        ));
    } else {
        checks.push(Check::ok(
            "schema",
            format!(
                "backup data schema {} is supported by the release ({})",
                worker.data_schema, probe.data_schema
            ),
        ));
    }
    checks.extend(secret_checks(
        &worker.secret_refs,
        &request.available_secrets,
        &request.revoked_secrets,
    ));
    checks.extend(configuration_checks(&request.backup, worker)?);

    let target_instance = root.join(&id);
    let default_config = archived_target(
        &target_instance,
        &worker.config_archived,
        &worker.config_path,
    );
    let target_config = resolve_target_path(
        request.config.as_deref(),
        &request.path_map,
        &worker.config_path,
        &default_config,
    );
    let default_data = archived_target(&target_instance, &worker.data_archived, &worker.data_dir);
    let target_data = resolve_target_path(
        request.data.as_deref(),
        &request.path_map,
        &worker.data_dir,
        &default_data,
    );
    let default_workspace = if Path::new(&worker.workspace_root)
        .strip_prefix(&worker.source_instance)
        .is_ok()
    {
        target_instance.join(relative_plain(
            Path::new(&worker.workspace_root),
            Path::new(&worker.source_instance),
        ))
    } else {
        PathBuf::from(&worker.workspace_root)
    };
    let target_workspace = resolve_target_path(
        request.workspace.as_deref(),
        &request.path_map,
        &worker.workspace_root,
        &default_workspace,
    );
    checks.extend(path_checks(
        &request.backup,
        worker,
        &request.path_map,
        &root,
        &id,
    )?);
    if !worker.config_archived.starts_with("instance/")
        && request.config.is_none()
        && !request.path_map.contains_key(&worker.config_path)
    {
        checks.push(Check::blocked(
            "external-config",
            format!(
                "operator configuration '{}' was stored outside the instance; supply --config or an explicit path mapping",
                worker.config_path
            ),
        ));
    }
    if !worker.data_archived.starts_with("instance/")
        && request.data.is_none()
        && !request.path_map.contains_key(&worker.data_dir)
    {
        checks.push(Check::blocked(
            "external-data",
            format!(
                "durable data '{}' was stored outside the instance; supply --data or an explicit path mapping",
                worker.data_dir
            ),
        ));
    }
    if checks.iter().any(|check| check.status == "blocked") {
        return Ok(RestoreReport {
            status: "blocked".into(),
            ready: false,
            instance: id,
            instance_dir: target_dir,
            checks,
            blocked_tasks: Vec::new(),
            launcher: Check::skipped("launcher", "restore preflight blocked"),
            gateway: Check::skipped("gateway", "restore preflight blocked"),
        });
    }

    let stage = root.join(format!(
        "{RESTORE_PREFIX}{id}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&stage).map_err(storage)?;
    set_mode(&stage, 0o700)?;
    let guard = StageGuard::new(stage.clone());

    let staged_config = stage_for(&target_config, &target_dir, &stage);
    let staged_data = stage_for(&target_data, &target_dir, &stage);
    let staged_workspace = stage_for(&target_workspace, &target_dir, &stage);
    materialize_instance(
        &request.backup,
        worker,
        &stage,
        &id,
        &installed,
        &staged_config,
        &staged_data,
        &staged_workspace,
        &target_config,
        &target_data,
        &target_workspace,
        &request.path_map,
        &target_dir,
    )?;

    let persisted = read_state_json(&staged_data)?;
    let recovery = recovery_checks(
        &request.backup,
        worker,
        persisted.as_ref(),
        &installed,
        &staged_data,
    );
    let blocked_tasks = recovery
        .iter()
        .filter(|check| check.status == "blocked")
        .map(|check| BlockedTask {
            task_id: check.name.trim_start_matches("recovery:").to_string(),
            reasons: vec![check.detail.clone()],
        })
        .collect::<Vec<_>>();
    let quarantine = !blocked_tasks.is_empty();
    finalize_state(&stage, quarantine, &blocked_tasks)?;
    checks.extend(recovery);

    let launcher = if request.verify_launcher {
        launcher_check(&installed, &worker.launcher_modes)
    } else {
        Check::skipped("launcher", "Launcher verification was explicitly disabled")
    };
    let gateway = gateway_check(worker, request)?;
    checks.push(launcher.clone());
    checks.push(gateway.clone());

    let mut quarantine = quarantine;
    if launcher.status == "blocked" || gateway.status == "blocked" {
        quarantine = true;
        finalize_state(&stage, true, &blocked_tasks)?;
    }
    sync_tree(&stage)?;
    std::fs::rename(&stage, &target_dir).map_err(storage)?;
    sync_dir(&root)?;
    drop(guard);

    let ready = !quarantine && checks.iter().all(|check| check.status != "blocked");
    Ok(RestoreReport {
        status: if ready {
            "ready".into()
        } else if quarantine {
            "quarantined".into()
        } else {
            "blocked".into()
        },
        ready,
        instance: id,
        instance_dir: target_dir,
        checks,
        blocked_tasks,
        launcher,
        gateway,
    })
}

/// Migrate a backup copy. The source is never modified. A durable journal lets
/// an interrupted run resume without repeating completed steps; a newer source
/// schema is reported as irreversible instead of being rewritten.
pub fn migrate(request: &MigrateRequest) -> Result<MigrationOutcome, InstanceError> {
    let source = absolute(&request.source)?;
    let target = absolute(&request.target)?;
    if target.starts_with(&source) || source.starts_with(&target) {
        return Err(InstanceError::new(
            "migration.path_invalid",
            "migration target and source must not contain each other",
        ));
    }
    let source_manifest = read_manifest(&request.source)?;
    if source_manifest.schema > common::BACKUP_SCHEMA || !source_manifest.schema_supported() {
        return Err(InstanceError::new(
            "migration.schema_unsupported",
            "backup manifest schema is newer than this tool understands; the source was not copied or rewritten",
        ));
    }
    let resumed = request.target.join(MIGRATION_JOURNAL).is_file();
    let mut journal = if resumed {
        read_json::<MigrationJournal>(&request.target.join(MIGRATION_JOURNAL)).map_err(|_| {
            InstanceError::new(
                "migration.journal_invalid",
                "migration journal is unreadable or malformed",
            )
        })?
    } else {
        if request.target.exists() {
            return Err(InstanceError::new(
                "migration.exists",
                "migration target already exists without a resumable journal",
            ));
        }
        let parent = request.target.parent().ok_or_else(|| {
            InstanceError::new("migration.path_invalid", "migration target has no parent")
        })?;
        std::fs::create_dir_all(parent).map_err(storage)?;
        let stage = parent.join(format!(
            "{MIGRATION_PREFIX}{}-{}",
            request
                .target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("copy"),
            uuid::Uuid::new_v4().simple()
        ));
        copy_dir(&request.source, &stage)?;
        write_json(
            &stage.join(MIGRATION_JOURNAL),
            &MigrationJournal {
                schema: JOURNAL_SCHEMA,
                source: request.source.clone(),
                target: request.target.clone(),
                status: "running".into(),
                completed: Vec::new(),
                updated_at: now_ms(),
                error: None,
                steps: Vec::new(),
            },
            0o600,
        )?;
        sync_tree(&stage)?;
        std::fs::rename(&stage, &request.target).map_err(storage)?;
        sync_dir(parent)?;
        read_json(&request.target.join(MIGRATION_JOURNAL)).map_err(|_| {
            InstanceError::new(
                "migration.journal_invalid",
                "migration journal could not be reopened",
            )
        })?
    };
    if journal.schema > JOURNAL_SCHEMA {
        journal.status = "blocked".into();
        journal.error = Some("migration journal is newer than supported".into());
        write_journal(&request.target, &journal)?;
        return Err(InstanceError::new(
            "migration.journal_unsupported",
            "migration journal was written by a newer tool",
        ));
    }
    let source_verification = verify(&request.source)?;
    if !source_verification.ready {
        journal.status = "blocked".into();
        journal.error = Some("source backup did not verify".into());
        journal.steps = steps_from_checks(&source_verification.checks);
        write_journal(&request.target, &journal)?;
        return Err(InstanceError::new(
            "migration.source_invalid",
            "source backup failed content verification",
        ));
    }

    let data_schema = journal_data_schema(&request.target)?;
    if data_schema > request.supported_data_schema {
        journal.status = "irreversible".into();
        journal.error = Some(format!(
            "backup data schema {data_schema} is newer than supported {}; the copy was not rewritten",
            request.supported_data_schema
        ));
        journal.updated_at = now_ms();
        write_journal(&request.target, &journal)?;
        return Err(InstanceError::new(
            "migration.irreversible",
            "the backup contains newer incompatible state; it was left unmodified",
        ));
    }

    let mut manifest = read_manifest(&request.target)?;
    if manifest.schema > common::BACKUP_SCHEMA {
        journal.status = "irreversible".into();
        journal.error = Some("backup manifest schema is newer than supported".into());
        journal.updated_at = now_ms();
        write_journal(&request.target, &journal)?;
        return Err(InstanceError::new(
            "migration.schema_unsupported",
            "backup manifest schema is newer than this tool understands",
        ));
    }
    let steps = [
        "verify-source",
        "copy-present",
        "schema-compatible",
        "normalize-manifest",
        "verify-copy",
    ];
    let mut steps_done = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let name = (*step).to_string();
        if journal.completed.contains(&name) {
            steps_done.push(MigrationStep {
                name,
                status: "complete".into(),
                detail: "already complete; not repeated".into(),
            });
            continue;
        }
        let detail = match *step {
            "verify-source" => "source backup verified".to_string(),
            "copy-present" => "target is a complete copy of the source".to_string(),
            "schema-compatible" => {
                format!("data schema {data_schema} is not newer than supported")
            }
            "normalize-manifest" => {
                if manifest.schema < common::BACKUP_SCHEMA {
                    manifest.schema = common::BACKUP_SCHEMA;
                    write_json(&request.target.join(MANIFEST_FILE), &manifest, 0o600)?;
                    format!("manifest schema normalized to {}", manifest.schema)
                } else {
                    "manifest schema already current".into()
                }
            }
            "verify-copy" => {
                let report = verify(&request.target)?;
                if !report.ready {
                    journal.status = "blocked".into();
                    journal.error = Some("migrated copy failed verification".into());
                    journal.steps = steps_from_checks(&report.checks);
                    journal.updated_at = now_ms();
                    write_journal(&request.target, &journal)?;
                    return Err(InstanceError::new(
                        "migration.copy_invalid",
                        "migrated copy failed verification",
                    ));
                }
                "migrated copy verified".to_string()
            }
            _ => unreachable!(),
        };
        journal.completed.push(name.clone());
        journal.updated_at = now_ms();
        steps_done.push(MigrationStep {
            name,
            status: "complete".into(),
            detail,
        });
        journal.steps = steps_done.clone();
        write_journal(&request.target, &journal)?;
        if interruption_after(index + 1) {
            journal.status = "interrupted".into();
            journal.error = Some("test interruption after a durable step".into());
            write_journal(&request.target, &journal)?;
            return Err(InstanceError::new(
                "migration.interrupted",
                "migration was interrupted after a durable step; rerun to resume",
            ));
        }
    }
    let verification = verify(&request.target)?;
    journal.status = if verification.ready {
        "complete".into()
    } else {
        "blocked".into()
    };
    journal.error = None;
    journal.updated_at = now_ms();
    journal.steps = steps_done;
    write_journal(&request.target, &journal)?;
    Ok(MigrationOutcome {
        path: request.target.clone(),
        resumed,
        journal,
        verification,
        irreversible: false,
    })
}

fn validate_worker_manifest(
    worker: &WorkerBackup,
    manifest: &BackupManifest,
    checks: &mut Vec<Check>,
) {
    if worker.instance.is_empty() {
        checks.push(Check::blocked(
            "identity",
            "Worker instance identity is empty",
        ));
    } else if worker.release.artifact_digest.is_empty()
        || worker.release.binary_digest.is_empty()
        || worker.release.package.is_empty()
    {
        checks.push(Check::blocked(
            "identity",
            "Worker package identity or digests are missing",
        ));
    } else {
        checks.push(Check::ok(
            "identity",
            format!(
                "Worker instance '{}' is bound to {} {} ({})",
                worker.instance,
                worker.release.package,
                worker.release.version,
                worker.release.artifact_digest
            ),
        ));
    }
    if has_file(manifest, &worker.config_archived) {
        checks.push(Check::ok(
            "configuration",
            format!(
                "operator configuration '{}' is archived",
                worker.config_archived
            ),
        ));
    } else {
        checks.push(Check::blocked(
            "configuration",
            "the archived operator configuration is missing",
        ));
    }
    if worker.data_schema == 0 {
        checks.push(Check::ok(
            "native-data",
            "the instance has no durable state yet; restore starts from an empty data directory",
        ));
    } else if has_file(manifest, &worker.data_archived)
        || worker.data_archived.is_empty()
        || manifest
            .files
            .iter()
            .any(|file| file.path.starts_with(&format!("{}/", worker.data_archived)))
    {
        checks.push(Check::ok(
            "native-data",
            format!("durable data '{}' is archived", worker.data_archived),
        ));
    } else {
        checks.push(Check::blocked(
            "native-data",
            "the archived durable data directory is empty or missing",
        ));
    }
    let mut profiles = BTreeSet::new();
    for profile in &worker.profiles {
        for revision in &profile.revisions {
            profiles.insert((profile.name.clone(), *revision));
        }
    }
    let mut unresolved = Vec::new();
    for task in &worker.tasks {
        if !is_terminal_state(&task.state) {
            if !task.profile.is_empty()
                && !profiles.contains(&(task.profile.clone(), task.profile_revision))
            {
                unresolved.push(format!("{} profile revision", task.task_id));
            }
            if !task.package_digest.is_empty()
                && task.package_digest != worker.release.artifact_digest
            {
                unresolved.push(format!("{} package identity", task.task_id));
            }
            if !task.scopes.is_empty() {
                let missing = task
                    .scopes
                    .iter()
                    .filter(|scope| !worker.grants.contains(scope))
                    .cloned()
                    .collect::<Vec<_>>();
                if !missing.is_empty() {
                    unresolved.push(format!("{} authority", task.task_id));
                }
            }
        }
    }
    if unresolved.is_empty() {
        checks.push(Check::ok(
            "task-bindings",
            format!(
                "{} task binding(s) and {} profile(s) are internally consistent",
                worker.tasks.len(),
                worker.profiles.len()
            ),
        ));
    } else {
        checks.push(Check::blocked(
            "task-bindings",
            format!(
                "unfinished work references missing definitions or authority: {}",
                unresolved.join(", ")
            ),
        ));
    }
    let mut external_ok = true;
    for asset in &worker.external_assets {
        if asset.files.iter().any(|path| !has_file(manifest, path)) {
            external_ok = false;
            checks.push(Check::blocked(
                "external-assets",
                format!("external {} asset files are incomplete", asset.kind),
            ));
        }
    }
    if external_ok {
        checks.push(Check::ok(
            "external-assets",
            format!(
                "{} external asset directory/directories are archived",
                worker.external_assets.len()
            ),
        ));
    }
    for reference in &worker.secret_refs {
        if looks_like_secret_value(reference) {
            checks.push(Check::blocked(
                "secret-references",
                "manifest contains a credential value instead of a reference",
            ));
        }
    }
}

fn validate_gateway_manifest(
    gateway: &common::GatewayBackup,
    manifest: &BackupManifest,
    checks: &mut Vec<Check>,
) {
    if gateway.gateway_id.is_empty() {
        checks.push(Check::blocked(
            "gateway-identity",
            "Gateway projection identity is empty",
        ));
    } else {
        checks.push(Check::ok(
            "gateway-identity",
            format!("Gateway projection '{}' is recorded", gateway.gateway_id),
        ));
    }
    if gateway.dispatch_schema > common::BACKUP_SCHEMA + 64
        || gateway.registry_schema > common::BACKUP_SCHEMA + 64
    {
        checks.push(Check::blocked(
            "gateway-schema",
            "Gateway store schema is implausible for this build",
        ));
    } else {
        checks.push(Check::ok(
            "gateway-schema",
            format!(
                "Gateway stores use dispatch schema {} and registry schema {}",
                gateway.dispatch_schema, gateway.registry_schema
            ),
        ));
    }
    let workers: BTreeSet<&str> = gateway.workers.iter().map(String::as_str).collect();
    let mut missing = Vec::new();
    for execution in &gateway.executions {
        if !workers.contains(execution.worker.as_str()) {
            missing.push(execution.execution_id.clone());
        }
    }
    if missing.is_empty() {
        checks.push(Check::ok(
            "gateway-mappings",
            format!(
                "{} execution mapping(s) reference approved Worker identities",
                gateway.executions.len()
            ),
        ));
    } else {
        checks.push(Check::blocked(
            "gateway-mappings",
            format!(
                "execution mappings reference Workers absent from the restored registry: {}",
                missing.join(",")
            ),
        ));
    }
    if manifest
        .files
        .iter()
        .any(|file| file.role == "gateway-store")
    {
        checks.push(Check::ok(
            "gateway-content",
            "Gateway store files are archived separately from Worker state",
        ));
    } else {
        checks.push(Check::blocked(
            "gateway-content",
            "Gateway store files are missing from the manifest",
        ));
    }
}

fn package_check(
    worker: &WorkerBackup,
    installed: &install::Installed,
    metadata: &ReleaseMetadata,
) -> Result<Check, InstanceError> {
    if installed.artifact_digest != worker.release.artifact_digest {
        return Ok(Check::blocked(
            "package",
            format!(
                "restored release digest {} does not match the backup's {}",
                installed.artifact_digest, worker.release.artifact_digest
            ),
        ));
    }
    if installed.package != worker.release.package || installed.version != worker.release.version {
        return Ok(Check::blocked(
            "package",
            "restored release package or version differs from the backup",
        ));
    }
    if common::bytes_digest(&std::fs::read(&installed.binary).map_err(storage)?)
        != worker.release.binary_digest
    {
        return Ok(Check::blocked(
            "package",
            "restored executable digest differs from the backup",
        ));
    }
    let expected_hooks: BTreeSet<&str> =
        metadata.package.hooks.iter().map(String::as_str).collect();
    let missing: Vec<&str> = worker
        .release
        .hooks
        .iter()
        .map(String::as_str)
        .filter(|hook| !expected_hooks.contains(hook))
        .collect();
    if !missing.is_empty() {
        return Ok(Check::blocked(
            "package",
            format!(
                "the restored package cannot provide required hooks: {}",
                missing.join(",")
            ),
        ));
    }
    if installed.launcher_modes != worker.release.launcher_modes {
        return Ok(Check::blocked(
            "package",
            "the restored package's native Launcher modes differ from the backup",
        ));
    }
    Ok(Check::ok(
        "package",
        format!(
            "verified release {} {} ({}) is present with matching digest, binary, hooks, and Launcher modes",
            installed.package, installed.version, installed.artifact_digest
        ),
    ))
}

fn secret_checks(refs: &[String], available: &[String], revoked: &[String]) -> Vec<Check> {
    let mut checks = Vec::new();
    let mut blocked = Vec::new();
    for reference in refs {
        if revoked.contains(reference) {
            blocked.push(format!("{reference} is revoked"));
            continue;
        }
        if available.contains(reference) {
            checks.push(Check::ok(
                "secret-reference",
                format!("external secret reference '{reference}' is provisioned"),
            ));
            continue;
        }
        if let Some(name) = reference.strip_prefix("env:") {
            if name.is_empty() || std::env::var(name).is_err() {
                blocked.push(format!("{reference} is not present in the environment"));
            } else {
                checks.push(Check::ok(
                    "secret-reference",
                    format!("external secret reference '{reference}' is present"),
                ));
            }
        } else {
            blocked.push(format!(
                "{reference} uses a provider this host does not support; provide an explicit attestation"
            ));
        }
    }
    checks.extend(blocked.into_iter().map(|reason| {
        Check::blocked(
            "secret-reference",
            format!("required secret unavailable: {reason}"),
        )
    }));
    if checks.is_empty() {
        checks.push(Check::ok(
            "secret-reference",
            "the backup declares no external secret references",
        ));
    }
    checks
}

fn configuration_checks(backup: &Path, worker: &WorkerBackup) -> Result<Vec<Check>, InstanceError> {
    let bytes = std::fs::read(backup.join(&worker.config_archived)).map_err(storage)?;
    let text = String::from_utf8(bytes).map_err(|_| {
        InstanceError::new(
            "restore.config_invalid",
            "archived operator configuration is not UTF-8 TOML",
        )
    })?;
    let config: toml::Value = toml::from_str(&text).map_err(|_| {
        InstanceError::new(
            "restore.config_invalid",
            "archived operator configuration is not valid TOML",
        )
    })?;
    let storage = config
        .get("storage")
        .and_then(|storage| storage.get("directory"))
        .and_then(toml::Value::as_str);
    let mut checks = vec![Check::ok(
        "configuration",
        "archived operator configuration parses",
    )];
    if storage.is_none() {
        checks.push(Check::warning(
            "configuration",
            "configuration has no [storage] directory; the instance data directory remains authoritative",
        ));
    }
    for key in ["agents", "prompts", "skills"] {
        if let Some(value) = config_path_value(&config, key) {
            if value.trim().is_empty() || has_parent_component(&value) {
                checks.push(Check::blocked(
                    "configuration",
                    format!("configuration path '{key}' is not a valid directory reference"),
                ));
            }
        }
    }
    Ok(checks)
}

fn path_checks(
    backup: &Path,
    worker: &WorkerBackup,
    path_map: &BTreeMap<String, PathBuf>,
    root: &Path,
    id: &str,
) -> Result<Vec<Check>, InstanceError> {
    let mut checks = Vec::new();
    let source_instance = PathBuf::from(&worker.data_dir);
    let target_instance = root.join(id);
    for asset in &worker.external_assets {
        let mapped = path_map.get(&asset.original_path);
        let source_inside = path_within(&PathBuf::from(&asset.original_path), &target_instance);
        let mapped_path = mapped.map(PathBuf::as_path);
        if mapped_path.is_none() && !source_inside && !Path::new(&asset.original_path).is_dir() {
            checks.push(Check::blocked(
                "external-path",
                format!(
                    "external {} path '{}' is absent on this host and no explicit mapping was supplied",
                    asset.kind, asset.original_path
                ),
            ));
            continue;
        }
        if let Some(archived) = &asset.archived_path {
            if !backup.join(archived).exists() {
                checks.push(Check::blocked(
                    "external-path",
                    format!("external {} archive is missing", asset.kind),
                ));
            }
        }
        checks.push(Check::ok(
            "external-path",
            format!(
                "external {} path '{}' has an explicit restore location",
                asset.kind, asset.original_path
            ),
        ));
    }
    if source_instance.exists() && !path_within(&source_instance, root) {
        checks.push(Check::warning(
            "source-path",
            "the source instance directory still exists on this host; restore writes only to the target root",
        ));
    }
    Ok(checks)
}

fn recovery_checks(
    backup: &Path,
    worker: &WorkerBackup,
    persisted: Option<&Value>,
    installed: &install::Installed,
    data_dir: &Path,
) -> Vec<Check> {
    let Some(persisted) = persisted else {
        return vec![Check::ok(
            "recovery",
            "the instance has no durable state yet; no unfinished work requires reconciliation",
        )];
    };
    let profiles = profiles_from_state(persisted)
        .into_iter()
        .flat_map(|profile| {
            profile
                .revisions
                .into_iter()
                .map(move |revision| (profile.name.clone(), revision))
        })
        .collect::<BTreeSet<_>>();
    let mut checks = Vec::new();
    for task in tasks_from_state(persisted, &worker.release.artifact_digest) {
        if is_terminal_state(&task.state) {
            continue;
        }
        let mut reasons = Vec::new();
        if task.package_digest != installed.artifact_digest {
            reasons.push("package definition differs".to_string());
        }
        if !task.profile.is_empty()
            && !profiles.contains(&(task.profile.clone(), task.profile_revision))
        {
            reasons.push("profile revision is absent".to_string());
        }
        if !task.scopes.is_empty() {
            let missing = task
                .scopes
                .iter()
                .filter(|scope| !worker.grants.contains(scope))
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                reasons.push(format!("authority is missing {}", missing.join(",")));
            }
        }
        if let Some(expiry) = task.authority_expires_at {
            if expiry > 0 && expiry < now_ms() {
                reasons.push("delegated authority expired".to_string());
            }
        }
        if !task.session_id.is_empty()
            && !backup
                .join(&worker.data_archived)
                .join("native-sessions.json")
                .is_file()
        {
            reasons.push("native session history is missing".to_string());
        }
        if task.native_thread_id.is_some()
            && !backup
                .join(&worker.data_archived)
                .join("native-graph.sqlite3")
                .is_file()
        {
            reasons.push("native graph checkpoint is missing".to_string());
        }
        if task.native_a2a_task_id.is_some()
            && !backup
                .join(&worker.data_archived)
                .join("a2a-tasks.sqlite3")
                .is_file()
        {
            reasons.push("native A2A task binding is missing".to_string());
        }
        if task.asset_revision > 0 {
            for asset in &worker.external_assets {
                if asset.files.iter().any(|path| !backup.join(path).is_file()) {
                    reasons.push(format!("{} assets are incomplete", asset.kind));
                }
            }
        }
        if reasons.is_empty() {
            checks.push(Check::ok(
                format!("recovery:{}", task.task_id),
                "originating definition, authority, session, and checkpoint are present",
            ));
        } else {
            checks.push(Check::blocked(
                format!("recovery:{}", task.task_id),
                format!(
                    "unfinished task is quarantined instead of resumed: {}",
                    reasons.join("; ")
                ),
            ));
        }
    }
    if checks.is_empty() {
        checks.push(Check::ok(
            "recovery",
            "no unfinished task requires resumption",
        ));
    }
    // Native stores must be present and readable after restore.
    let sqlite = sqlite_files(data_dir);
    if sqlite.is_empty() {
        checks.push(Check::ok(
            "sqlite-consistency",
            "no SQLite native store is present in this backup",
        ));
    } else {
        checks.push(Check::ok(
            "sqlite-consistency",
            format!(
                "{} SQLite store(s) were restored with their WAL/SHM companions",
                sqlite.len()
            ),
        ));
    }
    checks
}

fn launcher_check(installed: &install::Installed, recorded: &[String]) -> Check {
    if recorded.is_empty() {
        return Check::skipped("launcher", "the deployment enabled no native Launcher mode");
    }
    let mut failures = Vec::new();
    let mut verified = Vec::new();
    for mode in recorded {
        match upgrade::launcher_probe(&installed.binary, mode) {
            Ok(()) => verified.push(mode.clone()),
            Err(message) => failures.push(format!("{mode}: {message}")),
        }
    }
    if failures.is_empty() {
        Check::ok(
            "launcher",
            format!(
                "restored native Launcher composition verified: {}",
                verified.join(",")
            ),
        )
    } else {
        Check::blocked("launcher", failures.join("; "))
    }
}

fn gateway_check(worker: &WorkerBackup, request: &RestoreRequest) -> Result<Check, InstanceError> {
    if !worker.gateway_enabled {
        return Ok(Check::skipped(
            "gateway",
            "the backed-up deployment did not select a Gateway connection",
        ));
    }
    let Some(url) = &request.gateway_url else {
        return Ok(Check::blocked(
            "gateway",
            "the deployment enabled Gateway access; supply the distinct Gateway URL and credential for verification",
        ));
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| {
            InstanceError::new(
                "restore.gateway_check_failed",
                format!("Gateway verification client could not start: {error}"),
            )
        })?;
    let mut builder = client.get(format!("{}/v1/health", url.trim_end_matches('/')));
    if let Some(token) = &request.gateway_token {
        builder = builder.bearer_auth(token);
    }
    match builder.send() {
        Ok(response) if response.status().is_success() => Ok(Check::ok(
            "gateway",
            "restored deployment can reach its Gateway through the SDK/HTTP boundary",
        )),
        Ok(response) => Ok(Check::blocked(
            "gateway",
            format!(
                "Gateway access check returned HTTP {}; restore is not ready",
                response.status()
            ),
        )),
        Err(_) => Ok(Check::blocked(
            "gateway",
            "Gateway access check failed; restore is not ready",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn materialize_instance(
    backup: &Path,
    worker: &WorkerBackup,
    stage: &Path,
    id: &str,
    installed: &install::Installed,
    staged_config: &Path,
    staged_data: &Path,
    staged_workspace: &Path,
    target_config: &Path,
    target_data: &Path,
    target_workspace: &Path,
    path_map: &BTreeMap<String, PathBuf>,
    target_dir: &Path,
) -> Result<(), InstanceError> {
    let manifest = read_manifest(backup)?;
    std::fs::create_dir_all(stage).map_err(storage)?;
    for file in &manifest.files {
        let Some(relative) = file.path.strip_prefix("instance/") else {
            continue;
        };
        if relative == "instance.json" {
            continue;
        }
        let destination = stage.join(relative);
        copy_archived(backup, &file.path, &destination)?;
    }
    let source_data = backup.join(&worker.data_archived);
    copy_dir(&source_data, staged_data)?;
    std::fs::create_dir_all(staged_workspace).map_err(storage)?;
    set_mode(staged_workspace, 0o700)?;
    for asset in &worker.external_assets {
        let target = if let Some(relative) = asset
            .archived_path
            .as_deref()
            .and_then(|archived| archived.strip_prefix("instance/"))
        {
            stage.join(relative)
        } else {
            path_map
                .get(&asset.original_path)
                .cloned()
                .unwrap_or_else(|| PathBuf::from(&asset.original_path))
        };
        let Some(archived) = &asset.archived_path else {
            continue;
        };
        let source = backup.join(archived);
        if source.is_dir() {
            copy_dir(&source, &target)?;
        }
    }
    let config_source = backup.join(&worker.config_archived);
    let config_text = std::fs::read_to_string(&config_source).map_err(storage)?;
    let mut config: toml::Value = toml::from_str(&config_text).map_err(|_| {
        InstanceError::new(
            "restore.config_invalid",
            "archived configuration is not valid TOML",
        )
    })?;
    rewrite_config_paths(&mut config, worker, target_data, target_dir, path_map);
    let rendered = toml::to_string_pretty(&config).map_err(|_| {
        InstanceError::new(
            "restore.config_invalid",
            "restored configuration could not be rendered",
        )
    })?;
    if let Some(parent) = staged_config.parent() {
        std::fs::create_dir_all(parent).map_err(storage)?;
    }
    write_file(staged_config, rendered.as_bytes(), 0o600)?;

    let mut record: InstanceRecord = serde_json::from_slice(
        &std::fs::read(backup.join("instance").join("instance.json")).map_err(storage)?,
    )
    .map_err(|_| {
        InstanceError::new(
            "restore.metadata_invalid",
            "archived instance metadata is malformed",
        )
    })?;
    record.id = id.to_string();
    record.release_dir = installed.directory.clone();
    record.binary = installed.binary.clone();
    record.config_path = target_config.to_path_buf();
    record.data_dir = target_data.to_path_buf();
    record.workspace_root = target_workspace.to_path_buf();
    write_json(&stage.join("instance.json"), &record, 0o600)?;
    Ok(())
}

fn rewrite_config_paths(
    config: &mut toml::Value,
    worker: &WorkerBackup,
    target_data: &Path,
    target_instance: &Path,
    path_map: &BTreeMap<String, PathBuf>,
) {
    if let Some(storage) = config
        .get_mut("storage")
        .and_then(toml::Value::as_table_mut)
    {
        storage.insert(
            "directory".into(),
            toml::Value::String(target_data.display().to_string()),
        );
    }
    if let Some(paths) = config.get_mut("paths").and_then(toml::Value::as_table_mut) {
        for asset in &worker.external_assets {
            let target = path_map.get(&asset.original_path).cloned().or_else(|| {
                asset
                    .archived_path
                    .as_deref()
                    .and_then(|archived| archived.strip_prefix("instance/"))
                    .map(|relative| target_instance.join(relative))
            });
            if let Some(target) = target {
                paths.insert(
                    asset.kind.clone(),
                    toml::Value::String(target.display().to_string()),
                );
            }
        }
    }
}

fn finalize_state(
    stage: &Path,
    quarantine: bool,
    blocked: &[BlockedTask],
) -> Result<(), InstanceError> {
    let state_path = stage.join("state.json");
    let bytes = std::fs::read(&state_path).map_err(storage)?;
    let mut state: InstanceState = serde_json::from_slice(&bytes).map_err(|error| {
        InstanceError::new(
            "restore.state_invalid",
            format!(
                "restored instance state at {} has no readable lifecycle record: {error}",
                state_path.display()
            ),
        )
    })?;
    state.lifecycle = if quarantine {
        "quarantined".into()
    } else {
        "stopped".into()
    };
    state.pid = None;
    state.addr = None;
    state.incarnation = None;
    state.started_at = None;
    state.stopped_at = Some(now_ms());
    state.last_error = if quarantine {
        if blocked.is_empty() {
            Some("restore.quarantined: restore verification did not establish readiness".into())
        } else {
            Some(format!(
                "restore.quarantined: unfinished work lacks {}",
                blocked
                    .iter()
                    .map(|task| task.task_id.clone())
                    .collect::<Vec<_>>()
                    .join(",")
            ))
        }
    } else {
        None
    };
    state.trust = if quarantine {
        Some(crate::layout::TrustStatus {
            decision: install::InstalledAction::Quarantine,
            reason:
                "restored unfinished work could not be reconciled to its originating definitions"
                    .into(),
            advisory_generation: None,
            decided_at: now_ms(),
        })
    } else {
        None
    };
    write_json(&state_path, &state, 0o600)?;
    Ok(())
}

fn resolve_release(
    root: &Path,
    explicit: Option<&Path>,
    worker: &WorkerBackup,
) -> Result<PathBuf, InstanceError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    let digest = worker
        .release
        .artifact_digest
        .strip_prefix("sha256:")
        .unwrap_or(&worker.release.artifact_digest);
    for base in [
        root.to_path_buf(),
        root.parent().map(Path::to_path_buf).unwrap_or_default(),
    ] {
        let candidate = base.join("packages").join("sha256").join(digest);
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }
    Err(InstanceError::new(
        "restore.release_missing",
        format!(
            "verified release {} is not present on this host; restore does not rebuild a Worker or substitute bundled defaults",
            worker.release.artifact_digest
        ),
    ))
}

fn stage_for(target: &Path, target_instance: &Path, stage: &Path) -> PathBuf {
    target
        .strip_prefix(target_instance)
        .map(|relative| stage.join(relative))
        .unwrap_or_else(|_| target.to_path_buf())
}

fn resolve_target_path(
    explicit: Option<&Path>,
    map: &BTreeMap<String, PathBuf>,
    original: &str,
    default: &Path,
) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| map.get(original).cloned())
        .unwrap_or_else(|| default.to_path_buf())
}

fn archived_target(target_instance: &Path, archived: &str, original: &str) -> PathBuf {
    if let Some(relative) = archived.strip_prefix("instance/") {
        target_instance.join(relative)
    } else {
        PathBuf::from(original)
    }
}

fn relative_plain(path: &Path, base: &Path) -> PathBuf {
    path.strip_prefix(base)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| PathBuf::from("workspace"))
}

fn release_metadata(installed: &install::Installed) -> Result<ReleaseMetadata, InstanceError> {
    let bytes = std::fs::read(installed.directory.join("metadata.json")).map_err(|_| {
        InstanceError::new(
            "backup.release_invalid",
            "verified release has no readable signed metadata",
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        InstanceError::new(
            "backup.release_invalid",
            "verified release metadata is malformed",
        )
    })
}

fn read_manifest(backup: &Path) -> Result<BackupManifest, InstanceError> {
    let path = backup.join(MANIFEST_FILE);
    let bytes = std::fs::read(&path).map_err(|_| {
        InstanceError::new(
            "backup.unreadable",
            "backup manifest is missing or unreadable",
        )
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|_| InstanceError::new("backup.manifest_invalid", "backup manifest is malformed"))
}

fn has_file(manifest: &BackupManifest, path: &str) -> bool {
    manifest.files.iter().any(|file| file.path == path)
}

fn manifest_digest(manifest: &BackupManifest) -> Result<String, InstanceError> {
    let bytes = serde_json::to_vec(manifest)
        .map_err(|error| InstanceError::new("backup.storage_unavailable", error.to_string()))?;
    Ok(common::bytes_digest(&bytes))
}

fn profiles_from_state(state: &Value) -> Vec<BackupProfile> {
    state
        .get("profiles")
        .and_then(Value::as_object)
        .map(|profiles| {
            profiles
                .iter()
                .map(|(key, profile)| BackupProfile {
                    name: profile
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(key)
                        .to_string(),
                    active_revision: profile
                        .get("active_revision")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    revisions: profile
                        .get("revisions")
                        .and_then(Value::as_array)
                        .map(|revisions| {
                            revisions
                                .iter()
                                .filter_map(|revision| {
                                    revision.get("revision").and_then(Value::as_u64)
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tasks_from_state(state: &Value, package_digest: &str) -> Vec<BackupTask> {
    state
        .get("tasks")
        .and_then(Value::as_object)
        .map(|tasks| {
            tasks
                .iter()
                .map(|(id, task)| BackupTask {
                    task_id: id.clone(),
                    state: task
                        .get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    session_id: string_field(task, "session_id"),
                    profile: string_field(task, "profile"),
                    profile_revision: task
                        .get("profile_revision")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    asset_revision: task
                        .get("asset_revision")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    scopes: string_list(task, "scopes"),
                    authority_expires_at: task.get("authority_expires_at").and_then(Value::as_i64),
                    native_invocation_id: optional_string(task, "native_invocation_id"),
                    native_thread_id: optional_string(task, "native_thread_id"),
                    native_a2a_task_id: optional_string(task, "native_a2a_task_id"),
                    package_digest: package_digest.to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn artifacts_from_state(state: &Value) -> Vec<BackupArtifact> {
    state
        .get("artifacts")
        .and_then(Value::as_object)
        .map(|artifacts| {
            artifacts
                .iter()
                .map(|(id, artifact)| BackupArtifact {
                    id: id.clone(),
                    digest: string_field(artifact, "digest"),
                    version: artifact.get("version").and_then(Value::as_i64).unwrap_or(0),
                    size: artifact.get("size").and_then(Value::as_u64).unwrap_or(0),
                    finalized: artifact
                        .get("finalized")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn string_list(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn is_terminal_state(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "cancelled")
}

fn looks_like_secret_value(value: &str) -> bool {
    value.starts_with("sk-")
        || value.starts_with("Bearer ")
        || value.starts_with("AKIA")
        || value.len() > 256
}

fn config_path_value(config: &toml::Value, key: &str) -> Option<String> {
    config
        .get("paths")
        .and_then(|paths| paths.get(key))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
}

fn resolve_config_path(config: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else if let Some(parent) = config.parent() {
        parent.join(path)
    } else {
        path
    }
}

fn has_parent_component(value: &str) -> bool {
    Path::new(value)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn read_toml(path: &Path) -> Result<toml::Value, InstanceError> {
    let text = std::fs::read_to_string(path).map_err(storage)?;
    toml::from_str(&text).map_err(|_| {
        InstanceError::new(
            "backup.config_invalid",
            "operator configuration is not valid TOML",
        )
    })
}

fn read_state_json(data_dir: &Path) -> Result<Option<Value>, InstanceError> {
    let path = data_dir.join("state.json");
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(storage)?;
    serde_json::from_slice(&bytes).map(Some).map_err(|_| {
        InstanceError::new(
            "backup.state_invalid",
            "durable Worker state is not valid JSON",
        )
    })
}

fn journal_data_schema(target: &Path) -> Result<u32, InstanceError> {
    let manifest = read_manifest(target)?;
    Ok(manifest
        .worker
        .as_ref()
        .map(|worker| worker.data_schema)
        .unwrap_or(0))
}

fn steps_from_checks(checks: &[Check]) -> Vec<MigrationStep> {
    checks
        .iter()
        .map(|check| MigrationStep {
            name: check.name.clone(),
            status: check.status.clone(),
            detail: check.detail.clone(),
        })
        .collect()
}

fn write_journal(target: &Path, journal: &MigrationJournal) -> Result<(), InstanceError> {
    write_json(&target.join(MIGRATION_JOURNAL), journal, 0o600)
}

fn interruption_after(step: usize) -> bool {
    std::env::var("GOLDENBEE_BACKUP_INTERRUPT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|value| value == step)
}

fn copy_instance_tree(
    paths: &InstancePaths,
    record: &InstanceRecord,
    stage: &Path,
    files: &mut Vec<BackupFile>,
) -> Result<(), InstanceError> {
    let skip = [
        "backups".to_string(),
        "quarantine".to_string(),
        "tmp".to_string(),
    ];
    copy_tree(
        &paths.dir,
        &stage.join("instance"),
        "instance",
        files,
        stage,
        &skip,
    )?;
    // The data directory may be outside the instance directory. It is copied
    // separately so restore can place it explicitly.
    if !path_within(&record.data_dir, &paths.dir) {
        copy_tree(
            &record.data_dir,
            &stage.join("data"),
            "native-data",
            files,
            stage,
            &[],
        )?;
    }
    Ok(())
}

fn copy_archived(backup: &Path, archived: &str, destination: &Path) -> Result<(), InstanceError> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(storage)?;
    }
    let bytes = std::fs::read(backup.join(archived)).map_err(storage)?;
    write_file(destination, &bytes, 0o600)
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    role: &str,
    files: &mut Vec<BackupFile>,
    _stage: &Path,
    skip: &[String],
) -> Result<(), InstanceError> {
    if !source.is_dir() {
        return Err(InstanceError::new(
            "backup.path_missing",
            format!("source directory '{}' is missing", source.display()),
        ));
    }
    std::fs::create_dir_all(destination).map_err(storage)?;
    for entry in std::fs::read_dir(source).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        let name = entry.file_name().to_string_lossy().to_string();
        if skip.iter().any(|value| value == &name) {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type().map_err(storage)?;
        let target = destination.join(&name);
        if kind.is_dir() {
            copy_tree(&path, &target, role, files, _stage, skip)?;
        } else if kind.is_file() {
            if name == "process.log" {
                continue;
            }
            copy_one(&path, &target, role, files)?;
        }
    }
    Ok(())
}

fn copy_one(
    source: &Path,
    destination: &Path,
    role: &str,
    files: &mut Vec<BackupFile>,
) -> Result<(), InstanceError> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(storage)?;
    }
    std::fs::copy(source, destination).map_err(storage)?;
    set_mode(destination, 0o600)?;
    let bytes = std::fs::read(destination).map_err(storage)?;
    let stage_root = destination
        .ancestors()
        .find(|ancestor| {
            ancestor
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(STAGING_PREFIX)
                        || name.starts_with(RESTORE_PREFIX)
                        || name.starts_with(MIGRATION_PREFIX)
                })
        })
        .ok_or_else(|| InstanceError::new("backup.storage_unavailable", "staging root is lost"))?;
    files.push(BackupFile {
        path: relative(destination, stage_root)?
            .to_string_lossy()
            .replace('\\', "/"),
        role: role.into(),
        digest: common::bytes_digest(&bytes),
        size: bytes.len() as u64,
    });
    Ok(())
}

fn copy_dir(source: &Path, destination: &Path) -> Result<(), InstanceError> {
    if !source.is_dir() {
        return Err(InstanceError::new(
            "backup.path_missing",
            format!("source directory '{}' is missing", source.display()),
        ));
    }
    std::fs::create_dir_all(destination).map_err(storage)?;
    for entry in std::fs::read_dir(source).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        let kind = entry.file_type().map_err(storage)?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target).map_err(storage)?;
        }
    }
    Ok(())
}

fn tree_digest(root: &Path) -> Result<String, InstanceError> {
    if !root.is_dir() {
        return Ok(String::new());
    }
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

fn sqlite_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.filter_map(Result::ok) {
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                let path = entry.path();
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.ends_with(".sqlite3")
                            || name.ends_with(".db")
                            || name.ends_with(".sqlite")
                    })
                {
                    found.push(path);
                }
            }
        }
    }
    found
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.starts_with('/')
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn relative(path: &Path, base: &Path) -> Result<PathBuf, InstanceError> {
    path.strip_prefix(base)
        .map(Path::to_path_buf)
        .map_err(|_| InstanceError::new("backup.path_invalid", "path escapes its backup root"))
}

fn path_within(path: &Path, base: &Path) -> bool {
    path.strip_prefix(base).is_ok()
}

fn absolute(path: &Path) -> Result<PathBuf, InstanceError> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(storage)?.join(path)
    };
    if path.exists() {
        std::fs::canonicalize(&path).map_err(storage)
    } else {
        Ok(path)
    }
}

fn sync_tree(path: &Path) -> Result<(), InstanceError> {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).map_err(storage)? {
            let entry = entry.map_err(storage)?;
            let kind = entry.file_type().map_err(storage)?;
            if kind.is_dir() {
                sync_tree(&entry.path())?;
            } else if kind.is_file() {
                std::fs::File::open(entry.path())
                    .map_err(storage)?
                    .sync_all()
                    .map_err(storage)?;
            }
        }
    }
    sync_dir(path)
}

fn sync_dir(path: &Path) -> Result<(), InstanceError> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)
            .map_err(storage)?
            .sync_all()
            .map_err(storage)?;
    }
    Ok(())
}

fn storage(error: std::io::Error) -> InstanceError {
    InstanceError::new(
        "backup.storage_unavailable",
        format!("backup storage is unavailable: {error}"),
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn host_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "unknown-host".into())
}

struct StageGuard {
    path: PathBuf,
}

impl StageGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_paths_are_constrained() {
        assert!(safe_relative("instance/worker.toml"));
        assert!(safe_relative("data/native-graph.sqlite3"));
        assert!(!safe_relative("../escape"));
        assert!(!safe_relative("/absolute"));
        assert!(!safe_relative("a/../../b"));
    }

    #[test]
    fn secret_references_are_not_values() {
        assert!(!looks_like_secret_value("env:MODEL_KEY"));
        assert!(!looks_like_secret_value("secret://worker/model"));
        assert!(looks_like_secret_value("sk-live-value"));
        assert!(looks_like_secret_value(&"x".repeat(300)));
    }
}
