//! Worker-declared subagent policy.
//!
//! The Worker owns the named definitions and the ceilings; the runtime owns
//! admission, durable linkage, cancellation, and result merge. Every ceiling
//! here is a maximum: a caller may narrow it for one child but can never widen
//! it, and the parent's pinned profile revision bounds the result again.

use crate::policy::PermissionMode;
use crate::{ApiError, Result};
use std::collections::BTreeMap;

/// Default bound on merged child output.
pub const DEFAULT_RESULT_BYTES: usize = 16 * 1024;
/// Default bound on a foreground or explicit wait.
pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 120_000;

/// One named subagent definition declared by a Worker Package.
#[derive(Debug, Clone)]
pub struct SubagentDefinition {
    agent: String,
    description: String,
    model: Option<String>,
    tools: Option<Vec<String>>,
    permission_mode: Option<PermissionMode>,
    background: Option<bool>,
}

impl SubagentDefinition {
    /// Declare a definition that runs one registered agent.
    pub fn new(agent: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            description: String::new(),
            model: None,
            tools: None,
            permission_mode: None,
            background: None,
        }
    }

    /// Human-readable description exposed to the model.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Default logical model reference for this definition.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Default tool allowlist. `None` inherits the parent binding; an empty
    /// list binds no tools.
    pub fn tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    /// Inherit the parent's tool binding.
    pub fn inherit_tools(mut self) -> Self {
        self.tools = None;
        self
    }

    /// Default permission-mode ceiling for this definition.
    pub fn permission_mode(mut self, mode: PermissionMode) -> Self {
        self.permission_mode = Some(mode);
        self
    }

    /// Whether this definition spawns in the background by default.
    pub fn background(mut self, background: bool) -> Self {
        self.background = Some(background);
        self
    }

    /// Concrete registered agent this definition selects.
    pub fn agent_name(&self) -> &str {
        &self.agent
    }
}

/// Worker Package subagent policy.
///
/// Registering this policy is what makes the subagent tools available. A
/// runtime without it rejects every subagent operation with
/// `subagent.not_enabled`.
#[derive(Debug, Clone)]
pub struct SubagentConfig {
    definitions: BTreeMap<String, SubagentDefinition>,
    max_depth: u32,
    max_fanout: u32,
    max_tasks: u32,
    background: bool,
    models: Vec<String>,
    tools: Option<Vec<String>>,
    permission_mode: PermissionMode,
    result_bytes: usize,
    wait_timeout_ms: u64,
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl SubagentConfig {
    /// Empty policy: no definitions, depth one, fanout four.
    pub fn new() -> Self {
        Self {
            definitions: BTreeMap::new(),
            max_depth: 1,
            max_fanout: 4,
            max_tasks: 8,
            background: true,
            models: Vec::new(),
            tools: None,
            permission_mode: PermissionMode::Default,
            result_bytes: DEFAULT_RESULT_BYTES,
            wait_timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
        }
    }

    /// Declare one named definition.
    pub fn definition(mut self, name: impl Into<String>, definition: SubagentDefinition) -> Self {
        self.definitions.insert(name.into(), definition);
        self
    }

    /// Maximum delegation depth below a root task.
    pub fn max_depth(mut self, depth: u32) -> Self {
        self.max_depth = depth;
        self
    }

    /// Maximum non-terminal children per parent task.
    pub fn max_fanout(mut self, fanout: u32) -> Self {
        self.max_fanout = fanout;
        self
    }

    /// Package ceiling on committed child admission capacity.
    pub fn max_tasks(mut self, tasks: u32) -> Self {
        self.max_tasks = tasks;
        self
    }

    /// Whether background admission is permitted.
    pub fn background(mut self, allowed: bool) -> Self {
        self.background = allowed;
        self
    }

    /// Logical model references a child may select. Empty means "the parent's
    /// provider only".
    pub fn models<I, S>(mut self, models: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.models = models.into_iter().map(Into::into).collect();
        self
    }

    /// Global tool allowlist ceiling. `None` leaves the definition in charge.
    pub fn tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    /// Permission-mode ceiling. No child may be less restrictive than this.
    pub fn permission_mode(mut self, mode: PermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }

    /// Bound on merged child output.
    pub fn result_bytes(mut self, bytes: usize) -> Self {
        self.result_bytes = bytes.max(1);
        self
    }

    /// Default bound on a foreground spawn or explicit wait.
    pub fn wait_timeout_ms(mut self, ms: u64) -> Self {
        self.wait_timeout_ms = ms.max(1);
        self
    }

    /// Reject a policy that cannot be enforced deterministically.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.definitions.is_empty() {
            return Err("subagents.definitions must declare at least one definition".into());
        }
        for (name, definition) in &self.definitions {
            if name.trim().is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
                return Err("subagent definition names must be 1-64 printable characters".into());
            }
            if definition.agent.trim().is_empty() {
                return Err(format!("subagent definition '{name}' selects no agent"));
            }
        }
        Ok(())
    }

    /// Look up one named definition.
    pub fn definition_named(&self, name: &str) -> Option<&SubagentDefinition> {
        self.definitions.get(name)
    }

    /// Declared definition names, in stable order.
    pub fn names(&self) -> Vec<&str> {
        self.definitions.keys().map(String::as_str).collect()
    }

    /// Effective depth ceiling for one child of a task at `parent_depth`.
    pub fn depth_ceiling(&self) -> u32 {
        self.max_depth
    }

    /// Effective fanout ceiling.
    pub fn fanout_ceiling(&self) -> u32 {
        self.max_fanout
    }

    /// Package admission ceiling.
    pub fn task_ceiling(&self) -> u32 {
        self.max_tasks
    }

    /// Whether background admission is allowed.
    pub fn allows_background(&self) -> bool {
        self.background
    }

    /// Merge ceiling applied to merged child output.
    pub fn result_limit(&self) -> usize {
        self.result_bytes
    }

    /// Bound on one foreground spawn or explicit wait.
    pub fn wait_limit_ms(&self) -> u64 {
        self.wait_timeout_ms
    }

    /// Effective permission ceiling.
    pub fn mode_ceiling(&self) -> PermissionMode {
        self.permission_mode
    }

    /// Resolve and narrow the permission ceiling for one child.
    ///
    /// The requested mode must not be less restrictive than the definition or
    /// the Worker ceiling. `parent_mode` is the mode in force when the parent
    /// called; the result is never wider than any of the three.
    pub fn narrow_mode(
        &self,
        definition: &SubagentDefinition,
        parent_mode: PermissionMode,
        requested: Option<PermissionMode>,
    ) -> Result<PermissionMode> {
        let ceiling = self
            .permission_mode
            .stricter(definition.permission_mode.unwrap_or(self.permission_mode));
        for candidate in [requested, definition.permission_mode]
            .into_iter()
            .flatten()
        {
            if candidate.is_wider_than(ceiling) {
                return Err(ApiError::new(
                    "subagent.permission_widened",
                    "the requested permission mode is wider than the subagent ceiling",
                )
                .with("requested", candidate.as_str())
                .with("ceiling", ceiling.as_str()));
            }
        }
        let requested = requested.unwrap_or(ceiling);
        Ok(ceiling.stricter(parent_mode).stricter(requested))
    }

    /// Resolve and narrow the tool allowlist for one child.
    ///
    /// The definition's list is intersected with the Worker ceiling, then the
    /// per-call request may only remove entries. `None` means the parent
    /// binding is inherited, but it is still filtered by the Worker ceiling.
    pub fn narrow_tools(
        &self,
        definition: &SubagentDefinition,
        requested: Option<&[String]>,
    ) -> Result<Option<Vec<String>>> {
        let definition_tools = definition.tools.as_ref();
        let ceiling = self.tools.as_ref();
        let mut effective = match (definition_tools, ceiling) {
            (Some(definition_tools), Some(ceiling)) => Some(
                definition_tools
                    .iter()
                    .filter(|tool| ceiling.iter().any(|allowed| allowed == *tool))
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
            (Some(definition_tools), None) => Some(definition_tools.clone()),
            (None, Some(ceiling)) => Some(ceiling.clone()),
            (None, None) => None,
        };
        if let Some(requested) = requested {
            if let Some(current) = &effective {
                if let Some(tool) = requested.iter().find(|tool| !current.contains(tool)) {
                    return Err(ApiError::new(
                        "subagent.tool_not_allowed",
                        "the requested tool is outside the subagent definition allowlist",
                    )
                    .with("tool", tool.clone()));
                }
            } else if let Some(ceiling) = self.tools.as_ref() {
                if let Some(tool) = requested.iter().find(|tool| !ceiling.contains(tool)) {
                    return Err(ApiError::new(
                        "subagent.tool_not_allowed",
                        "the requested tool is outside the Worker subagent allowlist",
                    )
                    .with("tool", tool.clone()));
                }
            }
            effective = Some(requested.to_vec());
        }
        Ok(effective)
    }

    /// Resolve and validate the logical model reference for one child.
    ///
    /// An empty Worker allowlist means the child may only use the parent's
    /// resolved provider. A non-empty allowlist is the ceiling.
    pub fn narrow_model(
        &self,
        definition: &SubagentDefinition,
        parent_provider: Option<&str>,
        requested: Option<&str>,
    ) -> Result<Option<String>> {
        let selected = requested
            .map(str::to_string)
            .or_else(|| definition.model.clone());
        let Some(selected) = selected else {
            return Ok(None);
        };
        if !self.models.is_empty() {
            if !self.models.iter().any(|allowed| allowed == &selected) {
                return Err(ApiError::new(
                    "subagent.model_widened",
                    "the requested model is outside the Worker subagent allowlist",
                )
                .with("model", selected));
            }
            return Ok(Some(selected));
        }
        match parent_provider {
            Some(parent) if parent == selected => Ok(Some(selected)),
            Some(parent) => Err(ApiError::new(
                "subagent.model_widened",
                "a subagent may not select a model other than the parent's provider",
            )
            .with("model", selected)
            .with("parent_provider", parent.to_string())),
            None => Ok(Some(selected)),
        }
    }

    /// Background default for one definition, bounded by the Worker policy.
    pub fn resolve_background(
        &self,
        definition: &SubagentDefinition,
        requested: Option<bool>,
    ) -> Result<bool> {
        let background = requested
            .or(definition.background)
            .unwrap_or(self.background);
        if background && !self.background {
            return Err(ApiError::new(
                "subagent.background_denied",
                "this Worker does not permit background subagents",
            ));
        }
        Ok(background)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SubagentConfig {
        SubagentConfig::new()
            .definition(
                "helper",
                SubagentDefinition::new("echo").tools(["read_file", "exec_command"]),
            )
            .permission_mode(PermissionMode::Default)
    }

    #[test]
    fn validation_requires_a_definition_and_an_agent() {
        assert!(SubagentConfig::new().validate().is_err());
        assert!(SubagentConfig::new()
            .definition("bad", SubagentDefinition::new(""))
            .validate()
            .is_err());
        assert!(config().validate().is_ok());
    }

    #[test]
    fn tools_are_intersected_and_requests_only_remove() {
        let config = config();
        let definition = config.definition_named("helper").expect("definition");
        assert_eq!(
            config.narrow_tools(definition, Some(&[])).expect("narrow"),
            Some(Vec::new())
        );
        assert_eq!(
            config
                .narrow_tools(definition, Some(&["read_file".to_string()]))
                .expect("narrow"),
            Some(vec!["read_file".to_string()])
        );
        let error = config
            .narrow_tools(definition, Some(&["web_fetch".to_string()]))
            .expect_err("widening is rejected");
        assert_eq!(error.code, "subagent.tool_not_allowed");

        let ceiling = config.clone().tools(["read_file"]);
        assert_eq!(
            ceiling
                .narrow_tools(
                    ceiling.definition_named("helper").expect("definition"),
                    None
                )
                .expect("intersection"),
            Some(vec!["read_file".to_string()])
        );
    }

    #[test]
    fn model_selection_is_bounded_by_the_parent_or_an_allowlist() {
        let config = config();
        let definition = config.definition_named("helper").expect("definition");
        assert_eq!(
            config
                .narrow_model(definition, Some("deterministic"), None)
                .expect("inherit"),
            None
        );
        assert_eq!(
            config
                .narrow_model(definition, Some("deterministic"), Some("deterministic"))
                .expect("same provider"),
            Some("deterministic".into())
        );
        let error = config
            .narrow_model(definition, Some("deterministic"), Some("echo"))
            .expect_err("widen");
        assert_eq!(error.code, "subagent.model_widened");

        let allowed = config.clone().models(["deterministic", "echo"]);
        assert_eq!(
            allowed
                .narrow_model(
                    allowed.definition_named("helper").expect("definition"),
                    Some("deterministic"),
                    Some("echo")
                )
                .expect("allowed by operator"),
            Some("echo".into())
        );
    }

    #[test]
    fn permission_ceiling_only_narrows() {
        let config = config();
        let definition = config.definition_named("helper").expect("definition");
        assert_eq!(
            config
                .narrow_mode(definition, PermissionMode::FullAuto, None)
                .expect("ceiling"),
            PermissionMode::Default
        );
        assert_eq!(
            config
                .narrow_mode(
                    definition,
                    PermissionMode::Default,
                    Some(PermissionMode::Plan)
                )
                .expect("narrower"),
            PermissionMode::Plan
        );
        let error = config
            .narrow_mode(
                definition,
                PermissionMode::FullAuto,
                Some(PermissionMode::FullAuto),
            )
            .expect_err("widen");
        assert_eq!(error.code, "subagent.permission_widened");
    }

    #[test]
    fn background_is_denied_when_the_worker_disallows_it() {
        let config = config().background(false);
        let definition = config.definition_named("helper").expect("definition");
        assert!(!config
            .resolve_background(definition, None)
            .expect("foreground default"));
        assert_eq!(
            config
                .resolve_background(definition, Some(true))
                .expect_err("denied")
                .code,
            "subagent.background_denied"
        );
    }
}
