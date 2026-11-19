//! Gateway-owned backup and restore adapters.
//!
//! The standalone Gateway owns a JSON dispatch store and a JSON registry. They
//! stay separate from Worker-native state: a Gateway backup never embeds an
//! instance data directory, and a Worker backup never embeds these files. The
//! manifest records Worker identities, team rosters, execution mappings, and
//! approval metadata without caller tokens, control tokens, or raw credentials.
//!
//! A backup requires the single-writer lock so the archived JSON files are a
//! quiescent snapshot. The Postgres adapter has no file-level snapshot here;
//! operators use `pg_dump` and keep that separate from a Worker backup.

use common::backup::{BackupApproval, BackupExecution, BackupFile, BackupManifest, GatewayBackup};
use common::ApiError;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::registry::REGISTRY_SCHEMA;
use crate::store::{SingleWriter, DISPATCH_SCHEMA};

/// Manifest filename inside a Gateway backup.
pub const MANIFEST_FILE: &str = "manifest.json";
const STAGING_PREFIX: &str = ".gateway-backup-staging-";

/// Gateway backup request.
#[derive(Debug, Clone)]
pub struct BackupRequest {
    /// Gateway dispatch data directory containing `dispatch.json`.
    pub data_dir: PathBuf,
    /// Persistent registry path, when the deployment uses one.
    pub registry: Option<PathBuf>,
    /// New backup directory.
    pub output: PathBuf,
}

/// Created Gateway backup.
#[derive(Debug, Clone)]
pub struct BackupOutcome {
    /// Written manifest.
    pub manifest: BackupManifest,
    /// Backup directory.
    pub path: PathBuf,
}

/// One verification check.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Check {
    /// Stable check name.
    pub name: String,
    /// `ok`, `warning`, `blocked`, or `skipped`.
    pub status: String,
    /// Redacted detail.
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

/// Verification result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VerifyReport {
    /// Whether every required check passed.
    pub ready: bool,
    /// Parsed manifest.
    pub manifest: BackupManifest,
    /// Check evidence.
    pub checks: Vec<Check>,
}

/// Restore request.
#[derive(Debug, Clone)]
pub struct RestoreRequest {
    /// Gateway backup directory.
    pub backup: PathBuf,
    /// Target dispatch data directory.
    pub data_dir: PathBuf,
    /// Target registry path.
    pub registry: Option<PathBuf>,
    /// A Worker root that the target must not overlap. It proves the restored
    /// Gateway projection is a separate store.
    pub not_within: Option<PathBuf>,
    /// Operator configuration file used to validate that access is configured.
    pub caller_config: Option<PathBuf>,
    /// Revoked Worker identities.
    pub revoked_workers: Vec<String>,
    /// Revoked approval identities.
    pub revoked_approvals: Vec<String>,
}

/// Restore result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RestoreReport {
    /// `ready`, `blocked`, or `quarantined`.
    pub status: String,
    /// Whether the restored Gateway may start.
    pub ready: bool,
    /// Preserved Gateway projection identity.
    pub gateway_id: String,
    /// Restored dispatch directory.
    pub data_dir: PathBuf,
    /// Restored registry path.
    pub registry: Option<PathBuf>,
    /// Check evidence.
    pub checks: Vec<Check>,
    /// Distinct access-configuration check.
    pub access: Check,
}

/// Create one Gateway backup while no other writer owns the store.
pub fn create(request: &BackupRequest) -> Result<BackupOutcome, ApiError> {
    std::fs::create_dir_all(&request.data_dir).map_err(io)?;
    let _lock = SingleWriter::acquire(&request.data_dir.join("gateway.lock"))?;
    let dispatch_path = request.data_dir.join("dispatch.json");
    if !dispatch_path.is_file() {
        return Err(ApiError::new(
            "backup.store_missing",
            "Gateway dispatch store is missing; start the Gateway once before backing it up",
        ));
    }
    let dispatch: Value = read_json(&dispatch_path)?;
    let dispatch_schema = value_u32(&dispatch, "schema");
    if dispatch_schema > DISPATCH_SCHEMA {
        return Err(ApiError::new(
            "backup.schema_unsupported",
            format!(
                "Gateway dispatch schema {dispatch_schema} is newer than supported {DISPATCH_SCHEMA}; it was not copied"
            ),
        ));
    }
    let registry = match &request.registry {
        Some(path) if path.is_file() => Some(read_json(path)?),
        Some(_) => {
            return Err(ApiError::new(
                "backup.registry_missing",
                "configured Gateway registry file is missing",
            ))
        }
        None => None,
    };
    let registry_schema = registry
        .as_ref()
        .map(|value| value_u32(value, "version"))
        .unwrap_or(0);
    if registry_schema > REGISTRY_SCHEMA {
        return Err(ApiError::new(
            "backup.schema_unsupported",
            format!(
                "Gateway registry schema {registry_schema} is newer than supported {REGISTRY_SCHEMA}; it was not copied"
            ),
        ));
    }

    let parent = request
        .output
        .parent()
        .ok_or_else(|| ApiError::new("backup.path_invalid", "backup output has no parent"))?;
    std::fs::create_dir_all(parent).map_err(io)?;
    if request.output.exists() {
        return Err(ApiError::new(
            "backup.exists",
            "backup output already exists; choose a new directory",
        ));
    }
    let stage = parent.join(format!("{STAGING_PREFIX}{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(stage.join("stores")).map_err(io)?;
    let guard = StageGuard::new(stage.clone());
    let mut files = Vec::new();
    copy_archive_file(
        &dispatch_path,
        &stage.join("stores/dispatch.json"),
        "gateway-store",
        &mut files,
        &stage,
    )?;
    if let Some(path) = &request.registry {
        if path.is_file() {
            copy_archive_file(
                path,
                &stage.join("stores/registry.json"),
                "gateway-store",
                &mut files,
                &stage,
            )?;
        }
    }

    let mut gateway = GatewayBackup {
        gateway_id: String::new(),
        store_kind: "file".into(),
        dispatch_schema,
        registry_schema,
        workers: workers_from_registry(&registry),
        teams: teams_from_registry(&registry),
        approvals: approvals_from_registry(&registry),
        executions: executions_from_dispatch(&dispatch),
    };
    gateway.gateway_id = gateway_identity(&gateway);
    let mut manifest = BackupManifest::new(
        format!("gateway-{}", uuid::Uuid::new_v4().simple()),
        host_name(),
    );
    manifest.gateway = Some(gateway);
    manifest.files = files;
    manifest
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    write_json(&stage.join(MANIFEST_FILE), &manifest)?;
    sync_tree(&stage)?;
    std::fs::rename(&stage, &request.output).map_err(io)?;
    sync_dir(parent)?;
    drop(guard);
    Ok(BackupOutcome {
        manifest,
        path: request.output.clone(),
    })
}

/// Verify one Gateway backup.
pub fn verify(backup: &Path) -> Result<VerifyReport, ApiError> {
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
    checks.push(Check::ok("schema", "manifest schema is supported"));
    let mut content_ok = true;
    let mut seen = BTreeSet::new();
    for file in &manifest.files {
        if !safe_relative(&file.path) || !seen.insert(file.path.as_str()) {
            content_ok = false;
            checks.push(Check::blocked(
                "content",
                format!("archived path '{}' is unsafe or repeated", file.path),
            ));
            continue;
        }
        let bytes = match std::fs::read(backup.join(&file.path)) {
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
        if bytes.len() as u64 != file.size || common::bytes_digest(&bytes) != file.digest {
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
    let Some(gateway) = &manifest.gateway else {
        checks.push(Check::blocked(
            "owner",
            "manifest does not contain a Gateway owner section",
        ));
        return Ok(VerifyReport {
            ready: false,
            manifest,
            checks,
        });
    };
    if gateway.store_kind != "file" {
        checks.push(Check::blocked(
            "store-kind",
            format!(
                "store kind '{}' requires its own native backup procedure",
                gateway.store_kind
            ),
        ));
    } else {
        checks.push(Check::ok(
            "store-kind",
            "standalone file store is captured as a quiescent snapshot",
        ));
    }
    if gateway.dispatch_schema > DISPATCH_SCHEMA {
        checks.push(Check::blocked(
            "schema",
            format!(
                "Gateway dispatch schema {} is newer than supported {DISPATCH_SCHEMA}",
                gateway.dispatch_schema
            ),
        ));
    } else if gateway.registry_schema > REGISTRY_SCHEMA {
        checks.push(Check::blocked(
            "schema",
            format!(
                "Gateway registry schema {} is newer than supported {REGISTRY_SCHEMA}",
                gateway.registry_schema
            ),
        ));
    } else {
        checks.push(Check::ok(
            "store-schema",
            format!(
                "dispatch schema {} and registry schema {} are supported",
                gateway.dispatch_schema, gateway.registry_schema
            ),
        ));
    }
    let expected = gateway_identity(gateway);
    if expected != gateway.gateway_id {
        checks.push(Check::blocked(
            "gateway-identity",
            "Gateway projection identity does not match its restored mappings",
        ));
    } else {
        checks.push(Check::ok(
            "gateway-identity",
            "Gateway projection identity is derived from its mappings",
        ));
    }
    let workers: BTreeSet<&str> = gateway.workers.iter().map(String::as_str).collect();
    let mut missing = Vec::new();
    for execution in &gateway.executions {
        if !workers.contains(execution.worker.as_str()) {
            missing.push(execution.execution_id.clone());
        }
        if execution.dispatch == "accepted" && execution.worker_task_id.is_none() {
            missing.push(format!(
                "{} accepted-without-handle",
                execution.execution_id
            ));
        }
    }
    if missing.is_empty() {
        checks.push(Check::ok(
            "gateway-mappings",
            format!(
                "{} execution mapping(s) reference registered Workers and accepted handles",
                gateway.executions.len()
            ),
        ));
    } else {
        checks.push(Check::blocked(
            "gateway-mappings",
            format!(
                "Gateway projections are inconsistent: {}",
                missing.join(",")
            ),
        ));
    }
    let mut approval_ids = BTreeSet::new();
    for approval in &gateway.approvals {
        if !approval_ids.insert(approval.id.as_str()) {
            checks.push(Check::blocked(
                "gateway-approvals",
                "approval identity appears twice",
            ));
        }
    }
    if manifest
        .files
        .iter()
        .any(|file| file.role == "gateway-store")
    {
        checks.push(Check::ok(
            "gateway-content",
            "Gateway files are archived independently of Worker-native state",
        ));
    } else {
        checks.push(Check::blocked(
            "gateway-content",
            "Gateway store files are missing",
        ));
    }
    let ready = checks.iter().all(|check| check.status != "blocked");
    Ok(VerifyReport {
        ready,
        manifest,
        checks,
    })
}

/// Restore a Gateway projection into an empty directory. The archive is never
/// merged into a Worker data directory.
pub fn restore(request: &RestoreRequest) -> Result<RestoreReport, ApiError> {
    let verification = verify(&request.backup)?;
    let gateway_id = verification
        .manifest
        .gateway
        .as_ref()
        .map(|gateway| gateway.gateway_id.clone())
        .unwrap_or_default();
    if !verification.ready {
        return Ok(RestoreReport {
            status: "blocked".into(),
            ready: false,
            gateway_id,
            data_dir: request.data_dir.clone(),
            registry: request.registry.clone(),
            checks: verification.checks,
            access: Check::skipped("access", "backup did not validate"),
        });
    }
    let manifest = verification.manifest;
    let gateway = manifest.gateway.as_ref().ok_or_else(|| {
        ApiError::new(
            "restore.not_gateway_backup",
            "this backup has no Gateway owner section",
        )
    })?;
    let mut checks = verification.checks;
    if let Some(worker_root) = &request.not_within {
        if path_within(&request.data_dir, worker_root)
            || request
                .registry
                .as_ref()
                .is_some_and(|path| path_within(path, worker_root))
        {
            checks.push(Check::blocked(
                "store-separation",
                "Gateway restore target overlaps a Worker root; the projection must stay a separate store",
            ));
        } else {
            checks.push(Check::ok(
                "store-separation",
                "Gateway projection is restored outside the Worker root",
            ));
        }
    } else {
        checks.push(Check::warning(
            "store-separation",
            "no Worker root was supplied; store separation was not proven",
        ));
    }
    for worker in &request.revoked_workers {
        if gateway.workers.contains(worker)
            || gateway
                .executions
                .iter()
                .any(|execution| &execution.worker == worker)
        {
            checks.push(Check::blocked(
                "revoked-credential",
                format!("Gateway projection references revoked Worker identity '{worker}'"),
            ));
        }
    }
    for approval in &request.revoked_approvals {
        if gateway.approvals.iter().any(|item| &item.id == approval) {
            checks.push(Check::blocked(
                "revoked-credential",
                format!("Gateway projection references revoked approval '{approval}'"),
            ));
        }
    }
    let access = access_check(request.caller_config.as_deref())?;
    if access.status == "blocked" {
        checks.push(access.clone());
    }
    if request.data_dir.exists() && has_any_file(&request.data_dir) {
        checks.push(Check::blocked(
            "target",
            "Gateway dispatch target is not empty; restore requires a clean directory",
        ));
    }
    if request.registry.as_ref().is_some_and(|path| path.exists()) {
        checks.push(Check::blocked(
            "target",
            "Gateway registry target already exists; restore requires a clean file",
        ));
    }
    if checks.iter().any(|check| check.status == "blocked") {
        return Ok(RestoreReport {
            status: "blocked".into(),
            ready: false,
            gateway_id,
            data_dir: request.data_dir.clone(),
            registry: request.registry.clone(),
            checks,
            access,
        });
    }

    let dispatch_file = archive_file(&manifest, "stores/dispatch.json").ok_or_else(|| {
        ApiError::new(
            "restore.incomplete",
            "archived Gateway dispatch store is missing",
        )
    })?;
    let dispatch = read_json(&request.backup.join(dispatch_file))?;
    if value_u32(&dispatch, "schema") > DISPATCH_SCHEMA {
        return Err(ApiError::new(
            "restore.schema_unsupported",
            "dispatch state was written by a newer Gateway schema",
        ));
    }
    std::fs::create_dir_all(&request.data_dir).map_err(io)?;
    copy_file(
        &request.backup.join(dispatch_file),
        &request.data_dir.join("dispatch.json"),
    )?;
    if let Some(registry_file) = archive_file(&manifest, "stores/registry.json") {
        let registry = read_json(&request.backup.join(registry_file))?;
        if value_u32(&registry, "version") > REGISTRY_SCHEMA {
            return Err(ApiError::new(
                "restore.schema_unsupported",
                "registry state was written by a newer Gateway schema",
            ));
        }
        if let Some(target) = &request.registry {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            copy_file(&request.backup.join(registry_file), target)?;
        }
    }
    checks.push(Check::ok(
        "identity",
        format!(
            "Gateway projection '{gateway_id}' was restored without changing Worker-native state"
        ),
    ));
    checks.push(access.clone());
    Ok(RestoreReport {
        status: "ready".into(),
        ready: true,
        gateway_id,
        data_dir: request.data_dir.clone(),
        registry: request.registry.clone(),
        checks,
        access,
    })
}

fn archive_file<'a>(manifest: &'a BackupManifest, path: &str) -> Option<&'a str> {
    manifest
        .files
        .iter()
        .find(|file| file.path == path)
        .map(|file| file.path.as_str())
}

fn access_check(config: Option<&Path>) -> Result<Check, ApiError> {
    let Some(path) = config else {
        return Ok(Check::blocked(
            "access",
            "caller configuration was not supplied; SDK access cannot be validated",
        ));
    };
    let value = read_json(path)?;
    let callers = value
        .get("callers")
        .and_then(Value::as_array)
        .map(|callers| {
            callers
                .iter()
                .filter(|caller| {
                    caller
                        .get("token")
                        .and_then(Value::as_str)
                        .is_some_and(|token| !token.trim().is_empty())
                        && caller
                            .get("id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| !id.trim().is_empty())
                })
                .count()
        })
        .unwrap_or(0);
    if callers == 0 {
        return Ok(Check::blocked(
            "access",
            "caller configuration has no usable caller binding; SDK access would not be configured",
        ));
    }
    Ok(Check::ok(
        "access",
        format!("{callers} caller binding(s) are configured for the restored Gateway"),
    ))
}

fn has_any_file(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

fn workers_from_registry(registry: &Option<Value>) -> Vec<String> {
    registry
        .as_ref()
        .and_then(|value| value.get("workers"))
        .and_then(Value::as_object)
        .map(|workers| workers.keys().cloned().collect())
        .unwrap_or_default()
}

fn teams_from_registry(registry: &Option<Value>) -> Vec<String> {
    registry
        .as_ref()
        .and_then(|value| value.get("teams"))
        .and_then(Value::as_object)
        .map(|teams| {
            teams
                .values()
                .map(|team| {
                    format!(
                        "{}/{}@{}",
                        value_str(team, "namespace"),
                        value_str(team, "name"),
                        value_u64(team, "version")
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn approvals_from_registry(registry: &Option<Value>) -> Vec<BackupApproval> {
    registry
        .as_ref()
        .and_then(|value| value.get("approvals"))
        .and_then(Value::as_object)
        .map(|approvals| {
            approvals
                .values()
                .map(|approval| BackupApproval {
                    id: value_str(approval, "id"),
                    request_digest: approval
                        .get("request_digest")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    update_digest: approval
                        .get("update_digest")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    revision: value_u64(approval, "revision"),
                    enabled: approval
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                    decommissioned: approval
                        .get("decommissioned")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn executions_from_dispatch(dispatch: &Value) -> Vec<BackupExecution> {
    dispatch
        .get("submissions")
        .and_then(Value::as_object)
        .map(|submissions| {
            submissions
                .values()
                .map(|submission| BackupExecution {
                    execution_id: value_str(submission, "execution_id"),
                    namespace: value_str(submission, "namespace"),
                    caller: value_str(submission, "caller"),
                    worker: value_str(submission, "worker"),
                    worker_instance: value_str(submission, "worker_instance"),
                    worker_incarnation: value_str(submission, "worker_incarnation"),
                    agent: value_str(submission, "agent"),
                    worker_task_id: submission
                        .get("worker_task_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    dispatch: submission
                        .get("dispatch")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    request_digest: value_str(submission, "request_digest"),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn gateway_identity(gateway: &GatewayBackup) -> String {
    let mut workers = gateway.workers.clone();
    workers.sort();
    let mut teams = gateway.teams.clone();
    teams.sort();
    let mut approvals = gateway
        .approvals
        .iter()
        .map(|approval| approval.id.clone())
        .collect::<Vec<_>>();
    approvals.sort();
    let mut executions = gateway
        .executions
        .iter()
        .map(|execution| execution.execution_id.clone())
        .collect::<Vec<_>>();
    executions.sort();
    common::canonical_digest(&(workers, teams, approvals, executions))
}

fn read_json(path: &Path) -> Result<Value, ApiError> {
    let bytes = std::fs::read(path).map_err(|_| {
        ApiError::new(
            "backup.unreadable",
            format!("state file '{}' is unreadable", path.display()),
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::new(
            "backup.invalid",
            format!("state file '{}' is not valid JSON", path.display()),
        )
    })
}

fn read_manifest(backup: &Path) -> Result<BackupManifest, ApiError> {
    let bytes = std::fs::read(backup.join(MANIFEST_FILE)).map_err(|_| {
        ApiError::new(
            "backup.unreadable",
            "Gateway backup manifest is missing or unreadable",
        )
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::new("backup.invalid", "Gateway backup manifest is malformed"))
}

fn value_str(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn value_u32(value: &Value, key: &str) -> u32 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0) as u32
}

fn value_u64(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn copy_archive_file(
    source: &Path,
    destination: &Path,
    role: &str,
    files: &mut Vec<BackupFile>,
    stage: &Path,
) -> Result<(), ApiError> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    std::fs::copy(source, destination).map_err(io)?;
    let bytes = std::fs::read(destination).map_err(io)?;
    let path = destination
        .strip_prefix(stage)
        .map_err(|_| ApiError::new("backup.path_invalid", "archive path escapes its root"))?;
    files.push(BackupFile {
        path: path.to_string_lossy().replace('\\', "/"),
        role: role.into(),
        digest: common::bytes_digest(&bytes),
        size: bytes.len() as u64,
    });
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), ApiError> {
    std::fs::copy(source, destination).map_err(io)?;
    Ok(())
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn path_within(path: &Path, base: &Path) -> bool {
    path.strip_prefix(base).is_ok()
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), ApiError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| ApiError::new("backup.unavailable", error.to_string()))?;
    std::fs::write(path, bytes).map_err(io)
}

fn sync_tree(path: &Path) -> Result<(), ApiError> {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).map_err(io)? {
            let entry = entry.map_err(io)?;
            let kind = entry.file_type().map_err(io)?;
            if kind.is_dir() {
                sync_tree(&entry.path())?;
            } else if kind.is_file() {
                std::fs::File::open(entry.path())
                    .map_err(io)?
                    .sync_all()
                    .map_err(io)?;
            }
        }
    }
    sync_dir(path)
}

fn sync_dir(path: &Path) -> Result<(), ApiError> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)
            .map_err(io)?
            .sync_all()
            .map_err(io)?;
    }
    Ok(())
}

fn io(error: std::io::Error) -> ApiError {
    ApiError::new(
        "backup.storage_unavailable",
        format!("Gateway backup storage is unavailable: {error}"),
    )
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
    fn safe_paths_are_relative() {
        assert!(safe_relative("stores/dispatch.json"));
        assert!(!safe_relative("../dispatch.json"));
        assert!(!safe_relative("/tmp/dispatch.json"));
    }

    #[test]
    fn gateway_identity_is_order_independent() {
        let first = GatewayBackup {
            workers: vec!["b".into(), "a".into()],
            ..GatewayBackup::default()
        };
        let second = GatewayBackup {
            workers: vec!["a".into(), "b".into()],
            ..GatewayBackup::default()
        };
        assert_eq!(gateway_identity(&first), gateway_identity(&second));
    }
}
