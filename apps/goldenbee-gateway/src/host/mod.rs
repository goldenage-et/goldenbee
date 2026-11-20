//! Gateway-owned local host lifecycle.
//!
//! This module replaces the former `gb-composer` process. The Gateway resolves
//! verified packages in its own cache, validates every local path and grant
//! against host policy, and uses the shared `instance` crate for provisioning,
//! process lifecycle, upgrade, and rollback. There is no host credential and no
//! second HTTP service.

mod packages;
pub(crate) mod routes;

use crate::config::LocalHostConfig;
use common::{
    ApiError, HostCompatibility, HostDescriptor, HostFinding, HostInspection, HostInstanceView,
    HostLimits, HostOperationOutcome, HostProfileSelection, HostProvisionRequest,
    HostRecoverySummary, HostRollbackRequest, HostTrustView, HostUpgradeRequest,
    HostUpgradeSummary, HostWorkPolicy, InstanceLogs, PackageInstallOutcome, PackageInstallRequest,
    PackageList, HOST_OPERATION_INSPECT, HOST_OPERATION_PROVISION, HOST_OPERATION_RESTART,
    HOST_OPERATION_ROLLBACK, HOST_OPERATION_START, HOST_OPERATION_STOP, HOST_OPERATION_UPGRADE,
    HOST_PROTOCOL_VERSION,
};
use gateway_core::LocalHost;
use install::{Installer, TrustPolicy};
use instance::upgrade::{self, UpgradeRequest, WorkPolicy};
use instance::{CreateRequest, Inspection, InstanceError, InstanceLimits};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const OPERATIONS_FILE: &str = "operations.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OperationRecord {
    operation_id: String,
    kind: String,
    instance: String,
    digest: String,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<HostOperationOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<ApiError>,
    updated_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OperationDocument {
    #[serde(default)]
    operations: BTreeMap<String, OperationRecord>,
}

struct Store {
    path: PathBuf,
    document: OperationDocument,
}

impl Store {
    fn open(root: &Path) -> Result<Self, ApiError> {
        let path = root.join(OPERATIONS_FILE);
        let document = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                ApiError::new(
                    "host.storage_invalid",
                    "host operation journal is malformed",
                )
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                OperationDocument::default()
            }
            Err(_) => {
                return Err(ApiError::new(
                    "host.storage_unavailable",
                    "host operation journal could not be read",
                ))
            }
        };
        Ok(Self { path, document })
    }

    fn write(&self) -> Result<(), ApiError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| {
                ApiError::new(
                    "host.storage_unavailable",
                    "host operation journal directory could not be created",
                )
            })?;
        }
        let bytes = serde_json::to_vec(&self.document).map_err(|_| {
            ApiError::new(
                "host.storage_unavailable",
                "host operation journal could not be serialized",
            )
        })?;
        let temporary = self.path.with_extension("tmp");
        std::fs::write(&temporary, bytes).map_err(|_| {
            ApiError::new(
                "host.storage_unavailable",
                "host operation journal could not be written",
            )
        })?;
        std::fs::rename(&temporary, &self.path).map_err(|_| {
            ApiError::new(
                "host.storage_unavailable",
                "host operation journal could not be committed",
            )
        })
    }
}

/// One Gateway-owned local host bound to immutable policy.
///
/// Cloning shares the same operation journal; callers use clones to move local
/// lifecycle work onto Tokio's blocking pool.
#[derive(Clone)]
pub struct HostManager {
    config: LocalHostConfig,
    base_dir: Option<PathBuf>,
    store: std::sync::Arc<Mutex<Store>>,
}

impl HostManager {
    /// Prepare the local host and load its durable operation journal.
    pub fn new(config: LocalHostConfig, base_dir: Option<&Path>) -> Result<Self, ApiError> {
        if !config.enabled() {
            return Err(ApiError::new(
                "host.not_configured",
                "local host management is not configured",
            ));
        }
        let base_dir = base_dir.map(Path::to_path_buf);
        let root = config.instances_root(base_dir.as_deref()).ok_or_else(|| {
            ApiError::new("host.config_invalid", "instances_root is not configured")
        })?;
        let packages = config.packages_root(base_dir.as_deref()).ok_or_else(|| {
            ApiError::new("host.config_invalid", "packages_root is not configured")
        })?;
        std::fs::create_dir_all(&root).map_err(|_| {
            ApiError::new(
                "host.storage_unavailable",
                "host instance root could not be created",
            )
        })?;
        std::fs::create_dir_all(&packages).map_err(|_| {
            ApiError::new(
                "host.storage_unavailable",
                "host package cache could not be created",
            )
        })?;
        let store = Store::open(&root)?;
        Ok(Self {
            config,
            base_dir,
            store: std::sync::Arc::new(Mutex::new(store)),
        })
    }

    /// Stable local host identity.
    pub fn host_id(&self) -> &str {
        &self.config.host_id
    }

    /// Configured package cache root.
    pub fn packages_root(&self) -> PathBuf {
        self.config
            .packages_root(self.base_dir.as_deref())
            .unwrap_or_default()
    }

    /// Configured instance root.
    pub fn instances_root(&self) -> PathBuf {
        self.config
            .instances_root(self.base_dir.as_deref())
            .unwrap_or_default()
    }

    /// Host descriptor used by compatibility routes and diagnostics.
    pub fn descriptor(&self) -> HostDescriptor {
        HostDescriptor {
            host: self.config.host_id.clone(),
            protocol: HOST_PROTOCOL_VERSION.into(),
            adapter_version: env!("CARGO_PKG_VERSION").into(),
            supported: self.config.operations.clone(),
            reachable: true,
            error: None,
        }
    }

    /// Optional local instance allow-list.
    pub fn allowed_instances(&self) -> Option<Vec<String>> {
        self.config.allowed_instances.clone()
    }

    /// Optional local operation allow-list.
    pub fn allowed_operations(&self) -> Option<Vec<String>> {
        Some(self.config.operations.clone())
    }

    /// Provision one verified instance and activate its authorized profile.
    pub fn provision_instance(
        &self,
        request: HostProvisionRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        self.ensure_operation(HOST_OPERATION_PROVISION)?;
        self.ensure_instance(&request.instance)?;
        self.execute(
            &request.operation_id,
            HOST_OPERATION_PROVISION,
            &request.digest(),
            &request.instance,
            || self.provision_inner(&request),
        )
    }

    /// Inspect one instance without privileged installation access.
    pub fn inspect_instance(&self, instance: &str) -> Result<HostInstanceView, ApiError> {
        self.ensure_operation(HOST_OPERATION_INSPECT)?;
        self.ensure_instance(instance)?;
        self.view(instance)
    }

    /// Managed upgrade with local verification, permissions, drain, backup,
    /// activation, and readiness semantics.
    pub fn upgrade_instance(
        &self,
        instance: &str,
        request: &HostUpgradeRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        self.ensure_operation(HOST_OPERATION_UPGRADE)?;
        self.ensure_instance(instance)?;
        self.execute(
            &request.operation_id,
            HOST_OPERATION_UPGRADE,
            &request.digest(),
            instance,
            || self.upgrade_inner(instance, request),
        )
    }

    /// Managed rollback from a recoverable activation journal.
    pub fn rollback_instance(
        &self,
        instance: &str,
        request: &HostRollbackRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        self.ensure_operation(HOST_OPERATION_ROLLBACK)?;
        self.ensure_instance(instance)?;
        self.execute(
            &request.operation_id,
            HOST_OPERATION_ROLLBACK,
            &request.digest(),
            instance,
            || self.rollback_inner(instance),
        )
    }

    /// Start one stopped instance. A running instance converges to its view.
    pub fn start_instance(&self, instance: &str) -> Result<HostInstanceView, ApiError> {
        self.ensure_operation(HOST_OPERATION_START)?;
        self.ensure_instance(instance)?;
        let root = self.instances_root();
        let inspection = instance::inspect(&root, instance).map_err(host_error)?;
        if inspection.process_health != "running" {
            match instance::start_with_env(&root, instance, None, &self.process_env()) {
                Ok(_) => {}
                Err(error) if error.code == "instance.already_running" => {}
                Err(error) => return Err(host_error(error)),
            }
        }
        self.view(instance)
    }

    /// Stop one running instance.
    pub fn stop_instance(&self, instance: &str) -> Result<HostInstanceView, ApiError> {
        self.ensure_operation(HOST_OPERATION_STOP)?;
        self.ensure_instance(instance)?;
        instance::stop(&self.instances_root(), instance).map_err(host_error)?;
        self.view(instance)
    }

    /// Restart one instance, preserving its previous listener when possible.
    pub fn restart_instance(&self, instance: &str) -> Result<HostInstanceView, ApiError> {
        self.ensure_operation(HOST_OPERATION_RESTART)?;
        self.ensure_instance(instance)?;
        let root = self.instances_root();
        let inspection = instance::inspect(&root, instance).map_err(host_error)?;
        let previous = inspection.addr.clone();
        if inspection.process_health == "running" {
            instance::stop(&root, instance).map_err(host_error)?;
        }
        let mut started = match previous.as_deref() {
            Some(addr) => {
                instance::start_with_env(&root, instance, Some(addr), &self.process_env())
            }
            None => instance::start_with_env(&root, instance, None, &self.process_env()),
        };
        if started.is_err() && previous.is_some() {
            started = instance::start_with_env(&root, instance, None, &self.process_env());
        }
        started.map_err(host_error)?;
        self.view(instance)
    }

    /// Bounded tail of one instance process log.
    pub fn instance_logs(&self, instance: &str, tail: usize) -> Result<InstanceLogs, ApiError> {
        self.ensure_operation(HOST_OPERATION_INSPECT)?;
        self.ensure_instance(instance)?;
        instance::inspect(&self.instances_root(), instance).map_err(host_error)?;
        let path = self.instances_root().join(instance).join("process.log");
        let text = std::fs::read_to_string(&path).map_err(|_| {
            ApiError::new("host.logs_unavailable", "the instance log is not readable")
        })?;
        let lines: Vec<&str> = text.lines().collect();
        let tail = tail.clamp(1, 1000);
        let start = lines.len().saturating_sub(tail);
        Ok(InstanceLogs {
            instance: instance.into(),
            lines: lines[start..]
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
            truncated: start > 0,
        })
    }

    /// Every valid instance in the local root.
    pub fn list_instances(&self) -> Result<Vec<HostInstanceView>, ApiError> {
        let root = self.instances_root();
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => {
                return Err(ApiError::new(
                    "host.storage_unavailable",
                    "host instance root is unreadable",
                ))
            }
        };
        let mut instances = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| {
                ApiError::new(
                    "host.storage_unavailable",
                    "host instance root is unreadable",
                )
            })?;
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(".staging-") {
                continue;
            }
            instances.push(self.view(&name)?);
        }
        instances.sort_by(|left, right| left.instance.cmp(&right.instance));
        Ok(instances)
    }

    /// Install one verified package from a local folder or URL archive.
    pub async fn install_package(
        &self,
        request: PackageInstallRequest,
    ) -> Result<PackageInstallOutcome, ApiError> {
        packages::install(self, request).await
    }

    /// List verified packages in the local cache.
    pub fn list_packages(&self) -> Result<PackageList, ApiError> {
        packages::list(self)
    }

    /// Load the local trust policy, if configured.
    pub(crate) fn trust_policy(&self) -> Result<TrustPolicy, ApiError> {
        let path = self
            .config
            .trust_policy(self.base_dir.as_deref())
            .ok_or_else(|| {
                ApiError::new(
                    "host.trust_policy_unavailable",
                    "this Gateway has no configured trust policy",
                )
            })?;
        TrustPolicy::load(&path).map_err(install_error)
    }

    /// Resolve a folder source under the configured package source roots.
    pub(crate) fn package_source(&self, path: &str) -> Result<PathBuf, ApiError> {
        if self.config.allowed_package_sources.is_empty() {
            return Err(ApiError::new(
                "host.package_source_forbidden",
                "this Gateway does not accept folder package sources",
            ));
        }
        self.contained(
            path,
            &self.config.allowed_package_sources,
            "host.package_source_forbidden",
        )
    }

    fn ensure_operation(&self, operation: &str) -> Result<(), ApiError> {
        if self
            .config
            .operations
            .iter()
            .any(|supported| supported == operation)
        {
            Ok(())
        } else {
            Err(ApiError::new(
                "host.unsupported",
                format!(
                    "host '{}' does not support '{operation}'",
                    self.config.host_id
                ),
            ))
        }
    }

    fn ensure_instance(&self, instance: &str) -> Result<(), ApiError> {
        if let Some(instances) = &self.config.allowed_instances {
            if !instances.iter().any(|allowed| allowed == instance) {
                return Err(ApiError::new(
                    "host.scope_denied",
                    format!(
                        "host '{}' may not administer instance '{instance}'",
                        self.config.host_id
                    ),
                ));
            }
        }
        Ok(())
    }

    fn execute(
        &self,
        operation_id: &str,
        kind: &str,
        digest: &str,
        instance: &str,
        run: impl FnOnce() -> Result<HostOperationOutcome, ApiError>,
    ) -> Result<HostOperationOutcome, ApiError> {
        {
            let store = self.lock();
            if let Some(existing) = store.document.operations.get(operation_id) {
                if existing.digest != digest {
                    return Err(ApiError::new(
                        "host.operation_conflict",
                        "this lifecycle operation id was already used with a different request",
                    ));
                }
                if let Some(outcome) = &existing.outcome {
                    let mut outcome = outcome.clone();
                    outcome.replayed = true;
                    return Ok(outcome);
                }
                if let Some(error) = &existing.error {
                    return Err(error.clone());
                }
            }
        }

        let result = run();
        let mut store = self.lock();
        let now = common::now_ms();
        match result {
            Ok(mut outcome) => {
                outcome.operation_id = operation_id.into();
                outcome.kind = kind.into();
                outcome.host = self.config.host_id.clone();
                outcome.instance = instance.into();
                store.document.operations.insert(
                    operation_id.to_string(),
                    OperationRecord {
                        operation_id: operation_id.into(),
                        kind: kind.into(),
                        instance: instance.into(),
                        digest: digest.into(),
                        status: outcome.status.clone(),
                        outcome: Some(outcome.clone()),
                        error: None,
                        updated_at: now,
                    },
                );
                store.write()?;
                Ok(outcome)
            }
            Err(error) => {
                store.document.operations.insert(
                    operation_id.to_string(),
                    OperationRecord {
                        operation_id: operation_id.into(),
                        kind: kind.into(),
                        instance: instance.into(),
                        digest: digest.into(),
                        status: if error.retryable {
                            "unknown".into()
                        } else {
                            "failed".into()
                        },
                        outcome: None,
                        error: Some(error.clone()),
                        updated_at: now,
                    },
                );
                store.write()?;
                Err(error)
            }
        }
    }

    fn provision_inner(
        &self,
        request: &HostProvisionRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        let release = self.release(&request.release_digest)?;
        self.validate_grants(
            &request.grants,
            &self.config.allowed_grants,
            "host.grant_forbidden",
        )?;
        for reference in &request.secret_refs {
            if !self
                .config
                .allowed_secret_prefixes
                .iter()
                .any(|prefix| !prefix.is_empty() && reference.starts_with(prefix))
            {
                return Err(ApiError::new(
                    "host.secret_forbidden",
                    "the requested credential reference is not permitted by this host",
                ));
            }
        }
        let config = match &request.config {
            Some(path) => {
                Some(self.config_path(path, &request.instance, request.control_token.as_deref())?)
            }
            None => None,
        };
        let workspace = match &request.workspace {
            Some(path) => Some(self.workspace_path(path)?),
            None => None,
        };
        if let Some(addr) = &request.addr {
            self.validate_listener(addr)?;
        }
        let token = request
            .control_token
            .clone()
            .filter(|token| !token.trim().is_empty());

        let root = self.instances_root();
        let mut create = CreateRequest::new(&root, &request.instance, release.directory);
        create.grants = request.grants.clone();
        create.workspace = workspace;
        create.read_only = request.read_only;
        create.limits = InstanceLimits {
            max_active: request.limits.max_active,
            max_queue: request.limits.max_queue,
            per_principal: request.limits.per_principal,
        };
        create.secret_refs = request.secret_refs.clone();
        create.config = config;
        create.token = token;
        instance::create(create).map_err(host_error)?;

        if let Some(profile) = &request.profile {
            self.activate_profile(&request.instance, profile)?;
        }

        let inspection = instance::inspect(&root, &request.instance).map_err(host_error)?;
        if inspection.process_health != "running" {
            match instance::start_with_env(
                &root,
                &request.instance,
                request.addr.as_deref(),
                &self.process_env(),
            ) {
                Ok(_) => {}
                Err(error) if error.code == "instance.already_running" => {}
                Err(error) => return Err(host_error(error)),
            }
        }
        let view = self.view(&request.instance)?;
        Ok(HostOperationOutcome {
            operation_id: String::new(),
            kind: String::new(),
            host: String::new(),
            instance: String::new(),
            status: "succeeded".into(),
            view: Some(view),
            worker: None,
            upgrade: None,
            recovery: None,
            replayed: false,
        })
    }

    fn upgrade_inner(
        &self,
        instance: &str,
        request: &HostUpgradeRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        let release = self.release(&request.release_digest)?;
        let policy = match request.work_policy {
            HostWorkPolicy::Drain => WorkPolicy::Drain,
            HostWorkPolicy::Pin => WorkPolicy::Pin,
            HostWorkPolicy::Migrate => WorkPolicy::Migrate,
        };
        let mut upgrade = UpgradeRequest::new(self.instances_root(), instance, release.directory);
        upgrade.authorized_permissions = request.authorized_permissions.clone();
        upgrade.work_policy = policy;
        upgrade.verify_input = request.verify_input.clone();
        upgrade.addr = request.addr.clone();
        upgrade.env = self.process_env();
        let outcome = upgrade::apply(&upgrade).map_err(host_error)?;
        let report = outcome.report;
        Ok(HostOperationOutcome {
            operation_id: String::new(),
            kind: String::new(),
            host: String::new(),
            instance: String::new(),
            status: "succeeded".into(),
            view: Some(self.view(instance)?),
            worker: None,
            upgrade: Some(HostUpgradeSummary {
                current_digest: report.current.artifact_digest,
                candidate_digest: report.candidate.artifact_digest,
                permissions_added: report.permissions.added,
                permissions_removed: report.permissions.removed,
                blocked: report
                    .blocked
                    .into_iter()
                    .map(|finding| HostFinding {
                        code: finding.code,
                        message: finding.message,
                    })
                    .collect(),
                authorization_satisfied: report.authorization.satisfied,
                config_preserved: report.config.preflight_ok,
                schema_status: report.schema_change.status,
                work_action: report.work.action,
                launcher_modes: report.launcher.verified,
            }),
            recovery: None,
            replayed: false,
        })
    }

    fn rollback_inner(&self, instance: &str) -> Result<HostOperationOutcome, ApiError> {
        let outcome =
            upgrade::recover_with_env(&self.instances_root(), instance, &self.process_env())
                .map_err(host_error)?;
        let quarantine = outcome.quarantine.is_some();
        Ok(HostOperationOutcome {
            operation_id: String::new(),
            kind: String::new(),
            host: String::new(),
            instance: String::new(),
            status: "recovered".into(),
            view: Some(self.view(instance)?),
            worker: None,
            upgrade: None,
            recovery: Some(HostRecoverySummary {
                binary_restored: outcome.binary_restored,
                data_restored: outcome.data_restored,
                quarantine,
                journal_status: outcome.journal.status,
            }),
            replayed: false,
        })
    }

    fn activate_profile(
        &self,
        instance: &str,
        profile: &HostProfileSelection,
    ) -> Result<(), ApiError> {
        if profile.name.trim().is_empty() {
            return Err(ApiError::new(
                "host.profile_invalid",
                "profile name must not be empty",
            ));
        }
        self.validate_grants(
            &profile.grants,
            &self.config.allowed_profile_grants,
            "host.grant_forbidden",
        )?;
        let mut args = vec![
            if profile.create { "create" } else { "activate" }.to_string(),
            profile.name.clone(),
        ];
        if profile.create {
            if let Some(instruction) = &profile.instruction {
                args.push("--instruction".into());
                args.push(instruction.clone());
            }
            for member in &profile.members {
                args.push("--member".into());
                args.push(member.clone());
            }
            for capability in &profile.capabilities {
                args.push("--capability".into());
                args.push(capability.clone());
            }
            for grant in &profile.grants {
                args.push("--grant".into());
                args.push(grant.clone());
            }
            args.push("--activate".into());
        }
        let outcome =
            instance::profile(&self.instances_root(), instance, &args).map_err(host_error)?;
        if outcome.status != 0 {
            eprintln!(
                "host.profile_failed instance={instance} status={} stderr={}",
                outcome.status,
                common::redact(outcome.stderr.trim())
            );
            return Err(ApiError::new(
                "host.profile_failed",
                format!("profile activation failed with status {}", outcome.status),
            ));
        }
        Ok(())
    }

    fn view(&self, instance: &str) -> Result<HostInstanceView, ApiError> {
        let root = self.instances_root();
        let inspection = instance::inspect(&root, instance).map_err(host_error)?;
        let compatibility = self.compatibility(&inspection);
        Ok(HostInstanceView {
            host: self.config.host_id.clone(),
            instance: inspection.instance,
            package: inspection.package,
            version: inspection.version,
            artifact_digest: inspection.artifact_digest,
            binary_digest: inspection.binary_digest,
            signer: inspection.signer,
            target: TrustPolicy::host_target(),
            protocol: compatibility.protocol.clone(),
            lifecycle: inspection.lifecycle,
            process_health: inspection.process_health,
            task_activity: inspection.task_activity,
            runtime_phase: inspection.runtime_phase,
            incarnation: inspection.incarnation,
            active_profile: inspection.active_profile,
            endpoint: inspection.addr.map(|addr| format!("http://{addr}")),
            grants: inspection.grants,
            limits: HostLimits {
                max_active: inspection.limits.max_active,
                max_queue: inspection.limits.max_queue,
                per_principal: inspection.limits.per_principal,
            },
            workspace_write: inspection.workspace_write,
            secret_refs: inspection.secret_refs,
            operator_config: inspection.operator_config,
            trust: inspection.trust.map(|trust| HostTrustView {
                decision: trust.decision.as_str().into(),
                reason: trust.reason,
            }),
            compatibility,
        })
    }

    fn compatibility(&self, inspection: &Inspection) -> HostCompatibility {
        let mut issues = Vec::new();
        let metadata = std::fs::read(inspection.release_dir.join("metadata.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<common::ReleaseMetadata>(&bytes).ok());
        let (target, protocol, protocol_min, protocol_max, runtime, adk) = match metadata {
            Some(metadata) => (
                metadata.target,
                metadata.package.protocol,
                metadata.protocol_min,
                metadata.protocol_max,
                metadata.package.runtime,
                metadata.package.adk,
            ),
            None => {
                issues.push("installed release metadata is not readable".into());
                (
                    TrustPolicy::host_target(),
                    common::PROTOCOL_VERSION.into(),
                    common::PROTOCOL_VERSION.into(),
                    common::PROTOCOL_VERSION.into(),
                    String::new(),
                    String::new(),
                )
            }
        };
        if target != TrustPolicy::host_target() {
            issues.push(format!(
                "release target '{target}' is not supported by this host"
            ));
        }
        if !common::protocol_in_range(common::PROTOCOL_VERSION, &protocol_min, &protocol_max) {
            issues.push(format!(
                "release protocol '{protocol}' is not compatible with host protocol {}",
                common::PROTOCOL_VERSION
            ));
        }
        let compatible = issues.is_empty();
        HostCompatibility {
            target,
            protocol,
            runtime,
            adk,
            compatible,
            issues,
        }
    }

    fn release(&self, digest: &str) -> Result<install::Installed, ApiError> {
        if !install::is_digest(digest) {
            return Err(ApiError::new(
                "host.release_invalid",
                "release must be a sha256 digest of a verified local package",
            ));
        }
        let installed = Installer::new(self.packages_root(), TrustPolicy::default())
            .installed()
            .map_err(|_| {
                ApiError::new(
                    "host.release_unavailable",
                    "the host verified package cache is unreadable",
                )
            })?;
        installed
            .into_iter()
            .find(|release| release.artifact_digest == digest)
            .ok_or_else(|| {
                ApiError::new(
                    "host.release_unavailable",
                    "the requested digest is not present in the host verified package cache",
                )
            })
    }

    fn config_path(
        &self,
        path: &str,
        instance: &str,
        token: Option<&str>,
    ) -> Result<PathBuf, ApiError> {
        if self.config.allowed_config_roots.is_empty() {
            return Err(ApiError::new(
                "host.config_forbidden",
                "this host does not accept an operator configuration selection",
            ));
        }
        let candidate = self.contained(
            path,
            &self.config.allowed_config_roots,
            "host.config_forbidden",
        )?;
        let text = std::fs::read_to_string(&candidate).map_err(|_| {
            ApiError::new(
                "host.config_invalid",
                "the selected host configuration file is unreadable",
            )
        })?;
        let value: toml::Value = toml::from_str(&text).map_err(|_| {
            ApiError::new(
                "host.config_invalid",
                "the selected host configuration file is not valid TOML",
            )
        })?;
        if let Some(configured) = value.get("instance").and_then(toml::Value::as_str) {
            if configured != instance {
                return Err(ApiError::new(
                    "host.config_forbidden",
                    "the selected configuration names a different instance identity",
                ));
            }
        }
        if let Some(token) = token {
            if let Some(configured) = value.get("token").and_then(toml::Value::as_str) {
                if configured != token {
                    return Err(ApiError::new(
                        "host.config_forbidden",
                        "the selected configuration binds a different Worker control token",
                    ));
                }
            }
        }
        Ok(candidate)
    }

    fn workspace_path(&self, path: &str) -> Result<PathBuf, ApiError> {
        if self.config.allowed_workspace_roots.is_empty() {
            return Err(ApiError::new(
                "host.workspace_forbidden",
                "this host does not accept an external workspace selection",
            ));
        }
        self.contained(
            path,
            &self.config.allowed_workspace_roots,
            "host.workspace_forbidden",
        )
    }

    fn contained(&self, path: &str, roots: &[String], code: &str) -> Result<PathBuf, ApiError> {
        let candidate = std::fs::canonicalize(path).map_err(|_| {
            ApiError::new(
                code,
                "the selected host path does not exist or is unreadable",
            )
        })?;
        for root in roots {
            let root = self.config.root(root, self.base_dir.as_deref());
            if let Ok(root) = std::fs::canonicalize(&root) {
                if candidate.starts_with(&root) {
                    return Ok(candidate);
                }
            }
        }
        Err(ApiError::new(
            code,
            "the selected host path is outside the authorized roots",
        ))
    }

    fn validate_grants(
        &self,
        requested: &[String],
        allowed: &[String],
        code: &str,
    ) -> Result<(), ApiError> {
        for grant in requested {
            if !allowed.iter().any(|allowed| allowed == grant) {
                return Err(ApiError::new(
                    code,
                    format!("grant '{grant}' is not permitted by this host"),
                ));
            }
        }
        Ok(())
    }

    fn validate_listener(&self, addr: &str) -> Result<(), ApiError> {
        if self.config.allow_remote_listener {
            return Ok(());
        }
        let local = addr.starts_with("127.0.0.1:")
            || addr.starts_with("localhost:")
            || addr.starts_with("[::1]:");
        if local {
            Ok(())
        } else {
            Err(ApiError::new(
                "host.listener_forbidden",
                "this host requires a loopback Worker listener",
            ))
        }
    }

    fn process_env(&self) -> Vec<(String, String)> {
        match std::env::var("GOLDENBEE_DELEGATION_SECRET") {
            Ok(secret) if !secret.is_empty() => {
                vec![("GOLDENBEE_DELEGATION_SECRET".into(), secret)]
            }
            _ => Vec::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[async_trait::async_trait]
impl LocalHost for HostManager {
    fn id(&self) -> &str {
        self.host_id()
    }

    fn descriptor(&self) -> HostDescriptor {
        HostManager::descriptor(self)
    }

    fn allowed_instances(&self) -> Option<Vec<String>> {
        HostManager::allowed_instances(self)
    }

    fn allowed_operations(&self) -> Option<Vec<String>> {
        HostManager::allowed_operations(self)
    }

    async fn provision(
        &self,
        request: HostProvisionRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        let host = self.clone();
        blocking(move || host.provision_instance(request)).await
    }

    async fn inspect(&self, instance: &str) -> Result<HostInspection, ApiError> {
        let host = self.clone();
        let instance = instance.to_string();
        blocking(move || {
            Ok(HostInspection {
                host: host.config.host_id.clone(),
                view: host.inspect_instance(&instance)?,
                worker: None,
            })
        })
        .await
    }

    async fn upgrade(
        &self,
        instance: &str,
        request: HostUpgradeRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        let host = self.clone();
        let instance = instance.to_string();
        blocking(move || host.upgrade_instance(&instance, &request)).await
    }

    async fn rollback(
        &self,
        instance: &str,
        request: HostRollbackRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        let host = self.clone();
        let instance = instance.to_string();
        blocking(move || host.rollback_instance(&instance, &request)).await
    }
}

async fn blocking<T, F>(run: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
{
    tokio::task::spawn_blocking(run).await.map_err(|_| {
        ApiError::new(
            "host.operation_failed",
            "the local host operation task did not complete",
        )
    })?
}

fn host_error(error: InstanceError) -> ApiError {
    match error.code.as_str() {
        "upgrade.quarantined" => ApiError::new(
            "upgrade.quarantined",
            "interrupted activation could not restore persisted data; the host quarantined the failed data",
        ),
        "upgrade.no_recovery" | "upgrade.nothing_to_recover" => {
            ApiError::new(error.code, error.message)
        }
        "instance.not_found" => ApiError::new(
            "host.instance_not_found",
            "the host has no provisioned instance with this identity",
        ),
        _ => ApiError::new(error.code, error.message),
    }
}

pub(crate) fn install_error(error: install::InstallError) -> ApiError {
    ApiError::new(error.code, error.message)
}
