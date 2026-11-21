//! Skill binding for loaded, profile, and command agents.
//!
//! Eager binding keeps the pinned native selection/injection behavior. When the
//! package declares and the operator selects progressive disclosure, the agent
//! receives a runtime toolset built on the pinned `adk_skill` instruction and
//! catalog primitives:
//!
//! - L1 `list_skills`: names, descriptions, and declarations only.
//! - L2 `load_skill`: one skill's instructions and frontmatter on demand.
//! - L3 `load_skill_resource`: one bundled file under `references/`, `assets/`,
//!   or `scripts/` on demand.
//!
//! Verified gap in pinned ADK 2.3.0: the native `adk_skill::SkillToolset` records
//! activation in session state, but the `ToolContext` supplied by the native
//! Runner reports `state() == None`. L3 therefore always rejects an activated
//! skill. This adapter keeps the pinned L1/L2/L3 surface and bounds, but reads
//! the immutable in-memory index instead of depending on unavailable activation
//! state. It never adds a tool: `allowed-tools` is validated against tools
//! already bound to the agent.
//!
//! Scripts are never executed here. `scripts/**` is exposed only as readable
//! text; execution must go through the ordinary sandboxed command tool with its
//! normal approval policy.

use crate::agents::definition::skill_index;
use crate::agents::SkillAsset;
use crate::tools::ToolKernel;
use adk_rust::prelude::LlmAgentBuilder;
use adk_rust::skill::{build_skill_system_instruction, SkillDocument, SkillIndex};
use adk_rust::{Tool, ToolContext, Toolset};
use async_trait::async_trait;
use common::ApiError;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Resolved skill disclosure behavior for one instance revision.
#[derive(Debug, Clone)]
pub(crate) struct SkillDisclosure {
    /// Whether the package declared and the operator selected progressive
    /// disclosure.
    pub progressive: bool,
    /// Maximum characters returned by `load_skill`.
    pub max_instruction_chars: usize,
    /// Maximum characters returned by `load_skill_resource`.
    pub max_resource_chars: usize,
}

impl SkillDisclosure {
    /// Stable wire name.
    pub(crate) fn as_str(&self) -> &'static str {
        if self.progressive {
            "progressive"
        } else {
            "eager"
        }
    }
}

impl Default for SkillDisclosure {
    fn default() -> Self {
        Self {
            progressive: false,
            max_instruction_chars: 8_000,
            max_resource_chars: 16_000,
        }
    }
}

/// Bind exactly the selected skills to one agent builder.
pub(crate) fn bind(
    builder: LlmAgentBuilder,
    skills: &[SkillAsset],
    names: &[String],
    disclosure: &SkillDisclosure,
    tools: BTreeMap<String, Arc<dyn Tool>>,
    kernel: Option<&ToolKernel>,
) -> Result<LlmAgentBuilder, ApiError> {
    let Some(index) = skill_index(skills, names)? else {
        return Ok(builder);
    };
    if !disclosure.progressive {
        return Ok(builder.with_skills(index));
    }
    let toolset: Arc<dyn Toolset> = Arc::new(ProgressiveSkills {
        index: Arc::new(index),
        tools,
        max_instruction_chars: disclosure.max_instruction_chars,
        max_resource_chars: disclosure.max_resource_chars,
    });
    let toolset = match kernel {
        Some(kernel) => kernel.wrap_toolset(toolset),
        None => toolset,
    };
    Ok(builder.toolset(toolset))
}

struct ProgressiveSkills {
    index: Arc<SkillIndex>,
    /// Tools already bound to the agent. A skill cannot add one.
    tools: BTreeMap<String, Arc<dyn Tool>>,
    max_instruction_chars: usize,
    max_resource_chars: usize,
}

impl ProgressiveSkills {
    fn truncate(value: &str, limit: usize) -> String {
        if value.chars().count() <= limit {
            return value.to_string();
        }
        let mut truncated = value.chars().take(limit).collect::<String>();
        truncated.push_str("\n[... truncated]");
        truncated
    }

    fn resource_path(skill: &SkillDocument, requested: &str) -> adk_rust::Result<PathBuf> {
        let relative = Path::new(requested);
        if !matches!(
            relative.components().next(),
            Some(Component::Normal(prefix))
                if prefix == "references" || prefix == "assets" || prefix == "scripts"
        ) || relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(adk_rust::AdkError::tool(
                "resource path must be a relative path under references/, assets/, or scripts/",
            ));
        }
        let root = skill
            .path
            .parent()
            .ok_or_else(|| adk_rust::AdkError::tool("the skill has no resource directory"))?;
        let root = fs::canonicalize(root)
            .map_err(|error| adk_rust::AdkError::tool(format!("resolve skill root: {error}")))?;
        let candidate = fs::canonicalize(root.join(relative)).map_err(|error| {
            adk_rust::AdkError::tool(format!(
                "resource '{requested}' could not be resolved: {error}"
            ))
        })?;
        if !candidate.starts_with(&root) {
            return Err(adk_rust::AdkError::tool(
                "resource path escapes the skill directory",
            ));
        }
        Ok(candidate)
    }
}

#[async_trait]
impl Toolset for ProgressiveSkills {
    fn name(&self) -> &str {
        "skills"
    }

    async fn tools(
        &self,
        _ctx: Arc<dyn adk_rust::ReadonlyContext>,
    ) -> adk_rust::Result<Vec<Arc<dyn Tool>>> {
        Ok(vec![
            Arc::new(ListSkills {
                skills: self.index.clone(),
            }),
            Arc::new(LoadSkill {
                skills: self.index.clone(),
                tools: self.tools.clone(),
                max_chars: self.max_instruction_chars,
            }),
            Arc::new(LoadResource {
                skills: self.index.clone(),
                max_chars: self.max_resource_chars,
            }),
        ])
    }

    async fn process_llm_request(
        &self,
        _ctx: Arc<dyn adk_rust::ReadonlyContext>,
        request: &mut adk_rust::LlmRequest,
    ) -> adk_rust::Result<()> {
        request.contents.insert(
            0,
            adk_rust::Content::new("user").with_text(build_skill_system_instruction(
                None,
                Some(&["list_skills", "load_skill", "load_skill_resource"]),
                None,
                false,
            )),
        );
        Ok(())
    }
}

struct ListSkills {
    skills: Arc<SkillIndex>,
}

#[async_trait]
impl Tool for ListSkills {
    fn name(&self) -> &str {
        "list_skills"
    }

    fn description(&self) -> &str {
        "Lists available skills with names, descriptions, and declared resources."
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({"type": "object", "properties": {}}))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_rust::Result<Value> {
        Ok(ProgressiveSkills::summaries_for(&self.skills))
    }
}

impl ProgressiveSkills {
    fn summaries_for(index: &SkillIndex) -> Value {
        Value::Array(
            index
                .skills()
                .iter()
                .map(|skill| {
                    json!({
                        "name": skill.name,
                        "description": skill.description,
                        "tags": skill.tags,
                        "references": skill.references,
                        "allowed_tools": skill.allowed_tools,
                        "trigger": skill.trigger,
                        "hint": skill.hint,
                    })
                })
                .collect(),
        )
    }
}

struct LoadSkill {
    skills: Arc<SkillIndex>,
    tools: BTreeMap<String, Arc<dyn Tool>>,
    max_chars: usize,
}

#[async_trait]
impl Tool for LoadSkill {
    fn name(&self) -> &str {
        "load_skill"
    }

    fn description(&self) -> &str {
        "Loads the SKILL.md instructions for a given skill."
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(
            json!({"type":"object","properties":{"skill_name":{"type":"string"}},"required":["skill_name"]}),
        )
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let name = args
            .get("skill_name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                adk_rust::AdkError::tool("missing required string argument 'skill_name'")
            })?;
        let skill = self
            .skills
            .find_by_name(name)
            .ok_or_else(|| adk_rust::AdkError::tool(format!("skill '{name}' was not found")))?;
        let missing = skill
            .allowed_tools
            .iter()
            .filter(|name| !self.tools.contains_key(*name))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(adk_rust::AdkError::tool(format!(
                "skill '{}' declares tools this agent does not bind: {}",
                skill.name,
                missing.join(", ")
            )));
        }
        Ok(json!({
            "skillName": skill.name,
            "skillId": skill.id,
            "hash": skill.hash,
            "instructions": ProgressiveSkills::truncate(&skill.body, self.max_chars),
            "frontmatter": {
                "name": skill.name,
                "description": skill.description,
                "version": skill.version,
                "tags": skill.tags,
                "allowed_tools": skill.allowed_tools,
                "references": skill.references,
            },
        }))
    }
}

struct LoadResource {
    skills: Arc<SkillIndex>,
    max_chars: usize,
}

#[async_trait]
impl Tool for LoadResource {
    fn name(&self) -> &str {
        "load_skill_resource"
    }

    fn description(&self) -> &str {
        "Loads a resource file from references/, assets/, or scripts/ within a skill."
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(
            json!({"type":"object","properties":{"skill_name":{"type":"string"},"file_path":{"type":"string"}},"required":["skill_name","file_path"]}),
        )
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let name = args
            .get("skill_name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                adk_rust::AdkError::tool("missing required string argument 'skill_name'")
            })?;
        let path = args
            .get("file_path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                adk_rust::AdkError::tool("missing required string argument 'file_path'")
            })?;
        let skill = self
            .skills
            .find_by_name(name)
            .ok_or_else(|| adk_rust::AdkError::tool(format!("skill '{name}' was not found")))?;
        let resource = ProgressiveSkills::resource_path(skill, path)?;
        let content = fs::read_to_string(&resource).map_err(|error| {
            adk_rust::AdkError::tool(format!("read skill resource '{path}': {error}"))
        })?;
        Ok(json!({
            "skillName": skill.name,
            "filePath": path,
            "content": ProgressiveSkills::truncate(&content, self.max_chars),
        }))
    }
}
