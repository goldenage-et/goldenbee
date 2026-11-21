//! Operator-selected plugin bundles.
//!
//! A plugin is data only. The loader reads one `plugin.toml` per bundle, verifies
//! operator trust, validates every referenced asset, and resolves the bundle
//! into the existing runtime registries: skills, commands, external hooks, MCP
//! servers, and declared tool ids. It never loads code, creates a second hook
//! framework, or replaces a Worker-owned asset.
//!
//! Activation is idempotent. A repeated identical bundle is a no-op; a changed
//! bundle is recorded as an update; a bundle that disappears from the operator
//! directory is recorded as a removal. A component id collision with the Worker
//! fails closed instead of overwriting the Worker's asset.

mod hooks;
pub mod manifest;
mod store;
pub mod trust;

pub use manifest::{PluginManifest, PluginTrust, PLUGIN_MANIFEST_FILE, PLUGIN_MANIFEST_VERSION};
pub use store::PluginChange;
pub use trust::{PluginsConfig, TrustRecord};

use crate::mcp::McpServerConfig;
use crate::worker::assets::{canonical_dir, read_dir, safe_file};
use crate::worker::config::WorkerSettings;
use crate::worker::{WorkerHooks, WorkerResult};
use adk_rust::Tool;
use common::ApiError;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Operator-visible inventory of one active plugin bundle.
///
/// It carries no host path and no credential value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginView {
    /// Plugin identity.
    pub name: String,
    /// Bundle version.
    pub version: String,
    /// Operator-facing description.
    pub description: String,
    /// Trust source: `signature` or `allowlist`.
    pub trust: String,
    /// Signer identity, or `operator`.
    pub signer: String,
    /// Content digest.
    pub digest: String,
    /// Resolved component summary.
    pub components: Vec<String>,
    /// Declared tool ids.
    pub tools: Vec<String>,
    /// External hook labels.
    pub hooks: Vec<String>,
    /// Bundled MCP server ids.
    pub mcp: Vec<String>,
    /// Number of bundled skills.
    pub skills: usize,
    /// Number of bundled commands.
    pub commands: usize,
}

impl From<PluginView> for common::PluginCapabilityInventory {
    fn from(view: PluginView) -> Self {
        Self {
            name: view.name,
            version: view.version,
            description: view.description,
            trust: view.trust,
            signer: view.signer,
            digest: view.digest,
            components: view.components,
            tools: view.tools,
            hooks: view.hooks,
            mcp: view.mcp,
            skills: view.skills,
            commands: view.commands,
        }
    }
}

/// Validated plugins resolved for one Worker revision.
pub(crate) struct LoadedPlugins {
    /// Operator-visible inventory.
    pub(crate) views: Vec<PluginView>,
    /// Ordered external hook handlers, one per plugin with hooks.
    pub(crate) hooks: Vec<Arc<dyn WorkerHooks>>,
    /// Additional skill roots.
    pub(crate) skills: Vec<PathBuf>,
    /// Additional command roots.
    pub(crate) commands: Vec<PathBuf>,
    /// Additional MCP servers.
    pub(crate) mcp: BTreeMap<String, McpServerConfig>,
    /// Declared tool ids.
    pub(crate) tools: BTreeSet<String>,
    records: BTreeMap<String, store::StoredPlugin>,
    data_dir: PathBuf,
}

impl LoadedPlugins {
    /// An empty result used when no plugin directory is configured.
    pub(crate) fn empty(data_dir: PathBuf) -> Self {
        Self {
            views: Vec::new(),
            hooks: Vec::new(),
            skills: Vec::new(),
            commands: Vec::new(),
            mcp: BTreeMap::new(),
            tools: BTreeSet::new(),
            records: BTreeMap::new(),
            data_dir,
        }
    }

    /// Reject a declared tool or MCP id that this Worker cannot resolve.
    pub(crate) fn validate_tools(
        &self,
        tools: &BTreeMap<String, Arc<dyn Tool>>,
        mcp_ids: &BTreeSet<String>,
    ) -> WorkerResult<()> {
        for id in &self.tools {
            if tools.contains_key(id) {
                continue;
            }
            if let Some(rest) = id.strip_prefix("mcp:") {
                let (server, _tool) = rest.split_once(':').unwrap_or((rest, ""));
                if !server.is_empty() && mcp_ids.contains(server) {
                    continue;
                }
            }
            return Err(ApiError::new(
                "plugin.tool_unknown",
                format!(
                    "plugin declares tool '{id}' but this Worker exposes no such compiled tool or MCP server"
                ),
            ));
        }
        Ok(())
    }

    /// Persist the activation record after every validation passed.
    pub(crate) fn commit(&self) -> WorkerResult<Vec<PluginChange>> {
        store::commit(&self.data_dir, &self.records)
    }
}

/// Discover, verify, and validate plugins from the operator-selected root.
///
/// Nothing is persisted here. The caller validates resolved tool ids and only
/// then calls [`LoadedPlugins::commit`], so a rejected bundle cannot leave a
/// half-activated record.
pub(crate) fn load(settings: &WorkerSettings) -> WorkerResult<LoadedPlugins> {
    let Some(directory) = settings.plugins_dir.as_deref() else {
        return Ok(LoadedPlugins::empty(settings.data_dir.clone()));
    };
    let root = canonical_dir(directory, "plugin").map_err(|error| {
        ApiError::new(
            "plugin.directory_missing",
            format!(
                "plugin directory '{}' is not readable: {}",
                directory.display(),
                error.message
            ),
        )
    })?;
    let config = &settings.file.plugins;
    let policy = settings.file.hooks.policy();
    let mut loaded = LoadedPlugins::empty(settings.data_dir.clone());
    let mut names = BTreeSet::new();
    for path in read_dir(&root, "plugin")? {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Err(ApiError::new(
                "plugin.directory_invalid",
                format!("plugin directory '{}' has no valid name", path.display()),
            ));
        };
        if name.starts_with('.') {
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join(PLUGIN_MANIFEST_FILE);
        if !manifest_path.is_file() {
            return Err(ApiError::new(
                "plugin.manifest_missing",
                format!(
                    "plugin directory '{}' does not contain {PLUGIN_MANIFEST_FILE}",
                    path.display()
                ),
            ));
        }
        let safe_manifest = safe_file(&manifest_path, &root, "plugin")?;
        let manifest = manifest::parse(&safe_manifest)?;
        if !names.insert(manifest.name.clone()) {
            return Err(ApiError::new(
                "plugin.duplicate",
                format!("plugin '{}' is installed more than once", manifest.name),
            ));
        }
        let plugin_root = path.canonicalize().map_err(|error| {
            ApiError::new(
                "plugin.directory_invalid",
                format!(
                    "plugin directory '{}' is not readable: {error}",
                    path.display()
                ),
            )
        })?;
        let trust = trust::verify(&manifest, &plugin_root, config)?;
        let commands = manifest
            .commands
            .as_ref()
            .map(|directory| resolve_directory(&plugin_root, "command", &directory.directory))
            .transpose()?;
        let skills = manifest
            .skills
            .as_ref()
            .map(|directory| resolve_directory(&plugin_root, "skill", &directory.directory))
            .transpose()?;
        let command_count = match &commands {
            Some(directory) => count_files(directory, "toml")?,
            None => 0,
        };
        let skill_count = match &skills {
            Some(directory) => count_skills(directory)?,
            None => 0,
        };
        if commands.is_some() && command_count == 0 {
            return Err(ApiError::new(
                "plugin.asset_empty",
                format!(
                    "plugin '{}' declares a command directory with no commands",
                    manifest.name
                ),
            ));
        }
        if skills.is_some() && skill_count == 0 {
            return Err(ApiError::new(
                "plugin.asset_empty",
                format!(
                    "plugin '{}' declares a skill directory with no skills",
                    manifest.name
                ),
            ));
        }
        if let Some(directory) = &commands {
            loaded.commands.push(directory.clone());
        }
        if let Some(directory) = &skills {
            loaded.skills.push(directory.clone());
        }
        if let Some(external) = hooks::ExternalHooks::new(
            &manifest.name,
            &plugin_root,
            &manifest.hooks,
            policy.max_output_bytes,
        )? {
            loaded.hooks.push(Arc::new(external));
        }
        let mut hook_labels = Vec::with_capacity(manifest.hooks.len());
        for hook in &manifest.hooks {
            hook_labels.push(format!("{}:{}", hook.event, hook.name));
        }
        let mut mcp = Vec::with_capacity(manifest.mcp.servers.len());
        let mcp_count = manifest.mcp.servers.len();
        for (id, server) in &manifest.mcp.servers {
            if loaded.mcp.contains_key(id) || settings.file.mcp.servers.contains_key(id) {
                return Err(ApiError::new(
                    "plugin.collision",
                    format!(
                        "plugin '{}' declares MCP server '{id}' which is already defined by the Worker or another plugin",
                        manifest.name
                    ),
                ));
            }
            loaded.mcp.insert(id.clone(), server.clone());
            mcp.push(id.clone());
        }
        for tool in &manifest.tools {
            loaded.tools.insert(tool.clone());
        }
        let mut components = Vec::new();
        if command_count > 0 {
            components.push(format!("commands:{command_count}"));
        }
        if skill_count > 0 {
            components.push(format!("skills:{skill_count}"));
        }
        if !manifest.hooks.is_empty() {
            components.push(format!("hooks:{}", manifest.hooks.len()));
        }
        if !manifest.mcp.servers.is_empty() {
            components.push(format!("mcp:{mcp_count}"));
        }
        if !manifest.tools.is_empty() {
            components.push(format!("tools:{}", manifest.tools.len()));
        }
        loaded.views.push(PluginView {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            trust: trust.source.clone(),
            signer: trust.signer.clone(),
            digest: trust.digest.clone(),
            components: components.clone(),
            tools: manifest.tools.clone(),
            hooks: hook_labels,
            mcp,
            skills: skill_count,
            commands: command_count,
        });
        loaded.records.insert(
            manifest.name,
            store::StoredPlugin {
                version: manifest.version,
                trust: trust.source,
                signer: trust.signer,
                digest: trust.digest,
                manifest_digest: trust.manifest_digest,
                components,
            },
        );
    }
    loaded
        .views
        .sort_by(|left, right| left.name.cmp(&right.name));
    Ok(loaded)
}

fn resolve_directory(root: &Path, kind: &str, value: &str) -> WorkerResult<PathBuf> {
    let path = root.join(value);
    let resolved = path.canonicalize().map_err(|error| {
        ApiError::new(
            format!("plugin.{kind}_missing"),
            format!("plugin {kind} directory '{value}' is not readable: {error}"),
        )
    })?;
    if !resolved.starts_with(root) || !resolved.is_dir() {
        return Err(ApiError::new(
            "plugin.path_denied",
            format!("plugin {kind} directory '{value}' resolves outside the plugin directory"),
        ));
    }
    Ok(resolved)
}

fn count_files(directory: &Path, extension: &str) -> WorkerResult<usize> {
    Ok(read_dir(directory, "plugin")?
        .into_iter()
        .filter(|path| path.is_file())
        .filter(|path| {
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        })
        .count())
}

fn count_skills(directory: &Path) -> WorkerResult<usize> {
    Ok(read_dir(directory, "plugin")?
        .into_iter()
        .filter(|path| path.is_dir() && path.join("SKILL.md").is_file())
        .count())
}

impl std::fmt::Debug for LoadedPlugins {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadedPlugins")
            .field(
                "plugins",
                &self.views.iter().map(|view| &view.name).collect::<Vec<_>>(),
            )
            .finish()
    }
}
