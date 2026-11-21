//! Ordered permission rules.
//!
//! A rule matches one tool request by tool name, command prefix, resolved
//! workspace path, or network domain. Patterns are operator data; no
//! model-supplied label participates in a match.

use super::PermissionRequest;
use serde::{Deserialize, Serialize};

/// Decision a matching rule requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleDecision {
    /// Execute without asking.
    Allow,
    /// Never execute.
    Deny,
    /// Ask the caller before executing.
    Ask,
}

impl RuleDecision {
    /// Tie-break rank within one priority: deny, then ask, then allow.
    fn rank(self) -> u8 {
        match self {
            Self::Deny => 3,
            Self::Ask => 2,
            Self::Allow => 1,
        }
    }
}

/// One ordered permission rule.
///
/// Empty predicate lists do not constrain that dimension. A rule that names
/// `paths` only matches requests with at least one matching resolved path; it
/// never matches a tool that resolves no path. The same rule shape matches a
/// command prefix or a network domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRule {
    /// Stable rule identity reported in audit and events.
    pub id: String,
    /// Decision this rule requests.
    pub decision: RuleDecision,
    /// Tool-name globs. Empty matches every tool.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Command argv prefixes. Every entry is tokenized, so both
    /// `["git", "status"]` and `["git status"]` describe the same prefix.
    #[serde(default)]
    pub command_prefix: Vec<String>,
    /// Workspace-relative path globs. A request matches when at least one
    /// resolved path matches one glob.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Network host globs, matched case-insensitively.
    #[serde(default)]
    pub domains: Vec<String>,
    /// Higher priority wins. Defaults to zero.
    #[serde(default)]
    pub priority: i32,
}

impl PermissionRule {
    /// Validate a rule before it can authorize anything.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("permission rule id must not be empty".into());
        }
        if self.id.len() > 100 {
            return Err(format!(
                "permission rule '{}' id must be at most 100 characters",
                self.id
            ));
        }
        if self
            .tools
            .iter()
            .chain(self.paths.iter())
            .chain(self.domains.iter())
            .any(|pattern| pattern.trim().is_empty())
        {
            return Err(format!(
                "permission rule '{}' contains an empty pattern",
                self.id
            ));
        }
        if !self.command_prefix.is_empty()
            && self
                .command_prefix
                .iter()
                .flat_map(|entry| command_tokens(entry))
                .next()
                .is_none()
        {
            return Err(format!(
                "permission rule '{}' has an empty command prefix",
                self.id
            ));
        }
        Ok(())
    }

    /// Whether this rule matches the exact request.
    pub fn matches(&self, request: &PermissionRequest) -> bool {
        if !self.tools.is_empty()
            && !self
                .tools
                .iter()
                .any(|pattern| glob_matches(pattern, &request.tool))
        {
            return false;
        }
        if !self.command_prefix.is_empty() {
            let Some(command) = request.command.as_deref() else {
                return false;
            };
            let wanted = self
                .command_prefix
                .iter()
                .flat_map(|entry| command_tokens(entry))
                .collect::<Vec<_>>();
            let observed = command_tokens(command);
            if !wanted.is_empty() && !observed.starts_with(&wanted) {
                return false;
            }
        }
        if !self.paths.is_empty()
            && !request
                .paths
                .iter()
                .any(|path| self.paths.iter().any(|pattern| glob_matches(pattern, path)))
        {
            return false;
        }
        if !self.domains.is_empty() {
            let Some(domain) = request.domain.as_deref() else {
                return false;
            };
            let domain = domain.to_ascii_lowercase();
            if !self
                .domains
                .iter()
                .any(|pattern| glob_matches(&pattern.to_ascii_lowercase(), &domain))
            {
                return false;
            }
        }
        true
    }

    pub(crate) fn outranks(&self, other: &Self) -> bool {
        self.priority > other.priority
            || (self.priority == other.priority && self.decision.rank() > other.decision.rank())
    }
}

/// Match one `*`/`?` glob. `*` matches any character sequence, including `/`,
/// so an operator must write explicit path segments when that distinction
/// matters.
pub fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let (mut pattern_index, mut value_index) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut resume = 0usize;
    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star = Some(pattern_index);
            pattern_index += 1;
            resume = value_index;
        } else if let Some(position) = star {
            pattern_index = position + 1;
            resume += 1;
            value_index = resume;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == '*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

/// Split a command into argv-like tokens without a shell.
///
/// Quotes group text; a backslash escapes the next character outside single
/// quotes and inside double quotes. No expansion, substitution, or globbing is
/// performed, so matching cannot be widened by shell syntax.
pub fn command_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    while let Some(character) = chars.next() {
        match quote {
            Some(active) => {
                if character == active {
                    quote = None;
                } else if character == '\\' && active == '"' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else {
                    current.push(character);
                }
            }
            None => {
                if character == '\\' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else if character == '\'' || character == '"' {
                    quote = Some(character);
                } else if character.is_whitespace() {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                } else {
                    current.push(character);
                }
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ActionEffect;

    fn request() -> PermissionRequest {
        PermissionRequest {
            tool: "exec_command".into(),
            capability: "exec:command".into(),
            effect: ActionEffect::None,
            high_risk: true,
            arguments_digest: "sha256:args".into(),
            paths: vec!["notes/a.txt".into()],
            command: Some("git status --short".into()),
            domain: Some("api.example.com".into()),
            principal: "alice".into(),
            profile_revision: 3,
            sandbox_profile: "workspace-write".into(),
        }
    }

    fn make_rule(decision: RuleDecision) -> PermissionRule {
        PermissionRule {
            id: "rule".into(),
            decision,
            tools: Vec::new(),
            command_prefix: Vec::new(),
            paths: Vec::new(),
            domains: Vec::new(),
            priority: 0,
        }
    }

    #[test]
    fn globs_match_tools_paths_and_domains() {
        assert!(glob_matches("write_*", "write_file"));
        assert!(glob_matches("notes/*.txt", "notes/a.txt"));
        assert!(!glob_matches("notes/*.txt", "other/a.txt"));
        assert!(glob_matches("*", "anything/at/all"));
        assert!(glob_matches("api.?.com", "api.x.com"));
        assert!(!glob_matches("api.?", "api.example.com"));
    }

    #[test]
    fn command_prefix_matches_argv_tokens_not_substrings() {
        let mut rule = make_rule(RuleDecision::Deny);
        rule.command_prefix = vec!["git status".into()];
        assert!(rule.matches(&request()));

        let mut other = request();
        other.command = Some("git statusx --short".into());
        assert!(!rule.matches(&other));
        other.command = Some("git".into());
        assert!(!rule.matches(&other));

        rule.command_prefix = vec!["git".into(), "status".into()];
        assert!(rule.matches(&request()));
    }

    #[test]
    fn command_tokenizer_respects_quotes_without_shell_expansion() {
        assert_eq!(
            command_tokens("git commit -m \"two words\""),
            vec!["git", "commit", "-m", "two words"]
        );
        assert_eq!(command_tokens("echo 'a b' c"), vec!["echo", "a b", "c"]);
        assert_eq!(command_tokens("echo $HOME"), vec!["echo", "$HOME"]);
    }

    #[test]
    fn predicates_require_an_observed_command_or_domain() {
        let mut rule = make_rule(RuleDecision::Deny);
        rule.command_prefix = vec!["git".into()];
        let mut read = request();
        read.command = None;
        assert!(!rule.matches(&read));

        let mut domain_rule = make_rule(RuleDecision::Deny);
        domain_rule.command_prefix.clear();
        domain_rule.domains = vec!["*.example.com".into()];
        assert!(domain_rule.matches(&request()));
        let mut local = request();
        local.domain = None;
        assert!(!domain_rule.matches(&local));
    }

    #[test]
    fn deny_ranks_above_ask_above_allow_at_equal_priority() {
        let allow = make_rule(RuleDecision::Allow);
        let ask = make_rule(RuleDecision::Ask);
        let deny = make_rule(RuleDecision::Deny);
        assert!(deny.outranks(&ask));
        assert!(ask.outranks(&allow));
        assert!(!allow.outranks(&ask));
    }

    #[test]
    fn validation_rejects_empty_identities_and_patterns() {
        assert!(make_rule(RuleDecision::Allow).validate().is_ok());
        let mut empty = make_rule(RuleDecision::Allow);
        empty.id = " ".into();
        assert!(empty.validate().is_err());
        let mut pattern = make_rule(RuleDecision::Allow);
        pattern.paths = vec![" ".into()];
        assert!(pattern.validate().is_err());
        let mut command = make_rule(RuleDecision::Allow);
        command.command_prefix = vec!["   ".into()];
        assert!(command.validate().is_err());
    }

    #[test]
    fn tool_and_path_predicates_scope_the_rule() {
        let mut rule = make_rule(RuleDecision::Allow);
        rule.tools = vec!["write_*".into()];
        rule.paths = vec!["notes/*".into()];
        let mut write = request();
        write.tool = "write_file".into();
        assert!(rule.matches(&write));
        let mut read = request();
        read.tool = "read_file".into();
        assert!(!rule.matches(&read));
        let mut outside = request();
        outside.paths = vec!["secrets/a.txt".into()];
        assert!(!rule.matches(&outside));
    }
}
