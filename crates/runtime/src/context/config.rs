//! Operator configuration for deterministic context assembly.
//!
//! Memory files are generic operator assets. The runtime never hardcodes a
//! vendor filename; a Worker Package or operator names the files, globs, import
//! behavior, and precedence. This is distinct from native long-term memory.

use common::ApiError;
use serde::{Deserialize, Serialize};

/// Resolved `[context]` configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextConfig {
    /// Ordered memory sources. Higher precedence is placed first.
    #[serde(default)]
    pub memory: Vec<MemorySource>,
    /// Per-file byte ceiling.
    #[serde(default = "default_max_memory_bytes")]
    pub max_memory_bytes: usize,
    /// Total assembled memory byte ceiling.
    #[serde(default = "default_max_context_bytes")]
    pub max_context_bytes: usize,
    /// Maximum number of distinct memory files.
    #[serde(default = "default_max_files")]
    pub max_memory_files: usize,
    /// Maximum `@path` import depth.
    #[serde(default = "default_max_import_depth")]
    pub max_import_depth: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            memory: Vec::new(),
            max_memory_bytes: default_max_memory_bytes(),
            max_context_bytes: default_max_context_bytes(),
            max_memory_files: default_max_files(),
            max_import_depth: default_max_import_depth(),
        }
    }
}

/// One configured memory source group.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySource {
    /// Stable source identity used in diagnostics and the context manifest.
    pub id: String,
    /// Exact files, in hierarchy order.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Glob patterns. Matches are sorted for deterministic assembly.
    #[serde(default)]
    pub globs: Vec<String>,
    /// Ordering weight. Higher precedence is assembled first.
    #[serde(default)]
    pub precedence: i32,
    /// Follow `@path` import lines in loaded files.
    #[serde(default = "default_true")]
    pub imports: bool,
    /// Record missing, unreadable, or oversized files as diagnostics instead
    /// of failing context assembly.
    #[serde(default)]
    pub optional: bool,
    /// Source-specific per-file byte ceiling.
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

impl ContextConfig {
    /// Validate the context configuration before any asset is loaded.
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.max_memory_bytes == 0 || self.max_memory_bytes > 16 * 1024 * 1024 {
            return Err(ApiError::new(
                "config.context_max_memory_bytes",
                "context.max_memory_bytes must be between 1 and 16777216",
            ));
        }
        if self.max_context_bytes == 0 || self.max_context_bytes > 64 * 1024 * 1024 {
            return Err(ApiError::new(
                "config.context_max_context_bytes",
                "context.max_context_bytes must be between 1 and 67108864",
            ));
        }
        if self.max_memory_files == 0 || self.max_memory_files > 65_536 {
            return Err(ApiError::new(
                "config.context_max_memory_files",
                "context.max_memory_files must be between 1 and 65536",
            ));
        }
        if self.max_import_depth == 0 || self.max_import_depth > 64 {
            return Err(ApiError::new(
                "config.context_max_import_depth",
                "context.max_import_depth must be between 1 and 64",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for source in &self.memory {
            if source.id.is_empty()
                || source.id.len() > 64
                || source
                    .id
                    .chars()
                    .any(|value| !value.is_ascii_alphanumeric() && value != '_' && value != '-')
            {
                return Err(ApiError::new(
                    "config.context_source_id",
                    "a context memory source id must be 1-64 characters of [A-Za-z0-9_-]",
                ));
            }
            if !ids.insert(source.id.as_str()) {
                return Err(ApiError::new(
                    "config.context_source_duplicate",
                    format!("context memory source '{}' is declared twice", source.id),
                ));
            }
            if source.paths.is_empty() && source.globs.is_empty() {
                return Err(ApiError::new(
                    "config.context_source_empty",
                    format!(
                        "context memory source '{}' must declare at least one path or glob",
                        source.id
                    ),
                ));
            }
            if !(-10_000..=10_000).contains(&source.precedence) {
                return Err(ApiError::new(
                    "config.context_precedence",
                    format!(
                        "context memory source '{}' precedence must be between -10000 and 10000",
                        source.id
                    ),
                ));
            }
            if source
                .max_bytes
                .is_some_and(|value| value == 0 || value > 16 * 1024 * 1024)
            {
                return Err(ApiError::new(
                    "config.context_source_max_bytes",
                    format!(
                        "context memory source '{}' max_bytes must be between 1 and 16777216",
                        source.id
                    ),
                ));
            }
            for pattern in &source.globs {
                glob::Pattern::new(pattern).map_err(|error| {
                    ApiError::new(
                        "config.context_glob_invalid",
                        format!(
                            "context memory source '{}' has invalid glob '{}': {error}",
                            source.id, pattern
                        ),
                    )
                })?;
            }
        }
        Ok(())
    }
}

fn default_true() -> bool {
    true
}

fn default_max_memory_bytes() -> usize {
    256 * 1024
}

fn default_max_context_bytes() -> usize {
    1024 * 1024
}

fn default_max_files() -> usize {
    128
}

fn default_max_import_depth() -> usize {
    8
}
