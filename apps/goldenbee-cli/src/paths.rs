//! Default filesystem locations for the operator CLI.

use std::path::{Path, PathBuf};

/// Resolved install, configuration, and state roots.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Directory receiving installed executables.
    pub bin: PathBuf,
    /// Directory containing generated configuration and local credentials.
    pub config: PathBuf,
    /// Directory containing Gateway and composer state.
    pub data: PathBuf,
}

impl Paths {
    /// Resolve explicit overrides, environment overrides, then XDG defaults.
    pub fn resolve(
        bin: Option<PathBuf>,
        config: Option<PathBuf>,
        data: Option<PathBuf>,
    ) -> Result<Self, String> {
        Ok(Self {
            bin: bin.unwrap_or_else(default_bin),
            config: config.unwrap_or_else(default_config),
            data: data.unwrap_or_else(default_data),
        })
    }
}

fn default_bin() -> PathBuf {
    if let Some(value) = env_path("GOLDENBEE_BIN_DIR") {
        return value;
    }
    home().join(".local/bin")
}

fn default_config() -> PathBuf {
    if let Some(value) = env_path("GOLDENBEE_CONFIG_DIR") {
        return value;
    }
    if let Some(value) = env_path("XDG_CONFIG_HOME") {
        return value.join("goldenbee");
    }
    home().join(".config/goldenbee")
}

fn default_data() -> PathBuf {
    if let Some(value) = env_path("GOLDENBEE_DATA_DIR") {
        return value;
    }
    if let Some(value) = env_path("XDG_DATA_HOME") {
        return value.join("goldenbee");
    }
    home().join(".local/share/goldenbee")
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home() -> PathBuf {
    env_path("HOME").unwrap_or_else(|| Path::new(".").to_path_buf())
}

/// Release directory supplied by environment, if any.
pub fn release_dir() -> Option<PathBuf> {
    env_path("GOLDENBEE_RELEASE_DIR")
}
