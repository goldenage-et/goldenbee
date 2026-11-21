//! Durable plugin activation records.
//!
//! The record exists so activation and removal are idempotent across restarts
//! and so capability evidence names exactly which bundle revision activated.
//! It is metadata only: assets are always re-resolved from the operator
//! directory, and a recorded plugin that disappears is removed from the record.

use common::ApiError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// Current activation-record schema.
pub(super) const PLUGIN_STORE_SCHEMA: u32 = 1;
/// File name below the instance data directory.
pub(super) const PLUGIN_STORE_FILE: &str = "plugins.json";

/// One recorded active plugin bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StoredPlugin {
    pub version: String,
    pub trust: String,
    pub signer: String,
    pub digest: String,
    pub manifest_digest: String,
    pub components: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredState {
    #[serde(default = "default_schema")]
    schema: u32,
    #[serde(default)]
    plugins: BTreeMap<String, StoredPlugin>,
}

fn default_schema() -> u32 {
    PLUGIN_STORE_SCHEMA
}

impl Default for StoredState {
    fn default() -> Self {
        Self {
            schema: PLUGIN_STORE_SCHEMA,
            plugins: BTreeMap::new(),
        }
    }
}

/// One change observed while reconciling the activation record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginChange {
    /// Plugin id.
    pub name: String,
    /// `activated`, `updated`, or `removed`.
    pub action: &'static str,
}

/// Read the activation record. A corrupt or newer record fails closed.
pub(super) fn read(data_dir: &Path) -> Result<BTreeMap<String, StoredPlugin>, ApiError> {
    let path = data_dir.join(PLUGIN_STORE_FILE);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        ApiError::new(
            "plugin.storage_unavailable",
            format!("cannot read plugin activation record: {error}"),
        )
    })?;
    let state: StoredState = serde_json::from_slice(&bytes).map_err(|error| {
        ApiError::new(
            "plugin.storage_corrupt",
            format!("cannot parse plugin activation record: {error}"),
        )
    })?;
    if state.schema > PLUGIN_STORE_SCHEMA {
        return Err(ApiError::new(
            "plugin.storage_unsupported",
            format!(
                "plugin activation schema {} is newer than supported {PLUGIN_STORE_SCHEMA}",
                state.schema
            ),
        ));
    }
    Ok(state.plugins)
}

/// Compare desired records with the stored set and write only when changed.
pub(super) fn commit(
    data_dir: &Path,
    desired: &BTreeMap<String, StoredPlugin>,
) -> Result<Vec<PluginChange>, ApiError> {
    let previous = read(data_dir)?;
    let mut changes = Vec::new();
    for (name, plugin) in desired {
        match previous.get(name) {
            None => changes.push(PluginChange {
                name: name.clone(),
                action: "activated",
            }),
            Some(existing) if existing != plugin => changes.push(PluginChange {
                name: name.clone(),
                action: "updated",
            }),
            Some(_) => {}
        }
    }
    for name in previous.keys() {
        if !desired.contains_key(name) {
            changes.push(PluginChange {
                name: name.clone(),
                action: "removed",
            });
        }
    }
    if changes.is_empty() {
        return Ok(changes);
    }
    let state = StoredState {
        schema: PLUGIN_STORE_SCHEMA,
        plugins: desired.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&state).map_err(|error| {
        ApiError::new(
            "plugin.storage_unavailable",
            format!("cannot serialize plugin activation record: {error}"),
        )
    })?;
    atomic_write(&data_dir.join(PLUGIN_STORE_FILE), &bytes).map_err(|error| {
        ApiError::new(
            "plugin.storage_unavailable",
            format!("cannot persist plugin activation record: {error}"),
        )
    })?;
    Ok(changes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
