//! Loaded agent definitions and immutable asset catalog entries.

use adk_rust::skill::{SkillDocument, SkillIndex};
use adk_rust::{Tool, Toolset};
use common::ApiError;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Where an agent in the checked catalog came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AgentOrigin {
    /// Loaded automatically from an operator-configured agent definition.
    Loaded,
    /// Registered by compiled package code.
    Manual,
    /// Compiled native graph workflow registered by package code.
    Graph,
}

/// One checked agent inventory entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentDescriptor {
    /// Configured or registered agent id.
    pub id: String,
    /// Installed package code or configured file.
    pub origin: AgentOrigin,
    /// Native description.
    pub description: String,
    /// Native skills referenced by this agent.
    pub skills: Vec<String>,
}

/// One discovered `SKILL.md` catalog entry.
#[derive(Debug, Clone)]
pub struct SkillAsset {
    /// Skill name from the native parser.
    pub name: String,
    /// Skill description from the native parser.
    pub description: String,
    /// Optional version from the native parser.
    pub version: Option<String>,
    /// Optional license from the native parser.
    pub license: Option<String>,
    /// Optional compatibility note from the native parser.
    pub compatibility: Option<String>,
    /// Native discovery tags.
    pub tags: Vec<String>,
    /// Tools the skill claims it may use. This is descriptive metadata only;
    /// it never binds or authorizes a tool.
    pub allowed_tools: Vec<String>,
    /// Supporting resource paths declared by the skill.
    pub references: Vec<String>,
    /// Whether the skill requires explicit invocation.
    pub trigger: bool,
    /// Optional guided-input hint.
    pub hint: Option<String>,
    /// Extension metadata from the native parser.
    pub metadata: BTreeMap<String, serde_json::Value>,
    /// File glob activation triggers.
    pub triggers: Vec<String>,
    /// Source file.
    pub path: PathBuf,
    /// Instructional body from the native parser.
    pub body: String,
}

impl SkillAsset {
    /// Build the pinned native `SkillDocument` for this discovered asset.
    ///
    /// The content hash and normalized id use the same shape as the native
    /// catalog loader because `adk_skill::build_document` is private. Parsing,
    /// selection, and prompt injection remain native.
    pub(crate) fn document(&self) -> SkillDocument {
        let mut hasher = Sha256::new();
        hasher.update(self.body.as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        let mut token = String::new();
        for value in self.name.chars() {
            if value.is_ascii_alphanumeric() {
                token.push(value.to_ascii_lowercase());
            } else if value == ' ' || value == '-' || value == '_' {
                token.push('-');
            } else if value.is_alphanumeric() {
                token.push(value);
            }
        }
        if token.is_empty() {
            token.push_str("skill");
        }
        SkillDocument {
            id: format!("{token}-{}", &hash[..12]),
            name: self.name.clone(),
            description: self.description.clone(),
            version: self.version.clone(),
            license: self.license.clone(),
            compatibility: self.compatibility.clone(),
            tags: self.tags.clone(),
            allowed_tools: self.allowed_tools.clone(),
            references: self.references.clone(),
            trigger: self.trigger,
            hint: self.hint.clone(),
            metadata: self
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            body: self.body.clone(),
            path: self.path.clone(),
            hash,
            last_modified: None,
            triggers: self.triggers.clone(),
        }
    }
}

/// Build a native selection index containing exactly the named skills.
///
/// Missing names are an actionable asset error. The catalog is not widened
/// with unrelated loaded skills, so native selection never inserts every
/// discovered skill into an agent prompt.
pub(crate) fn skill_index(
    skills: &[SkillAsset],
    names: &[String],
) -> std::result::Result<Option<SkillIndex>, ApiError> {
    if names.is_empty() {
        return Ok(None);
    }
    let mut documents = Vec::with_capacity(names.len());
    for name in names {
        let skill = skills
            .iter()
            .find(|skill| &skill.name == name)
            .ok_or_else(|| {
                ApiError::new(
                    "asset.skill_missing",
                    format!("skill '{name}' is not discovered by this Worker"),
                )
            })?;
        documents.push(skill.document());
    }
    documents.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(Some(SkillIndex::new(documents)))
}

/// One TOML agent definition. Unknown fields fail so a typo cannot silently
/// drop a prompt, skill, or tool reference.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    /// Agent id and task target.
    pub name: String,
    /// Native description.
    #[serde(default)]
    pub description: Option<String>,
    /// Runtime model reference.
    pub model: String,
    /// Prompt id resolved from the configured prompt directory.
    pub prompt: String,
    /// Skill names resolved from the configured skill catalog.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Registered tool ids exposed to this agent.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Registered toolset ids exposed to this agent.
    #[serde(default)]
    pub toolsets: Vec<String>,
}

/// Native builder inputs for a file-defined agent. Profiles rebuild these with
/// their own instruction and model while retaining the resolved tools.
#[derive(Clone)]
pub(crate) struct AgentSpec {
    pub(crate) description: String,
    /// Resolved model reference from the definition. Commands may override it
    /// only with another model the Worker already declares.
    pub(crate) model: String,
    pub(crate) tools: Vec<Arc<dyn Tool>>,
    pub(crate) toolsets: Vec<Arc<dyn Toolset>>,
    /// MCP connection ids declared by this agent. They are resolved from the
    /// instance registry per invocation, after the profile allowlist is applied.
    pub(crate) mcp_servers: Vec<String>,
    /// Skill names selected by the agent definition. Profiles may override the
    /// selection, but these remain the agent's own resolved skills.
    pub(crate) skills: Vec<String>,
}
