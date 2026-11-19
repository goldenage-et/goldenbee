//! Host-local lifecycle operations over the shared instance/installer crates.
//!
//! The composer owns privileged host actions. It resolves a verified package
//! digest inside its own cache, validates every host-local path against a
//! configured root, and passes the authorized configuration selection to the
//! shared Worker runtime through `instance`. It never accepts an arbitrary
//! executable or an unbounded filesystem path.

use crate::config::{Config, HostCaller};
use common::{
    ApiError, HostCompatibility, HostDescriptor, HostFinding, HostInstanceView, HostLimits,
    HostOperationOutcome, HostProfileSelection, HostProvisionRequest, HostRecoverySummary,
    HostRollbackRequest, HostTrustView, HostUpgradeRequest, HostUpgradeSummary, HostWorkPolicy,
    HOST_OPERATION_INSPECT, HOST_OPERATION_PROVISION, HOST_OPERATION_ROLLBACK,
    HOST_OPERATION_UPGRADE, HOST_PROTOCOL_VERSION,
};
use install::{Installer, TrustPolicy};
use instance::upgrade::{self, UpgradeRequest, WorkPolicy};
use instance::{CreateRequest, Inspection, InstanceError, InstanceLimits};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const OPERATIONS_FILE: &str = "operations.json";

/// Immutable record of one lifecycle operation, retained host-side so a lost
/// acknowledgment or repeated command returns the original outcome.
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

/// One host composer bound to immutable host policy.
pub struct Composer {
    config: Config,
    store: Mutex<Store>,
}

impl Composer {
    /// Prepare the composer and load its durable operation journal.
    pub fn new(config: Config) -> Result<Self, ApiError> {
        let root = config.instances_root();
        std::fs::create_dir_all(&root).map_err(|_| {
            ApiError::new(
                "host.storage_unavailable",
                "host instance root could not be created",
            )
        })?;
        let store = Store::open(&root)?;
        Ok(Self {
            config,
            store: Mutex::new(store),
        })
    }

    /// Composer identity and supported operations.
    pub fn descriptor(&self) -> HostDescriptor {
        HostDescriptor {
            host: self.config.id.clone(),
            protocol: HOST_PROTOCOL_VERSION.into(),
            adapter_version: env!("CARGO_PKG_VERSION").into(),
            supported: self.config.operations.clone(),
            reachable: true,
            error: None,
        }
    }

    /// Authenticate one Gateway credential.
    pub fn authenticate(&self, token: Option<&str>) -> Result<HostCaller, ApiError> {
        let Some(token) = token else {
            return Err(ApiError::new(
                "host.unauthorized",
                "a host composer credential is required",
            ));
        };
        let caller = self
            .config
            .callers
            .iter()
            .find(|caller| caller.token == token)
            .cloned();
        let Some(caller) = caller else {
            return Err(ApiError::new(
                "host.unauthorized",
                "the host composer credential is not recognized",
            ));
        };
        Ok(caller)
    }

    fn authorize(
        &self,
        caller: &HostCaller,
        operation: &str,
        instance: &str,
    ) -> Result<(), ApiError> {
        if !self
            .config
            .operations
            .iter()
            .any(|supported| supported == operation)
        {
            return Err(ApiError::new(
                "host.unsupported",
                format!("host '{}' does not support '{operation}'", self.config.id),
            ));
        }
        let allowed_scope = if operation == HOST_OPERATION_INSPECT {
            caller
                .scopes
                .iter()
                .any(|scope| matches!(scope.as_str(), "instance:read" | "instance:admin" | "*"))
        } else {
            caller
                .scopes
                .iter()
                .any(|scope| matches!(scope.as_str(), "instance:admin" | "*"))
        };
        if !allowed_scope {
            return Err(ApiError::new(
                "host.scope_denied",
                "the host composer credential lacks the required instance scope",
            ));
        }
        if let Some(instances) = &caller.instances {
            if !instances.iter().any(|allowed| allowed == instance) {
                return Err(ApiError::new(
                    "host.scope_denied",
                    format!(
                        "host credential for '{}' may not administer instance '{instance}'",
                        caller.gateway
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Provision one verified instance and activate its authorized profile.
    pub fn provision(
        &self,
        caller: &HostCaller,
        request: &HostProvisionRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        self.authorize(caller, HOST_OPERATION_PROVISION, &request.instance)?;
        self.execute(
            &request.operation_id,
            HOST_OPERATION_PROVISION,
            &request.digest(),
            &request.instance,
            || self.provision_inner(request),
        )
    }

    /// Inspect one instance without privileged installation access.
    pub fn inspect(
        &self,
        caller: &HostCaller,
        instance: &str,
    ) -> Result<HostInstanceView, ApiError> {
        self.authorize(caller, HOST_OPERATION_INSPECT, instance)?;
        self.view(instance)
    }

    /// Managed upgrade with the local verification, permission, drain,
    /// backup, activation, and readiness semantics.
    pub fn upgrade(
        &self,
        caller: &HostCaller,
        instance: &str,
        request: &HostUpgradeRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        self.authorize(caller, HOST_OPERATION_UPGRADE, instance)?;
        self.execute(
            &request.operation_id,
            HOST_OPERATION_UPGRADE,
            &request.digest(),
            instance,
            || self.upgrade_inner(instance, request),
        )
    }

    /// Managed rollback from a recoverable activation journal.
    pub fn rollback(
        &self,
        caller: &HostCaller,
        instance: &str,
        request: &HostRollbackRequest,
    ) -> Result<HostOperationOutcome, ApiError> {
        self.authorize(caller, HOST_OPERATION_ROLLBACK, instance)?;
        self.execute(
            &request.operation_id,
            HOST_OPERATION_ROLLBACK,
            &request.digest(),
            instance,
            || self.rollback_inner(instance),
        )
    }

    /// One recorded operation, for explicit reconciliation.
    pub fn operation(
        &self,
        caller: &HostCaller,
        operation: &str,
    ) -> Result<HostOperationOutcome, ApiError> {
        let store = self.lock();
        let Some(record) = store.document.operations.get(operation) else {
            return Err(ApiError::new(
                "host.operation_not_found",
                "the host has no recorded lifecycle operation",
            ));
        };
        self.authorize(caller, &record.kind, &record.instance)?;
        if let Some(outcome) = &record.outcome {
            let mut outcome = outcome.clone();
            outcome.replayed = true;
            return Ok(outcome);
        }
        Err(record.error.clone().unwrap_or_else(|| {
            ApiError::new(
                "host.operation_unknown",
                "the host did not record a terminal outcome for this operation",
            )
        }))
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
                outcome.host = self.config.id.clone();
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

        let root = self.config.instances_root();
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
        instance::create(create).map_err(composer_error)?;

        if let Some(profile) = &request.profile {
            self.activate_profile(&request.instance, profile)?;
        }

        let inspection = instance::inspect(&root, &request.instance).map_err(composer_error)?;
        if inspection.process_health != "running" {
            instance::start_with_env(
                &root,
                &request.instance,
                request.addr.as_deref(),
                &self.process_env(),
            )
            .map_err(composer_error)?;
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
        let mut upgrade =
            UpgradeRequest::new(self.config.instances_root(), instance, release.directory);
        upgrade.authorized_permissions = request.authorized_permissions.clone();
        upgrade.work_policy = policy;
        upgrade.verify_input = request.verify_input.clone();
        upgrade.addr = request.addr.clone();
        upgrade.env = self.process_env();
        let outcome = upgrade::apply(&upgrade).map_err(composer_error)?;
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
            upgrade::recover_with_env(&self.config.instances_root(), instance, &self.process_env())
                .map_err(composer_error)?;
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
        let outcome = instance::profile(&self.config.instances_root(), instance, &args)
            .map_err(composer_error)?;
        if outcome.status != 0 {
            return Err(ApiError::new(
                "host.profile_failed",
                format!("profile activation failed with status {}", outcome.status),
            ));
        }
        Ok(())
    }

    fn view(&self, instance: &str) -> Result<HostInstanceView, ApiError> {
        let root = self.config.instances_root();
        let inspection = instance::inspect(&root, instance).map_err(composer_error)?;
        let compatibility = self.compatibility(&inspection);
        Ok(HostInstanceView {
            host: self.config.id.clone(),
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
        let installed = Installer::new(self.config.packages_root(), TrustPolicy::default())
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
            let root = self.config.root(root);
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

    /// Narrow control environment for the Worker process. The shared
    /// delegation secret authenticates Gateway control exchanges; provider
    /// secrets remain references in instance configuration.
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

fn composer_error(error: InstanceError) -> ApiError {
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
