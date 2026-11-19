//! Host-composer operator configuration.
//!
//! One composer process serves exactly one host. Every token is scoped to a
//! Gateway identity, the `instance:admin` scope, and an optional instance
//! allow-list. Filesystem roots, grants, and credential prefixes are policy
//! owned by the host, never by the Gateway or a Client.

use common::{ApiError, HOST_OPERATIONS};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn default_operations() -> Vec<String> {
    HOST_OPERATIONS
        .iter()
        .map(|value| (*value).to_string())
        .collect()
}

/// One authenticated Gateway binding.
#[derive(Debug, Clone, Deserialize)]
pub struct HostCaller {
    /// Bearer credential used only for this host composer.
    pub token: String,
    /// Gateway identity this credential is bound to.
    pub gateway: String,
    /// Granted host scopes. Lifecycle operations require `instance:admin`.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Instances this Gateway may administer. Omitted permits every instance
    /// whose identifier is otherwise valid in this host's root.
    #[serde(default)]
    pub instances: Option<Vec<String>>,
}

/// One host composer configuration file.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Stable host identity.
    pub id: String,
    /// Host-local instance root.
    pub instances_root: String,
    /// Verified package cache root (`packages/sha256/<digest>`).
    pub packages_root: String,
    /// Operations this composer implements.
    #[serde(default = "default_operations")]
    pub operations: Vec<String>,
    /// Instance grants a caller may request.
    #[serde(default)]
    pub allowed_grants: Vec<String>,
    /// Profile grants a caller may request.
    #[serde(default)]
    pub allowed_profile_grants: Vec<String>,
    /// Roots under which an operator configuration file may be selected.
    #[serde(default)]
    pub allowed_config_roots: Vec<String>,
    /// Roots under which an authorized workspace may be selected.
    #[serde(default)]
    pub allowed_workspace_roots: Vec<String>,
    /// Credential reference prefixes this host accepts.
    #[serde(default)]
    pub allowed_secret_prefixes: Vec<String>,
    /// Whether a Worker may listen beyond loopback.
    #[serde(default)]
    pub allow_remote_listener: bool,
    /// Authenticated Gateway bindings.
    #[serde(default)]
    pub callers: Vec<HostCaller>,
    /// Directory relative paths resolve against. Set by [`Config::load`].
    #[serde(skip)]
    pub base_dir: Option<PathBuf>,
}

impl Config {
    /// Read and validate one composer configuration file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ApiError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|_| {
            ApiError::new(
                "host.config_invalid",
                "host composer configuration could not be read",
            )
            .with("path", path.display().to_string())
        })?;
        let mut config: Self = serde_json::from_slice(&bytes).map_err(|error| {
            ApiError::new(
                "host.config_invalid",
                "host composer configuration is not valid JSON",
            )
            .with("detail", error.to_string())
        })?;
        config.base_dir = path.parent().map(Path::to_path_buf);
        config.validate()?;
        Ok(config)
    }

    /// Resolve the instance root.
    pub fn instances_root(&self) -> PathBuf {
        resolve(&self.instances_root, self.base_dir.as_deref())
    }

    /// Resolve the verified package cache root.
    pub fn packages_root(&self) -> PathBuf {
        resolve(&self.packages_root, self.base_dir.as_deref())
    }

    /// Resolve one configured root.
    pub fn root(&self, value: &str) -> PathBuf {
        resolve(value, self.base_dir.as_deref())
    }

    fn validate(&self) -> Result<(), ApiError> {
        if self.id.trim().is_empty() {
            return Err(ApiError::new(
                "host.config_invalid",
                "host composer id must not be empty",
            ));
        }
        if self.callers.is_empty() {
            return Err(ApiError::new(
                "host.config_invalid",
                "host composer must bind at least one Gateway credential",
            ));
        }
        let known: BTreeSet<&str> = HOST_OPERATIONS.iter().copied().collect();
        let mut tokens = BTreeSet::new();
        for operation in &self.operations {
            if !known.contains(operation.as_str()) {
                return Err(ApiError::new(
                    "host.config_invalid",
                    format!("unknown host operation '{operation}'"),
                ));
            }
        }
        for caller in &self.callers {
            if caller.token.trim().is_empty() {
                return Err(ApiError::new(
                    "host.config_invalid",
                    "host composer credentials must not be empty",
                ));
            }
            if caller.gateway.trim().is_empty() {
                return Err(ApiError::new(
                    "host.config_invalid",
                    "host composer credentials must name a Gateway",
                ));
            }
            if !tokens.insert(caller.token.as_str()) {
                return Err(ApiError::new(
                    "host.config_invalid",
                    "each host credential may be bound only once",
                ));
            }
        }
        Ok(())
    }
}

fn resolve(value: &str, base: Option<&Path>) -> PathBuf {
    let path = Path::new(value);
    match (base, path.is_absolute()) {
        (Some(base), false) => base.join(path),
        _ => path.to_path_buf(),
    }
}
