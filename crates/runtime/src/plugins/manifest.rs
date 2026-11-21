//! Data-only plugin manifest.
//!
//! A manifest never loads code. It names assets that already exist in the
//! Worker package or in operator-selected directories. The shape borrows the
//! Codex plugin manifest idea (name, version, skills, commands, hooks, MCP) but
//! keeps Goldenbee's own schema and trust fields.

use common::ApiError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Current manifest schema version.
pub const PLUGIN_MANIFEST_VERSION: u32 = 1;
/// Manifest file name inside one plugin directory.
pub const PLUGIN_MANIFEST_FILE: &str = "plugin.toml";
/// Maximum accepted manifest bytes.
const MAX_MANIFEST_BYTES: usize = 262_144;

/// One plugin bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    /// Schema version. Only [`PLUGIN_MANIFEST_VERSION`] is accepted.
    #[serde(default = "default_manifest_version")]
    pub manifest_version: u32,
    /// Stable plugin identity.
    pub name: String,
    /// Bundle version.
    pub version: String,
    /// Operator-facing description.
    #[serde(default)]
    pub description: String,
    /// Discovery keywords.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Trust metadata. Absent means the operator allowlist must name the
    /// plugin explicitly.
    #[serde(default)]
    pub trust: Option<PluginTrust>,
    /// Compiled or MCP tool ids the bundle expects. Declarations are checked
    /// against installed capabilities before any component activates.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Bundled command directory, relative to the plugin root.
    #[serde(default)]
    pub commands: Option<PluginDirectory>,
    /// Bundled skill directory, relative to the plugin root.
    #[serde(default)]
    pub skills: Option<PluginDirectory>,
    /// Ordered external hook declarations.
    #[serde(default)]
    pub hooks: Vec<PluginHook>,
    /// Bundled MCP server declarations.
    #[serde(default)]
    pub mcp: PluginMcp,
}

fn default_manifest_version() -> u32 {
    PLUGIN_MANIFEST_VERSION
}

/// Plugin trust metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginTrust {
    /// Trusted signer identity. The matching public key must be configured by
    /// the operator.
    #[serde(default)]
    pub signer: Option<String>,
    /// Ed25519 signature over the canonical manifest digest, with this field
    /// omitted. Base64 URL-safe without padding.
    #[serde(default)]
    pub signature: Option<String>,
    /// SHA-256 digest of the plugin content, excluding `plugin.toml`.
    #[serde(default)]
    pub digest: Option<String>,
}

/// One bundled asset directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDirectory {
    /// Directory path relative to the plugin root.
    pub directory: String,
}

/// One external hook declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginHook {
    /// Stable hook name used in failure evidence.
    pub name: String,
    /// One runtime hook event.
    pub event: String,
    /// Executable path relative to the plugin root.
    pub command: String,
    /// Fixed arguments passed before the JSON protocol on stdin.
    #[serde(default)]
    pub args: Vec<String>,
    /// Tool-name matcher for `pre_tool`/`post_tool`: exact or one trailing `*`.
    #[serde(default)]
    pub matcher: Option<String>,
    /// Per-command deadline. The runtime hook policy may apply a shorter one.
    #[serde(default = "default_hook_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_hook_timeout_ms() -> u64 {
    2_000
}

/// Bundled MCP servers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginMcp {
    /// Server declarations keyed by stable id.
    #[serde(default)]
    pub servers: BTreeMap<String, crate::mcp::McpServerConfig>,
}

/// Runtime hook event names a plugin may attach to.
pub const HOOK_EVENTS: &[&str] = &[
    "pre_tool",
    "post_tool",
    "prompt_submit",
    "stop",
    "subagent_stop",
    "pre_compact",
    "post_compact",
    "session_start",
    "session_end",
];

/// Parse and structurally validate one manifest.
pub fn parse(path: &std::path::Path) -> Result<PluginManifest, ApiError> {
    let bytes = std::fs::read(path).map_err(|error| {
        ApiError::new(
            "plugin.manifest_unreadable",
            format!("cannot read plugin manifest '{}': {error}", path.display()),
        )
    })?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ApiError::new(
            "plugin.manifest_too_large",
            format!(
                "plugin manifest '{}' exceeds {MAX_MANIFEST_BYTES} bytes",
                path.display()
            ),
        ));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        ApiError::new(
            "plugin.manifest_invalid",
            format!("plugin manifest '{}' is not UTF-8", path.display()),
        )
    })?;
    let manifest: PluginManifest = toml::from_str(text).map_err(|error| {
        ApiError::new(
            "plugin.manifest_invalid",
            format!(
                "plugin manifest '{}' is invalid TOML: {error}",
                path.display()
            ),
        )
    })?;
    validate(&manifest)?;
    Ok(manifest)
}

fn validate(manifest: &PluginManifest) -> Result<(), ApiError> {
    if manifest.manifest_version != PLUGIN_MANIFEST_VERSION {
        return Err(ApiError::new(
            "plugin.manifest_version",
            format!(
                "plugin '{}' declares manifest_version {} but this runtime supports {PLUGIN_MANIFEST_VERSION}",
                manifest.name, manifest.manifest_version
            ),
        ));
    }
    validate_id("name", &manifest.name, 64)?;
    if manifest.version.trim().is_empty() || manifest.version.len() > 64 {
        return Err(ApiError::new(
            "plugin.manifest_invalid",
            format!(
                "plugin '{}' version must be 1-64 non-empty characters",
                manifest.name
            ),
        ));
    }
    if manifest.description.len() > 2_048 {
        return Err(ApiError::new(
            "plugin.manifest_invalid",
            format!(
                "plugin '{}' description exceeds 2048 characters",
                manifest.name
            ),
        ));
    }
    if manifest.keywords.len() > 32 {
        return Err(ApiError::new(
            "plugin.manifest_invalid",
            format!("plugin '{}' declares more than 32 keywords", manifest.name),
        ));
    }
    for keyword in &manifest.keywords {
        validate_id("keyword", keyword, 64)?;
    }
    let mut tools = BTreeSet::new();
    for tool in &manifest.tools {
        validate_id("tool", tool, 128)?;
        if !tools.insert(tool) {
            return Err(ApiError::new(
                "plugin.manifest_invalid",
                format!(
                    "plugin '{}' declares tool '{tool}' more than once",
                    manifest.name
                ),
            ));
        }
    }
    for (kind, directory) in [
        ("commands", manifest.commands.as_ref()),
        ("skills", manifest.skills.as_ref()),
    ] {
        if let Some(directory) = directory {
            validate_relative_path(kind, &directory.directory)?;
        }
    }
    let mut names = BTreeSet::new();
    for hook in &manifest.hooks {
        validate_id("hook", &hook.name, 64)?;
        if !names.insert(hook.name.as_str()) {
            return Err(ApiError::new(
                "plugin.manifest_invalid",
                format!(
                    "plugin '{}' declares hook '{}' more than once",
                    manifest.name, hook.name
                ),
            ));
        }
        if !HOOK_EVENTS.contains(&hook.event.as_str()) {
            return Err(ApiError::new(
                "plugin.hook_event",
                format!(
                    "plugin '{}' hook '{}' uses unknown event '{}'; expected one of {}",
                    manifest.name,
                    hook.name,
                    hook.event,
                    HOOK_EVENTS.join(", ")
                ),
            ));
        }
        if hook.timeout_ms == 0 || hook.timeout_ms > 600_000 {
            return Err(ApiError::new(
                "plugin.hook_timeout",
                format!(
                    "plugin '{}' hook '{}' timeout_ms must be 1-600000",
                    manifest.name, hook.name
                ),
            ));
        }
        validate_relative_path("hook", &hook.command)?;
        if hook.args.len() > 32 || hook.args.iter().any(|argument| argument.len() > 1_024) {
            return Err(ApiError::new(
                "plugin.hook_invalid",
                format!(
                    "plugin '{}' hook '{}' declares too many or oversized arguments",
                    manifest.name, hook.name
                ),
            ));
        }
        if let Some(matcher) = &hook.matcher {
            validate_matcher(matcher).map_err(|message| {
                ApiError::new(
                    "plugin.hook_matcher",
                    format!("plugin '{}' hook '{}': {message}", manifest.name, hook.name),
                )
            })?;
        }
    }
    for server in manifest.mcp.servers.keys() {
        validate_id("mcp server", server, 128)?;
    }
    Ok(())
}

fn validate_id(kind: &str, value: &str, maximum: usize) -> Result<(), ApiError> {
    let valid = !value.is_empty()
        && value.len() <= maximum
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-:".contains(character));
    if valid {
        Ok(())
    } else {
        Err(ApiError::new(
            "plugin.manifest_invalid",
            format!("plugin {kind} '{value}' must be 1-{maximum} characters of [A-Za-z0-9._-:]"),
        ))
    }
}

fn validate_relative_path(kind: &str, value: &str) -> Result<(), ApiError> {
    let path = std::path::Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(ApiError::new(
            "plugin.manifest_invalid",
            format!("plugin {kind} path '{value}' must be a relative path without '..'"),
        ));
    }
    Ok(())
}

fn validate_matcher(matcher: &str) -> Result<(), String> {
    if matcher.is_empty() || matcher.len() > 128 {
        return Err("matcher must be 1-128 characters".into());
    }
    if matcher.contains('*') && !(matcher.ends_with('*') && matcher.matches('*').count() == 1) {
        return Err("matcher supports only one trailing '*'".into());
    }
    Ok(())
}

impl PluginManifest {
    /// Canonical digest of this manifest with the signature field removed.
    ///
    /// The signature covers this value; `[trust].digest` covers bundled files.
    pub fn signing_digest(&self) -> String {
        let mut unsigned = self.clone();
        if let Some(trust) = unsigned.trust.as_mut() {
            trust.signature = None;
        }
        common::canonical_digest(&unsigned)
    }
}
