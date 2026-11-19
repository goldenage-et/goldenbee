//! Gateway control-plane configuration.
//!
//! This is the configuration for direct use of the `gateway-core` library.
//! Caller bearer tokens and listener addresses belong to a deployment adapter
//! such as `goldenbee-gateway`, never to the control plane itself.

use common::ApiError;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn default_data_dir() -> String {
    "gateway-state".to_string()
}

fn default_max_streams() -> usize {
    DEFAULT_MAX_STREAMS
}

const DEFAULT_LEASE_TTL_MS: i64 = 300_000;
const DEFAULT_MAX_STREAMS: usize = 256;
const DEFAULT_CLAIM_TTL_MS: i64 = 30_000;

/// Gateway control-plane configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct CoreConfig {
    /// Durable Gateway dispatch state. Relative paths resolve against
    /// `base_dir` when one is configured.
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    /// Approved Worker registrations.
    #[serde(default)]
    pub workers: Vec<WorkerConfig>,
    /// Authorized deployment host composers.
    #[serde(default)]
    pub hosts: Vec<HostConfig>,
    /// Path to persistent Gateway registry state. Omitted keeps registry state
    /// in memory for the process lifetime.
    #[serde(default)]
    pub registry: Option<String>,
    /// Gateway-measured lease duration in milliseconds.
    #[serde(default)]
    pub lease_ttl_ms: Option<i64>,
    /// Maximum concurrently forwarded task event streams.
    #[serde(default = "default_max_streams")]
    pub max_streams: usize,
    /// Maximum time a dispatch claim may remain in flight before another
    /// replica may reclaim it. Defaults to 30 seconds.
    #[serde(default)]
    pub claim_ttl_ms: Option<i64>,
    /// Stable identity for this Gateway replica. Shared stores record it on
    /// claims and lease renewals. Falls back to `GOLDENBEE_GATEWAY_REPLICA_ID`.
    #[serde(default)]
    pub replica_id: Option<String>,
    /// Durable store selection. Omitted keeps the standalone JSON file store.
    #[serde(default)]
    pub store: Option<StoreConfig>,
    /// Gateway-managed provider vault. Omitted leaves provider management
    /// disabled and preserves the existing environment-provider behavior.
    #[serde(default)]
    pub vault: Option<VaultConfig>,
    /// Delegation signing secret. Prefer `GOLDENBEE_DELEGATION_SECRET`; this
    /// field exists for isolated test deployments.
    #[serde(default)]
    pub delegation_secret: Option<String>,
    /// Directory that relative durable paths resolve against. An adapter that
    /// loads a configuration file sets this to the file's directory; a business
    /// backend using the core directly may leave it unset and supply absolute
    /// paths.
    #[serde(skip)]
    pub base_dir: Option<PathBuf>,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            workers: Vec::new(),
            hosts: Vec::new(),
            registry: None,
            lease_ttl_ms: None,
            max_streams: default_max_streams(),
            claim_ttl_ms: None,
            replica_id: None,
            store: None,
            vault: None,
            delegation_secret: None,
            base_dir: None,
        }
    }
}

/// Gateway-managed provider catalog and encrypted credential vault.
#[derive(Debug, Clone, Deserialize)]
pub struct VaultConfig {
    /// Whether provider management is enabled. Presence enables it unless this
    /// is explicitly false.
    #[serde(default = "default_vault_enabled")]
    pub enabled: bool,
    /// Directory holding the encrypted vault document.
    #[serde(default = "default_vault_dir")]
    pub directory: String,
    /// Environment variable holding the base64 32-byte master key. The value is
    /// never read from Gateway JSON.
    #[serde(default = "default_master_key_env")]
    pub master_key_env: String,
}

fn default_vault_enabled() -> bool {
    true
}

fn default_vault_dir() -> String {
    "gateway-vault".into()
}

fn default_master_key_env() -> String {
    "GOLDENBEE_VAULT_MASTER_KEY".into()
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            enabled: default_vault_enabled(),
            directory: default_vault_dir(),
            master_key_env: default_master_key_env(),
        }
    }
}

/// Durable store configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct StoreConfig {
    /// `file` (default) or `postgres`.
    #[serde(default = "default_store_kind")]
    pub kind: String,
    /// Environment variable holding the Postgres DSN. A literal DSN is never
    /// read from the operator JSON.
    #[serde(default = "default_store_url_env")]
    pub url_env: String,
    /// Connection pool ceiling for the shared adapter.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// Apply migrations on startup. Refuse a newer schema regardless.
    #[serde(default = "default_migrate")]
    pub migrate: bool,
}

fn default_store_kind() -> String {
    "file".into()
}

fn default_store_url_env() -> String {
    "GOLDENBEE_GATEWAY_DATABASE_URL".into()
}

fn default_max_connections() -> u32 {
    8
}

fn default_migrate() -> bool {
    true
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            kind: default_store_kind(),
            url_env: default_store_url_env(),
            max_connections: default_max_connections(),
            migrate: default_migrate(),
        }
    }
}

impl StoreConfig {
    /// Whether this selection is the shared Postgres adapter.
    pub fn is_postgres(&self) -> bool {
        self.kind == "postgres"
    }

    /// Resolve the DSN from the configured environment variable. Errors never
    /// echo the value.
    pub fn resolve_url(&self) -> Result<String, ApiError> {
        std::env::var(&self.url_env).map_err(|_| {
            ApiError::new(
                "config.invalid",
                format!(
                    "shared store DSN environment variable '{}' is not set",
                    self.url_env
                ),
            )
            .with(
                "hint",
                "set the configured environment variable in the Gateway environment file",
            )
        })
    }
}

/// One authorized deployment host composer.
#[derive(Debug, Clone, Deserialize)]
pub struct HostConfig {
    /// Stable host identity selected by callers and lifecycle records.
    pub id: String,
    /// Host composer base URL.
    pub endpoint: String,
    /// Bearer credential used only for this host composer. Never logged.
    pub token: String,
    /// Instances this Gateway may administer on this host. Omitted permits any
    /// instance the adapter itself authorizes.
    #[serde(default)]
    pub instances: Option<Vec<String>>,
    /// Operations this Gateway may request. Omitted permits every operation
    /// the adapter advertises.
    #[serde(default)]
    pub operations: Option<Vec<String>>,
}

/// One approved Worker identity and its control-token binding.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerConfig {
    /// Gateway-assigned identity used by callers.
    pub id: String,
    /// Worker base URL.
    pub endpoint: String,
    /// Expected Worker process identity. Registration confirms it.
    #[serde(default)]
    pub instance: Option<String>,
    /// Expected package name. Registration confirms it.
    #[serde(default)]
    pub package: Option<String>,
    /// Approved capability identifiers. Omitted approves the verified card.
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    /// Bearer token used only by this Worker for authenticated registration and
    /// lease renewal. Never logged or exposed by discovery.
    #[serde(default)]
    pub control_token: Option<String>,
    /// Verify the endpoint during Gateway startup when no registry record
    /// exists. Disabled entries stay unreachable until they register.
    #[serde(default)]
    pub preflight: Option<bool>,
}

impl CoreConfig {
    /// Resolve the durable dispatch directory. Relative paths are anchored to
    /// `base_dir`, not the process working directory.
    pub fn data_dir(&self) -> PathBuf {
        resolve(&self.data_dir, self.base_dir.as_deref())
    }

    /// Resolve the persistent registry path.
    pub fn registry_path(&self) -> Option<PathBuf> {
        self.registry
            .as_deref()
            .map(|registry| resolve(registry, self.base_dir.as_deref()))
    }

    /// Gateway-measured lease duration.
    pub fn lease_ttl_ms(&self) -> i64 {
        self.lease_ttl_ms.unwrap_or(DEFAULT_LEASE_TTL_MS)
    }

    /// Maximum dispatch claim duration.
    pub fn claim_ttl_ms(&self) -> i64 {
        self.claim_ttl_ms.unwrap_or(DEFAULT_CLAIM_TTL_MS)
    }

    /// Selected durable store. Omitted means the standalone file store.
    pub fn store(&self) -> StoreConfig {
        self.store.clone().unwrap_or_default()
    }

    /// Resolve the vault directory. Relative paths anchor to `base_dir`.
    pub fn vault_dir(&self) -> Option<PathBuf> {
        self.vault
            .as_ref()
            .map(|vault| resolve(&vault.directory, self.base_dir.as_deref()))
    }

    /// Whether the provider vault is configured and enabled.
    pub fn vault_enabled(&self) -> bool {
        self.vault.as_ref().is_some_and(|vault| vault.enabled)
    }

    /// Validate the control-plane configuration.
    pub fn validate(&self) -> Result<(), ApiError> {
        let mut ids = BTreeSet::new();
        let mut control = BTreeSet::new();
        let mut host_ids = BTreeSet::new();
        let mut host_endpoints = BTreeSet::new();
        let mut host_tokens = BTreeSet::new();
        for host in &self.hosts {
            if host.id.trim().is_empty() || host.id.len() > 128 {
                return Err(ApiError::new(
                    "config.invalid",
                    "host ids must be 1-128 characters",
                ));
            }
            if !host.endpoint.starts_with("http://") && !host.endpoint.starts_with("https://") {
                return Err(ApiError::new(
                    "config.invalid",
                    "host composer endpoints must be HTTP or HTTPS base URLs",
                ));
            }
            if host.token.trim().is_empty() {
                return Err(ApiError::new(
                    "config.invalid",
                    "host composer credentials must not be empty",
                ));
            }
            if !host_ids.insert(host.id.as_str()) {
                return Err(ApiError::new("config.invalid", "host ids must be unique"));
            }
            if !host_endpoints.insert(host.endpoint.trim_end_matches('/')) {
                return Err(ApiError::new(
                    "config.invalid",
                    "host composer endpoints must be unique",
                ));
            }
            if !host_tokens.insert(host.token.as_str()) {
                return Err(ApiError::new(
                    "config.invalid",
                    "each host credential may be bound to only one adapter",
                ));
            }
            if let Some(operations) = &host.operations {
                for operation in operations {
                    if !common::HOST_OPERATIONS.contains(&operation.as_str()) {
                        return Err(ApiError::new(
                            "config.invalid",
                            format!("unknown host operation '{operation}'"),
                        ));
                    }
                }
            }
        }
        for worker in &self.workers {
            if worker.id.trim().is_empty() {
                return Err(ApiError::new(
                    "config.invalid",
                    "Worker registrations require a non-empty id",
                ));
            }
            if !worker.endpoint.starts_with("http://") && !worker.endpoint.starts_with("https://") {
                return Err(ApiError::new(
                    "config.invalid",
                    "Worker endpoints must be HTTP or HTTPS base URLs",
                ));
            }
            if let Some(token) = &worker.control_token {
                if token.trim().is_empty() {
                    return Err(ApiError::new(
                        "config.invalid",
                        "Worker control tokens must not be empty",
                    ));
                }
                if !control.insert(token.as_str()) {
                    return Err(ApiError::new(
                        "config.invalid",
                        "each Worker control token may be bound only once",
                    ));
                }
            }
            if !ids.insert(worker.id.as_str()) {
                return Err(ApiError::new("config.invalid", "Worker ids must be unique"));
            }
        }
        if self.lease_ttl_ms.is_some_and(|ttl| ttl < 1) {
            return Err(ApiError::new(
                "config.invalid",
                "lease_ttl_ms must be a positive duration",
            ));
        }
        if self.max_streams == 0 {
            return Err(ApiError::new(
                "config.invalid",
                "max_streams must be at least one",
            ));
        }
        if self.claim_ttl_ms.is_some_and(|ttl| ttl < 1) {
            return Err(ApiError::new(
                "config.invalid",
                "claim_ttl_ms must be a positive duration",
            ));
        }
        if let Some(vault) = &self.vault {
            if vault.enabled {
                if vault.directory.trim().is_empty() {
                    return Err(ApiError::new(
                        "config.invalid",
                        "vault.directory must not be empty",
                    ));
                }
                if vault.master_key_env.trim().is_empty() {
                    return Err(ApiError::new(
                        "config.invalid",
                        "vault.master_key_env must name an environment variable",
                    ));
                }
            }
        }
        if let Some(store) = &self.store {
            if !matches!(store.kind.as_str(), "file" | "postgres") {
                return Err(ApiError::new(
                    "config.invalid",
                    "store.kind must be 'file' or 'postgres'",
                ));
            }
            if store.kind == "postgres" && store.url_env.trim().is_empty() {
                return Err(ApiError::new(
                    "config.invalid",
                    "store.url_env must name a DSN environment variable",
                ));
            }
            if store.max_connections == 0 {
                return Err(ApiError::new(
                    "config.invalid",
                    "store.max_connections must be positive",
                ));
            }
        }
        Ok(())
    }

    /// Resolve the delegation signing secret. Environment configuration wins so
    /// a secret value need not be stored in the operator file.
    pub fn resolve_secret(&self) -> Result<Vec<u8>, ApiError> {
        let secret = std::env::var("GOLDENBEE_DELEGATION_SECRET")
            .ok()
            .or_else(|| self.delegation_secret.clone())
            .unwrap_or_default();
        if secret.is_empty() {
            return Err(
                ApiError::new("config.invalid", "a delegation signing secret is required")
                    .with("hint", "set GOLDENBEE_DELEGATION_SECRET"),
            );
        }
        Ok(secret.into_bytes())
    }
}

fn resolve(value: &str, base: Option<&Path>) -> PathBuf {
    let path = Path::new(value);
    match (base, path.is_absolute()) {
        (Some(base), false) => base.join(path),
        _ => path.to_path_buf(),
    }
}
