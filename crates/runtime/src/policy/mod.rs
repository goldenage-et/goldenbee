//! Permission modes and ordered allow/deny/ask rules.
//!
//! The policy is operator data. One decision function runs before every tool
//! dispatch in the runtime [`ToolKernel`](crate::tools::ToolKernel), for
//! package tools, built-in tools, MCP toolsets, profile-built agents, and
//! package agent factories. A policy snapshot is taken per call so a mode or
//! rule change affects only later calls and is visible in task evidence.

pub mod decision;
pub mod rules;

pub use decision::{ActionEffect, Decision, PermissionMode, PermissionRequest, PolicyDecision};
pub use rules::{PermissionRule, RuleDecision};

use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

/// Milliseconds an ask decision blocks inside the tool call by default.
pub const DEFAULT_APPROVAL_WAIT_MS: u64 = 30_000;
/// Lifetime of a permission approval by default.
pub const DEFAULT_APPROVAL_TTL_MS: u64 = 15 * 60_000;

fn default_wait_ms() -> u64 {
    DEFAULT_APPROVAL_WAIT_MS
}

fn default_ttl_ms() -> u64 {
    DEFAULT_APPROVAL_TTL_MS
}

/// Complete operator permission policy.
///
/// The schema is Goldenbee's own. It borrows the ordered-priority rule shape
/// from the Gemini CLI policy engine and the command-prefix/network-host rule
/// concepts from Codex without parsing either vendor's policy files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionPolicy {
    /// Mode in force.
    #[serde(default)]
    pub mode: PermissionMode,
    /// Bound on the in-tool approval wait. Zero means "emit a truthful
    /// pending outcome immediately" and is intended for headless clients that
    /// decide through the approval route.
    #[serde(default = "default_wait_ms")]
    pub approval_wait_ms: u64,
    /// Approval lifetime. A decision after expiry cannot authorize a retry.
    #[serde(default = "default_ttl_ms")]
    pub approval_ttl_ms: u64,
    /// Ordered rules. Higher priority wins; within one priority deny beats ask
    /// beats allow.
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        Self {
            mode: PermissionMode::default(),
            approval_wait_ms: default_wait_ms(),
            approval_ttl_ms: default_ttl_ms(),
            rules: Vec::new(),
        }
    }
}

impl PermissionPolicy {
    /// Reject a policy that cannot be enforced deterministically.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.approval_ttl_ms == 0 {
            return Err("permissions.approval_ttl_ms must be greater than zero".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for rule in &self.rules {
            rule.validate()?;
            if !seen.insert(rule.id.as_str()) {
                return Err(format!("permission rule '{}' is declared twice", rule.id));
            }
        }
        Ok(())
    }

    /// Evaluate this policy against one exact request.
    pub fn evaluate(&self, request: &PermissionRequest) -> PolicyDecision {
        decision::evaluate(self.mode, &self.rules, request)
    }

    /// Canonical digest of the policy data.
    pub fn digest(&self) -> String {
        common::canonical_digest(self)
    }
}

/// Immutable per-call view of the effective policy.
#[derive(Debug, Clone)]
pub struct PolicySnapshot {
    /// Monotonic policy revision. Starts at one and increments on replacement.
    pub revision: u64,
    /// Digest of the whole policy.
    pub digest: String,
    policy: PermissionPolicy,
}

impl PolicySnapshot {
    /// Effective mode.
    pub fn mode(&self) -> PermissionMode {
        self.policy.mode
    }

    /// Bound on the in-tool approval wait.
    pub fn approval_wait_ms(&self) -> u64 {
        self.policy.approval_wait_ms
    }

    /// Approval lifetime.
    pub fn approval_ttl_ms(&self) -> u64 {
        self.policy.approval_ttl_ms
    }

    /// Evaluate the snapshot against one request.
    pub fn evaluate(&self, request: &PermissionRequest) -> PolicyDecision {
        self.policy.evaluate(request)
    }

    /// Bounded JSON summary recorded in task evidence and audit.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "revision": self.revision,
            "digest": self.digest,
            "mode": self.policy.mode.as_str(),
            "rule_count": self.policy.rules.len(),
        })
    }
}

/// Mutable policy owner plus its revision counter.
#[derive(Debug)]
pub struct PolicyState {
    revision: u64,
    policy: PermissionPolicy,
}

impl PolicyState {
    /// Create revision one from a validated policy.
    pub fn new(policy: PermissionPolicy) -> Self {
        Self {
            revision: 1,
            policy,
        }
    }

    /// Current revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Immutable snapshot of the current policy.
    pub fn snapshot(&self) -> PolicySnapshot {
        PolicySnapshot {
            revision: self.revision,
            digest: self.policy.digest(),
            policy: self.policy.clone(),
        }
    }

    /// Replace the policy for subsequent calls.
    ///
    /// The revision always advances, even when the new policy has the same
    /// digest, so evidence records that an operator decision was applied.
    pub fn replace(&mut self, policy: PermissionPolicy) -> PolicySnapshot {
        self.revision = self.revision.saturating_add(1);
        self.policy = policy;
        self.snapshot()
    }
}

/// Shared policy handle used by the kernel and the runtime.
pub type PolicyHandle = Arc<RwLock<PolicyState>>;

/// Create a configured policy handle.
pub fn handle(policy: PermissionPolicy) -> PolicyHandle {
    Arc::new(RwLock::new(PolicyState::new(policy)))
}

/// Create the default policy handle.
pub fn default_handle() -> PolicyHandle {
    handle(PermissionPolicy::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(tool: &str, effect: ActionEffect, high_risk: bool) -> PermissionRequest {
        PermissionRequest {
            tool: tool.into(),
            capability: "workspace:write".into(),
            effect,
            high_risk,
            arguments_digest: "sha256:args".into(),
            paths: vec!["notes/a.txt".into()],
            command: None,
            domain: None,
            principal: "alice".into(),
            profile_revision: 2,
            sandbox_profile: "workspace-write".into(),
        }
    }

    fn rule(id: &str, decision: RuleDecision, priority: i32) -> PermissionRule {
        PermissionRule {
            id: id.into(),
            decision,
            tools: Vec::new(),
            command_prefix: Vec::new(),
            paths: Vec::new(),
            domains: Vec::new(),
            priority,
        }
    }

    fn make_policy(mode: PermissionMode, rules: Vec<PermissionRule>) -> PermissionPolicy {
        PermissionPolicy {
            mode,
            rules,
            ..PermissionPolicy::default()
        }
    }

    #[test]
    fn default_mode_allows_reads_and_asks_for_writes_and_commands() {
        let policy = make_policy(PermissionMode::Default, Vec::new());
        assert_eq!(
            policy
                .evaluate(&request("read_file", ActionEffect::None, false))
                .decision,
            Decision::Allow
        );
        assert_eq!(
            policy
                .evaluate(&request("write_file", ActionEffect::FileWrite, false))
                .decision,
            Decision::Ask
        );
        assert_eq!(
            policy
                .evaluate(&request("delete_file", ActionEffect::FileDelete, false))
                .decision,
            Decision::Ask
        );
        let mut command = request("exec_command", ActionEffect::None, true);
        command.command = Some("git status".into());
        assert_eq!(policy.evaluate(&command).decision, Decision::Ask);
        let mut domain = request("web_fetch", ActionEffect::None, false);
        domain.domain = Some("example.com".into());
        assert_eq!(policy.evaluate(&domain).decision, Decision::Ask);
        assert_eq!(
            policy
                .evaluate(&request("package_medium", ActionEffect::None, false))
                .decision,
            Decision::Allow
        );
    }

    #[test]
    fn accept_edits_allows_file_writes_but_asks_for_high_risk_and_commands() {
        let policy = make_policy(PermissionMode::AcceptEdits, Vec::new());
        assert_eq!(
            policy
                .evaluate(&request("write_file", ActionEffect::FileWrite, false))
                .decision,
            Decision::Allow
        );
        assert_eq!(
            policy
                .evaluate(&request("apply_patch", ActionEffect::FileWrite, true))
                .decision,
            Decision::Ask
        );
        assert_eq!(
            policy
                .evaluate(&request("delete_file", ActionEffect::FileDelete, false))
                .decision,
            Decision::Ask
        );
    }

    #[test]
    fn plan_mode_denies_mutations_commands_and_network_without_a_task() {
        let policy = make_policy(PermissionMode::Plan, Vec::new());
        assert_eq!(
            policy
                .evaluate(&request("read_file", ActionEffect::None, false))
                .decision,
            Decision::Allow
        );
        assert_eq!(
            policy
                .evaluate(&request("write_file", ActionEffect::FileWrite, false))
                .decision,
            Decision::Deny
        );
        let mut command = request("exec_command", ActionEffect::None, true);
        command.command = Some("git status".into());
        assert_eq!(policy.evaluate(&command).decision, Decision::Deny);
        let mut domain = request("web_fetch", ActionEffect::None, false);
        domain.domain = Some("example.com".into());
        assert_eq!(policy.evaluate(&domain).decision, Decision::Deny);
    }

    #[test]
    fn full_auto_allows_everything_except_an_explicit_deny() {
        let policy = make_policy(PermissionMode::FullAuto, Vec::new());
        assert_eq!(
            policy
                .evaluate(&request("write_file", ActionEffect::FileWrite, true))
                .decision,
            Decision::Allow
        );
        let mut asked = rule("ask-any", RuleDecision::Ask, 0);
        asked.tools = vec!["write_file".into()];
        let policy = make_policy(PermissionMode::FullAuto, vec![asked]);
        assert_eq!(
            policy
                .evaluate(&request("write_file", ActionEffect::FileWrite, false))
                .decision,
            Decision::Allow
        );
        let mut denied = rule("deny-secret", RuleDecision::Deny, 0);
        denied.paths = vec!["secrets/*".into()];
        let policy = make_policy(PermissionMode::FullAuto, vec![denied]);
        let mut secret = request("write_file", ActionEffect::FileWrite, false);
        secret.paths = vec!["secrets/key.pem".into()];
        assert_eq!(policy.evaluate(&secret).decision, Decision::Deny);
    }

    #[test]
    fn an_allow_rule_upgrades_an_ask_but_cannot_widen_plan_mode() {
        let mut allow = rule("allow-notes", RuleDecision::Allow, 0);
        allow.paths = vec!["notes/*".into()];
        let default = make_policy(PermissionMode::Default, vec![allow.clone()]);
        assert_eq!(
            default
                .evaluate(&request("write_file", ActionEffect::FileWrite, false))
                .decision,
            Decision::Allow
        );
        let plan = make_policy(PermissionMode::Plan, vec![allow]);
        assert_eq!(
            plan.evaluate(&request("write_file", ActionEffect::FileWrite, false))
                .decision,
            Decision::Deny
        );
    }

    #[test]
    fn higher_priority_allow_beats_lower_priority_deny() {
        let mut allow = rule("allow", RuleDecision::Allow, 10);
        allow.tools = vec!["write_file".into()];
        let mut deny = rule("deny", RuleDecision::Deny, 0);
        deny.tools = vec!["*".into()];
        let policy = make_policy(PermissionMode::Default, vec![deny, allow]);
        let decision = policy.evaluate(&request("write_file", ActionEffect::FileWrite, false));
        assert_eq!(decision.decision, Decision::Allow);
        assert_eq!(decision.rule.as_deref(), Some("allow"));
    }

    #[test]
    fn same_priority_deny_beats_ask_beats_allow() {
        let mut allow = rule("allow", RuleDecision::Allow, 0);
        allow.tools = vec!["write_file".into()];
        let mut ask = rule("ask", RuleDecision::Ask, 0);
        ask.tools = vec!["write_file".into()];
        let deny = rule("deny", RuleDecision::Deny, 0);
        let decision = make_policy(PermissionMode::Default, vec![allow, ask, deny.clone()])
            .evaluate(&request("write_file", ActionEffect::FileWrite, false));
        assert_eq!(decision.decision, Decision::Deny);
        assert_eq!(decision.rule.as_deref(), Some("deny"));

        let decision = make_policy(
            PermissionMode::Default,
            vec![rule("allow", RuleDecision::Allow, 0), {
                let mut ask = rule("ask", RuleDecision::Ask, 0);
                ask.tools = vec!["write_file".into()];
                ask
            }],
        )
        .evaluate(&request("write_file", ActionEffect::FileWrite, false));
        assert_eq!(decision.decision, Decision::Ask);
        assert_eq!(decision.rule.as_deref(), Some("ask"));
    }

    #[test]
    fn command_prefix_rule_matches_only_the_observed_command() {
        let mut allow = rule("allow-status", RuleDecision::Allow, 0);
        allow.command_prefix = vec!["git status".into()];
        let policy = make_policy(PermissionMode::Default, vec![allow]);
        let mut command = request("exec_command", ActionEffect::None, true);
        command.command = Some("git status --short".into());
        assert_eq!(policy.evaluate(&command).decision, Decision::Allow);
        command.command = Some("git push".into());
        assert_eq!(policy.evaluate(&command).decision, Decision::Ask);
    }

    #[test]
    fn domain_rule_matches_the_observed_host() {
        let mut allow = rule("allow-api", RuleDecision::Allow, 5);
        allow.domains = vec!["api.example.com".into()];
        let policy = make_policy(PermissionMode::Default, vec![allow]);
        let mut fetch = request("web_fetch", ActionEffect::None, false);
        fetch.domain = Some("api.example.com".into());
        assert_eq!(policy.evaluate(&fetch).decision, Decision::Allow);
        fetch.domain = Some("evil.example.com".into());
        assert_eq!(policy.evaluate(&fetch).decision, Decision::Ask);
    }

    #[test]
    fn approval_identity_binds_every_approver_visible_field() {
        let base = request("write_file", ActionEffect::FileWrite, false);
        let digest = base.action_digest();
        let mut changed = base.clone();
        changed.tool = "edit_file".into();
        assert_ne!(digest, changed.action_digest());
        let mut changed = base.clone();
        changed.arguments_digest = "sha256:other".into();
        assert_ne!(digest, changed.action_digest());
        let mut changed = base.clone();
        changed.paths = vec!["notes/b.txt".into()];
        assert_ne!(digest, changed.action_digest());
        let mut changed = base.clone();
        changed.command = Some("git status".into());
        assert_ne!(digest, changed.action_digest());
        let mut changed = base.clone();
        changed.domain = Some("example.com".into());
        assert_ne!(digest, changed.action_digest());
        let mut changed = base.clone();
        changed.sandbox_profile = "read-only".into();
        assert_ne!(digest, changed.action_digest());
        let mut changed = base.clone();
        changed.principal = "bob".into();
        assert_ne!(digest, changed.action_digest());
        let mut changed = base.clone();
        changed.profile_revision = 9;
        assert_ne!(digest, changed.action_digest());
        assert_eq!(digest, base.action_digest());
    }

    #[test]
    fn policy_snapshot_revision_advances_on_change() {
        let handle = handle(make_policy(PermissionMode::Default, Vec::new()));
        let first = handle.read().unwrap().snapshot();
        assert_eq!(first.revision, 1);
        let second = handle
            .write()
            .unwrap()
            .replace(make_policy(PermissionMode::FullAuto, Vec::new()));
        assert_eq!(second.revision, 2);
        assert_ne!(first.digest, second.digest);
        assert_eq!(second.mode(), PermissionMode::FullAuto);
    }

    #[test]
    fn validation_rejects_duplicate_ids_and_zero_ttl() {
        let mut first = rule("dup", RuleDecision::Allow, 0);
        first.tools = vec!["a".into()];
        let mut second = rule("dup", RuleDecision::Deny, 0);
        second.tools = vec!["b".into()];
        let duplicate = make_policy(PermissionMode::Default, vec![first, second]);
        assert!(duplicate.validate().is_err());
        let mut zero = make_policy(PermissionMode::Default, Vec::new());
        zero.approval_ttl_ms = 0;
        assert!(zero.validate().is_err());
    }
}
