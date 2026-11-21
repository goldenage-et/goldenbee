//! Runtime-local subagent policy, lifecycle summaries, and result merge.
//!
//! A subagent is an ordinary durable task admitted by the same runtime as its
//! parent. These types describe only the extra narrowing and linkage that make
//! the child a subagent: the exact definition, the parent/root identity, the
//! depth, the background flag, and the tool/model/permission ceiling that was
//! applied before admission.
//!
//! Absent values mean "not established". A result never turns a missing output
//! or an uncertain state into a fabricated success.

use crate::error::ApiError;
use crate::limits::Usage;
use crate::task::{ResolvedModel, TaskState};
use serde::{Deserialize, Serialize};

/// Durable narrowing applied to one runtime-local child task.
///
/// The policy is computed by the runtime from the parent task and the Worker's
/// declared subagent policy, then frozen onto the child task. It is immutable
/// for the life of that task and is re-applied when the child is executed after
/// a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentPolicy {
    /// Immediate parent task.
    pub parent_task_id: String,
    /// Root task of the lineage. Equal to the parent when the parent is a root.
    pub root_task_id: String,
    /// Named Worker-declared subagent definition.
    pub definition: String,
    /// One-based delegation depth below the root.
    pub depth: u32,
    /// Whether the parent admitted the child without waiting for completion.
    pub background: bool,
    /// Logical model reference selected for the child. Absent inherits the
    /// parent's pinned invocation model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Exact tool allowlist. `None` inherits the parent's binding; `Some([])`
    /// denies every tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Permission-mode ceiling in the runtime wire spelling. Absent inherits
    /// the mode in force when the child was admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// Remaining fanout allowed for this child's own descendants. Zero means
    /// the Worker policy ceiling still applies.
    #[serde(default)]
    pub max_fanout: u32,
    /// Remaining depth allowed below this child. Zero means no descendants.
    #[serde(default)]
    pub max_depth: u32,
}

impl SubagentPolicy {
    /// Whether the frozen allowlist admits this tool.
    pub fn allows_tool(&self, name: &str) -> bool {
        match &self.tools {
            None => true,
            Some(tools) => tools.iter().any(|tool| tool == name || tool == "*"),
        }
    }

    /// Whether this policy still permits one more delegation depth.
    pub fn allows_child(&self) -> bool {
        self.max_depth == 0 || self.depth < self.max_depth
    }
}

/// One child task visible in a parent's subagent list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentSummary {
    /// Child task identity.
    pub task_id: String,
    /// Immediate parent task.
    pub parent_task_id: String,
    /// Root task of the lineage.
    pub root_task_id: String,
    /// Named definition.
    pub definition: String,
    /// Concrete agent the child task selects.
    pub agent: String,
    /// Current durable state.
    pub state: TaskState,
    /// Whether the parent admitted the child without waiting.
    pub background: bool,
    /// Delegation depth below the root.
    pub depth: u32,
    /// Whether the lineage owner closed this child.
    pub closed: bool,
    /// Admission time.
    pub created_at: i64,
    /// Last update time.
    pub updated_at: i64,
}

/// Structured merge of one child result.
///
/// The runtime builds this value from the durable child projection only. It
/// never synthesizes a successful output from a partial model stream, and it
/// keeps `Unknown`/`Recovering` visible instead of mapping them to failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentResult {
    /// Child task identity.
    pub task_id: String,
    /// Immediate parent task.
    pub parent_task_id: String,
    /// Root task of the lineage.
    pub root_task_id: String,
    /// Named definition.
    pub definition: String,
    /// Concrete agent that ran.
    pub agent: String,
    /// Current durable state.
    pub state: TaskState,
    /// Whether the state is terminal. A timed-out wait reports `false`.
    pub terminal: bool,
    /// Whether the child was admitted in the background.
    pub background: bool,
    /// Delegation depth below the root.
    pub depth: u32,
    /// Final output, truncated to the configured result ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Whether `output` was truncated.
    #[serde(default)]
    pub output_truncated: bool,
    /// Digest of the full output, so a truncated merge stays verifiable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_digest: Option<String>,
    /// Stable failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
    /// Why the state is uncertain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<String>,
    /// Child-reported usage and cost.
    #[serde(default)]
    pub usage: Usage,
    /// Concrete provider and model the child resolved at admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<ResolvedModel>,
}

impl SubagentResult {
    /// Project one durable child task into a bounded result.
    ///
    /// `max_bytes` bounds the merged output on a character boundary so a large
    /// child answer cannot exhaust the parent's context or event stream.
    pub fn from_task(task: &crate::task::Task, max_bytes: usize) -> Self {
        let policy = task.subagent.as_ref();
        let (output, output_truncated, output_digest) = match task.output.as_deref() {
            Some(text) => {
                let digest = crate::canonical_digest(&text);
                if text.len() > max_bytes && max_bytes > 0 {
                    let mut end = max_bytes.min(text.len());
                    while end > 0 && !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    (Some(text[..end].to_string()), true, Some(digest))
                } else {
                    (Some(text.to_string()), false, None)
                }
            }
            None => (None, false, None),
        };
        Self {
            task_id: task.id.clone(),
            parent_task_id: policy
                .map(|policy| policy.parent_task_id.clone())
                .or_else(|| task.parent_task_id.clone())
                .unwrap_or_default(),
            root_task_id: policy
                .map(|policy| policy.root_task_id.clone())
                .or_else(|| task.root_task_id.clone())
                .unwrap_or_default(),
            definition: policy
                .map(|policy| policy.definition.clone())
                .unwrap_or_default(),
            agent: task.agent.clone(),
            state: task.state,
            terminal: task.state.is_terminal(),
            background: policy.is_some_and(|policy| policy.background),
            depth: policy.map(|policy| policy.depth).unwrap_or(0),
            output,
            output_truncated,
            output_digest,
            error: task.error.clone(),
            uncertainty: task.uncertainty.clone(),
            usage: task.usage.clone(),
            resolved_model: task.resolved_model.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(output: Option<&str>) -> crate::task::Task {
        crate::task::Task {
            id: "task_child".into(),
            namespace: "local".into(),
            caller: "alice".into(),
            agent: "echo".into(),
            session_id: "sub_1".into(),
            input: "hello".into(),
            input_images: Vec::new(),
            idempotency_key: "sub:spawn:1".into(),
            request_digest: "sha256:x".into(),
            profile: "default".into(),
            profile_revision: 1,
            asset_revision: 1,
            trace_id: String::new(),
            traffic: "local".into(),
            resolved_model: None,
            team: None,
            team_version: None,
            team_roster: None,
            scopes: Vec::new(),
            delegation_path: Vec::new(),
            authority_expires_at: None,
            state: TaskState::Succeeded,
            attempt: 1,
            native_invocation_id: None,
            native_thread_id: None,
            native_a2a_task_id: None,
            output: output.map(str::to_string),
            evidence: None,
            error: None,
            uncertainty: None,
            limits: Default::default(),
            usage: Default::default(),
            created_at: 1,
            updated_at: 2,
            parent_task_id: Some("task_parent".into()),
            root_task_id: Some("task_parent".into()),
            subagent: Some(SubagentPolicy {
                parent_task_id: "task_parent".into(),
                root_task_id: "task_parent".into(),
                definition: "helper".into(),
                depth: 1,
                background: false,
                model: Some("echo".into()),
                tools: Some(vec!["read_file".into()]),
                permission_mode: Some("plan".into()),
                max_fanout: 0,
                max_depth: 0,
            }),
            closed_at: None,
        }
    }

    #[test]
    fn tool_allowlist_is_exact_and_none_inherits() {
        let policy = task(None).subagent.expect("policy");
        assert!(policy.allows_tool("read_file"));
        assert!(!policy.allows_tool("exec_command"));
        let mut empty = policy.clone();
        empty.tools = Some(Vec::new());
        assert!(!empty.allows_tool("read_file"));
        let mut inherited = policy;
        inherited.tools = None;
        assert!(inherited.allows_tool("exec_command"));
    }

    #[test]
    fn result_carries_provenance_and_bounds_output() {
        let result = SubagentResult::from_task(&task(Some("abcdef")), 3);
        assert_eq!(result.parent_task_id, "task_parent");
        assert_eq!(result.definition, "helper");
        assert_eq!(result.depth, 1);
        assert!(result.terminal);
        assert_eq!(result.output.as_deref(), Some("abc"));
        assert!(result.output_truncated);
        assert!(result.output_digest.is_some());

        let unknown = SubagentResult::from_task(
            &crate::task::Task {
                state: TaskState::Unknown,
                output: None,
                uncertainty: Some("effect unknown".into()),
                ..task(None)
            },
            1024,
        );
        assert!(!unknown.terminal);
        assert_eq!(unknown.state, TaskState::Unknown);
        assert_eq!(unknown.uncertainty.as_deref(), Some("effect unknown"));
        assert!(unknown.output.is_none());
    }

    #[test]
    fn child_depth_is_bounded_by_the_frozen_policy() {
        let mut policy = task(None).subagent.expect("policy");
        policy.max_depth = 2;
        policy.depth = 1;
        assert!(policy.allows_child());
        policy.depth = 2;
        assert!(!policy.allows_child());
    }
}
