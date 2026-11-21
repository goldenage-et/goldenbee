//! Permission modes and the deterministic authorization decision.
//!
//! A mode is a ceiling and a default. Ordered rules may narrow it (ask or
//! deny) and may upgrade an ask to an allow, but a mode that denies an action
//! cannot be widened by a rule.

use super::rules::{PermissionRule, RuleDecision};
use serde::{Deserialize, Serialize};

/// Effect class the policy reasons about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionEffect {
    /// No known side effect.
    None,
    /// Creates or overwrites workspace content.
    FileWrite,
    /// Removes workspace content.
    FileDelete,
    /// A mutation the policy does not know more precisely.
    Other,
}

impl ActionEffect {
    /// Whether this effect mutates external state.
    pub fn is_mutation(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Exact tool request presented to the policy engine.
///
/// Every field is runtime-observed. `tool` comes from the wrapped tool, paths
/// are resolved against the configured workspace, and command and domain come
/// from descriptor extractors, never from a model-declared label.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    /// Wrapped tool name.
    pub tool: String,
    /// Capability declared by the tool descriptor.
    pub capability: String,
    /// Observed effect class.
    pub effect: ActionEffect,
    /// Whether the descriptor classifies the call as high risk.
    pub high_risk: bool,
    /// Canonical digest of the exact arguments.
    pub arguments_digest: String,
    /// Workspace-relative paths the call resolves against.
    pub paths: Vec<String>,
    /// Command text, when the tool executes one.
    pub command: Option<String>,
    /// Network host, when the tool reaches one.
    pub domain: Option<String>,
    /// Principal that owns the task, or `local` outside an admitted task.
    pub principal: String,
    /// Pinned profile revision, or zero outside an admitted task.
    pub profile_revision: u64,
    /// Sandbox profile in force for the call.
    pub sandbox_profile: String,
}

impl PermissionRequest {
    /// Durable approval identity for this exact action.
    pub fn approval_action(&self) -> common::ApprovalAction {
        common::ApprovalAction {
            tool: self.tool.clone(),
            arguments_digest: self.arguments_digest.clone(),
            paths: self.paths.clone(),
            command: self.command.clone(),
            domain: self.domain.clone(),
            sandbox_profile: self.sandbox_profile.clone(),
            principal: self.principal.clone(),
            profile_revision: self.profile_revision,
        }
    }

    /// Canonical digest binding every field an approver saw.
    pub fn action_digest(&self) -> String {
        self.approval_action().digest()
    }

    fn command_or_network(&self) -> bool {
        self.command.is_some() || self.domain.is_some()
    }
}

/// Operator-selected permission mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    /// Reads run; mutations, commands, network calls, and high-risk tools ask.
    #[default]
    Default,
    /// Like [`Default`](Self::Default), but file writes and edits run without
    /// asking. Deletes, commands, network calls, and high-risk tools still ask.
    #[serde(alias = "acceptEdits", alias = "accept_edits")]
    AcceptEdits,
    /// Only read-only calls run. Mutations, commands, network calls, and
    /// high-risk tools are denied.
    Plan,
    /// Every call runs unless an explicit deny rule matches. Ask rules are
    /// auto-approved because the mode exists to remove prompts.
    #[serde(alias = "full_auto", alias = "fullauto", alias = "yolo")]
    FullAuto,
}

impl PermissionMode {
    /// Stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "accept-edits",
            Self::Plan => "plan",
            Self::FullAuto => "full-auto",
        }
    }

    fn base(self, request: &PermissionRequest) -> (Decision, &'static str) {
        let risky =
            request.effect.is_mutation() || request.high_risk || request.command_or_network();
        match self {
            Self::Plan => {
                if risky {
                    (Decision::Deny, "mode_plan_ceiling")
                } else {
                    (Decision::Allow, "mode_read_only")
                }
            }
            Self::Default => {
                if request.effect.is_mutation() {
                    (Decision::Ask, "mode_mutation")
                } else if request.command_or_network() {
                    (Decision::Ask, "mode_command_or_network")
                } else if request.high_risk {
                    (Decision::Ask, "mode_high_risk")
                } else {
                    (Decision::Allow, "mode_read_only")
                }
            }
            Self::AcceptEdits => {
                if request.effect == ActionEffect::FileWrite && !request.high_risk {
                    (Decision::Allow, "mode_file_edits")
                } else if request.effect.is_mutation() || request.command_or_network() {
                    (Decision::Ask, "mode_mutation")
                } else if request.high_risk {
                    (Decision::Ask, "mode_high_risk")
                } else {
                    (Decision::Allow, "mode_read_only")
                }
            }
            Self::FullAuto => (Decision::Allow, "mode_full_auto"),
        }
    }
}

/// Authorization outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Decision {
    /// The call may execute.
    Allow,
    /// The call must not execute.
    Deny,
    /// The caller must decide the exact action first.
    Ask,
}

impl Decision {
    /// Stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Ask => "ask",
        }
    }
}

/// Final policy decision with the mode, rule, and reason that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    /// Deterministic authorization decision.
    pub decision: Decision,
    /// Mode in force when the decision was made.
    pub mode: PermissionMode,
    /// Matching rule identity, when a rule decided the outcome.
    pub rule: Option<String>,
    /// Stable reason code.
    pub reason: String,
}

impl PolicyDecision {
    pub(super) fn new(
        decision: Decision,
        mode: PermissionMode,
        rule: Option<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            decision,
            mode,
            rule,
            reason: reason.into(),
        }
    }
}

/// Evaluate ordered rules over a mode decision.
///
/// The highest-priority matching rule group decides. Inside one priority a
/// deny outranks an ask, which outranks an allow. Ties keep the first rule in
/// configuration order, so evaluation is deterministic.
pub(super) fn evaluate(
    mode: PermissionMode,
    rules: &[PermissionRule],
    request: &PermissionRequest,
) -> PolicyDecision {
    let mut matched: Option<&PermissionRule> = None;
    for rule in rules {
        if !rule.matches(request) {
            continue;
        }
        match matched {
            Some(current) if !rule.outranks(current) => {}
            _ => matched = Some(rule),
        }
    }
    let (base, base_reason) = mode.base(request);
    let Some(rule) = matched else {
        return PolicyDecision::new(base, mode, None, base_reason);
    };
    let rule_id = Some(rule.id.clone());
    match (rule.decision, base) {
        (RuleDecision::Deny, _) => PolicyDecision::new(Decision::Deny, mode, rule_id, "rule_deny"),
        (RuleDecision::Allow, Decision::Deny) => {
            // A mode that denies an action is a ceiling. An allow rule cannot
            // widen plan mode or any future restricted mode.
            PolicyDecision::new(Decision::Deny, mode, rule_id, "mode_ceiling")
        }
        (RuleDecision::Allow, _) => {
            PolicyDecision::new(Decision::Allow, mode, rule_id, "rule_allow")
        }
        (RuleDecision::Ask, Decision::Deny) => {
            PolicyDecision::new(Decision::Deny, mode, rule_id, "mode_ceiling")
        }
        (RuleDecision::Ask, _) if mode == PermissionMode::FullAuto => {
            PolicyDecision::new(Decision::Allow, mode, rule_id, "mode_full_auto")
        }
        (RuleDecision::Ask, _) => PolicyDecision::new(Decision::Ask, mode, rule_id, "rule_ask"),
    }
}
