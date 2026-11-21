//! Runtime-owned sandbox policy: profile, network, paths, commands, proxy.
//!
//! The vocabulary follows Codex (`read-only`, `workspace-write`,
//! `danger-full-access`, protected metadata paths) but this is Goldenbee's own
//! data model. It is evaluated once per tool call by the runtime
//! [`ToolKernel`](crate::tools::ToolKernel) before permission authorization,
//! and it is projected onto the existing bubblewrap/prlimit boundary.
//!
//! Nothing above the operator configuration may widen these rules. A Worker
//! Package, Runtime Profile, plugin, command, or subagent can only supply a
//! narrower overlay through [`SandboxRules::narrowed_by`].

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path};

/// Metadata directories that stay unwritable under `workspace-write`.
pub const DEFAULT_PROTECTED_PATHS: [&str; 4] = [".git", ".agents", ".codex", ".aws"];

/// Default maximum shell command length accepted by a command tool.
pub const DEFAULT_COMMAND_MAX_BYTES: usize = 16 * 1024;

/// Filesystem write ceiling selected by the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxProfile {
    /// The workspace and every declared path are readable, never writable.
    ReadOnly,
    /// Writes are limited to authorized workspace roots, and protected
    /// metadata paths need an explicit grant.
    #[default]
    #[serde(alias = "workspace_write", alias = "workspace")]
    WorkspaceWrite,
    /// Writes are not narrowed by path policy. The OS boundary still confines
    /// the process, and every protected-path exception is recorded.
    #[serde(
        alias = "danger-full-access",
        alias = "danger_full_access",
        alias = "full_access",
        alias = "full"
    )]
    FullAccess,
}

impl SandboxProfile {
    /// Stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::FullAccess => "full-access",
        }
    }

    /// Permissiveness rank. Higher ranks are wider.
    pub fn rank(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::WorkspaceWrite => 1,
            Self::FullAccess => 2,
        }
    }

    /// Return the stricter of two profiles. Used by every narrowing overlay.
    pub fn narrowed(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }

    /// Whether the profile permits workspace mutations at all.
    pub fn allows_writes(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    /// Whether protected metadata paths need an explicit grant.
    pub fn protects_metadata(self) -> bool {
        !matches!(self, Self::FullAccess)
    }
}

/// Network ceiling for sandboxed tools and processes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkRules {
    /// Whether any network destination is reachable.
    pub enabled: bool,
    /// Optional destination allowlist. `None` means every host while enabled;
    /// `Some([])` means no host. `*.example.com` matches a DNS suffix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_hosts: Option<Vec<String>>,
    /// Explicit proxy the sandbox exports to processes. Values are
    /// configuration, never credential material.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// Hosts that bypass the proxy (`NO_PROXY`).
    pub proxy_bypass: Vec<String>,
}

impl NetworkRules {
    /// Whether a host is authorized by the destination allowlist.
    pub fn allows_host(&self, host: &str) -> bool {
        if !self.enabled {
            return false;
        }
        match &self.allow_hosts {
            None => true,
            Some(hosts) => hosts.iter().any(|pattern| host_matches(pattern, host)),
        }
    }

    /// Narrowing overlay: enabled is an AND, allowlists intersect, and a proxy
    /// can only be forced by the overlay, never removed.
    fn narrowed_by(&self, other: &NetworkRules) -> NetworkRules {
        let allow_hosts = match (&self.allow_hosts, &other.allow_hosts) {
            (None, None) => None,
            (None, Some(other)) => Some(other.clone()),
            (Some(base), None) => Some(base.clone()),
            (Some(base), Some(other)) => Some(
                base.iter()
                    .filter(|pattern| other.iter().any(|o| o.eq_ignore_ascii_case(pattern)))
                    .cloned()
                    .collect(),
            ),
        };
        let proxy = self.proxy.clone().or_else(|| other.proxy.clone());
        let proxy_bypass = if self.proxy_bypass.is_empty() || other.proxy_bypass.is_empty() {
            Vec::new()
        } else {
            self.proxy_bypass
                .iter()
                .filter(|host| other.proxy_bypass.iter().any(|o| o == *host))
                .cloned()
                .collect()
        };
        NetworkRules {
            enabled: self.enabled && other.enabled,
            allow_hosts,
            proxy,
            proxy_bypass,
        }
    }
}

/// One exact sandbox request observed from a wrapped tool call.
#[derive(Debug, Clone, Copy)]
pub struct SandboxRequest<'a> {
    /// Whether the call mutates the workspace.
    pub action: SandboxAction,
    /// Workspace-relative paths the call touches.
    pub paths: &'a [String],
    /// Command text, when the call executes one.
    pub command: Option<&'a str>,
    /// Network host, when the call declares one.
    pub domain: Option<&'a str>,
    /// Whether the tool requires network access even without a single host.
    pub network_tool: bool,
}

/// Mutation class used by sandbox path policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxAction {
    /// Reads are allowed whenever the path is inside the workspace.
    Read,
    /// Writes are subject to the profile, allowlist, and protected paths.
    Write,
}

/// Stable denial produced before a tool is dispatched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxViolation {
    /// Stable machine-readable policy code, for example `sandbox.read_only`.
    pub code: String,
    /// Redacted human-readable detail.
    pub message: String,
}

impl SandboxViolation {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for SandboxViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SandboxViolation {}

/// One action the profile allowed even though a stricter policy would block it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxException {
    /// Stable exception kind, for example `protected_path`.
    pub kind: String,
    /// Workspace-relative detail.
    pub detail: String,
}

/// Outcome of sandbox policy evaluation for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxDecision {
    /// The call may proceed. `exceptions` records anything the profile waived.
    Allow {
        /// Actions the selected profile allowed without a grant.
        exceptions: Vec<SandboxException>,
    },
    /// The call must not execute.
    Deny(SandboxViolation),
}

impl SandboxDecision {
    /// Whether the call may proceed.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    /// Waived actions, empty for a denial.
    pub fn exceptions(&self) -> &[SandboxException] {
        match self {
            Self::Allow { exceptions } => exceptions,
            Self::Deny(_) => &[],
        }
    }
}

/// Complete operator sandbox policy.
///
/// This value is resolved once at Worker startup. It is serialized in operator
/// configuration, hashed into task evidence, and projected onto the OS
/// boundary. Relative paths are workspace-relative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxRules {
    /// Filesystem write ceiling.
    pub profile: SandboxProfile,
    /// Network ceiling.
    pub network: NetworkRules,
    /// Authorized writable roots for `workspace-write`. `None` means the whole
    /// workspace; `Some([])` means no writable root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writable_roots: Option<Vec<String>>,
    /// Metadata path components that require an explicit grant.
    pub protected_paths: Vec<String>,
    /// Protected paths the operator explicitly granted.
    pub protected_grants: Vec<String>,
    /// Permitted command prefixes. `None` permits every command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_allow: Option<Vec<String>>,
    /// Maximum accepted shell command length in bytes.
    pub command_max_bytes: usize,
    /// Process and resource ceilings projected onto `prlimit`.
    pub limits: super::SandboxLimits,
}

impl Default for SandboxRules {
    fn default() -> Self {
        Self {
            profile: SandboxProfile::default(),
            network: NetworkRules::default(),
            writable_roots: None,
            protected_paths: DEFAULT_PROTECTED_PATHS
                .iter()
                .map(|path| (*path).to_string())
                .collect(),
            protected_grants: Vec::new(),
            command_allow: None,
            command_max_bytes: DEFAULT_COMMAND_MAX_BYTES,
            limits: super::SandboxLimits::default(),
        }
    }
}

impl SandboxRules {
    /// Reject rules that cannot be enforced deterministically.
    pub fn validate(&self) -> Result<(), String> {
        if self.command_max_bytes == 0 {
            return Err("sandbox.command_max_bytes must be greater than zero".into());
        }
        for root in self.writable_roots.iter().flatten() {
            validate_relative(root, "writable root")?;
        }
        for path in &self.protected_paths {
            validate_relative(path, "protected path")?;
        }
        for grant in &self.protected_grants {
            validate_relative(grant, "protected grant")?;
        }
        for prefix in self.command_allow.iter().flatten() {
            if prefix.trim().is_empty() {
                return Err("sandbox.command_allow entries must not be empty".into());
            }
        }
        if let Some(hosts) = &self.network.allow_hosts {
            for host in hosts {
                if host.trim().is_empty() || host.trim() == "*" {
                    return Err(
                        "sandbox.network.allow_hosts must list explicit hosts, not '*'".into(),
                    );
                }
            }
        }
        if let Some(proxy) = &self.network.proxy {
            if proxy.trim().is_empty() || proxy.chars().any(char::is_whitespace) {
                return Err("sandbox.network.proxy must be a URL without whitespace".into());
            }
        }
        for host in &self.network.proxy_bypass {
            if host.trim().is_empty() {
                return Err("sandbox.network.proxy_bypass entries must not be empty".into());
            }
        }
        self.limits.validate().map_err(|code| {
            format!("sandbox limits are invalid ({code}); every limit must be greater than zero")
        })?;
        Ok(())
    }

    /// Canonical digest of the effective rules.
    pub fn digest(&self) -> String {
        common::canonical_digest(self)
    }

    /// Evaluate one exact tool call.
    pub fn evaluate(&self, request: &SandboxRequest<'_>) -> SandboxDecision {
        if let Some(command) = request.command {
            if let Err(violation) = self.check_command(command) {
                return SandboxDecision::Deny(violation);
            }
        }
        if request.action == SandboxAction::Write && !self.profile.allows_writes() {
            return SandboxDecision::Deny(SandboxViolation::new(
                "sandbox.read_only",
                "the read-only sandbox profile blocks workspace writes",
            ));
        }
        if (request.network_tool || request.domain.is_some()) && !self.network.enabled {
            return SandboxDecision::Deny(SandboxViolation::new(
                "sandbox.network_disabled",
                "network access is disabled by the sandbox policy",
            ));
        }
        if let Some(domain) = request.domain {
            if !self.network.allows_host(domain) {
                return SandboxDecision::Deny(SandboxViolation::new(
                    "sandbox.network_denied",
                    "the destination is not in the sandbox network allowlist",
                ));
            }
        }
        let mut exceptions = Vec::new();
        for path in request.paths {
            if let Err(violation) = validate_observed_path(path) {
                return SandboxDecision::Deny(violation);
            }
            for component in path_components(path) {
                if !self.is_protected(component) || self.is_granted(component) {
                    continue;
                }
                if self.profile.protects_metadata() {
                    return SandboxDecision::Deny(SandboxViolation::new(
                        "sandbox.protected_path",
                        "the path is protected metadata and has no explicit grant",
                    ));
                }
                exceptions.push(SandboxException {
                    kind: "protected_path".into(),
                    detail: path.clone(),
                });
            }
            if request.action == SandboxAction::Write
                && self.profile == SandboxProfile::WorkspaceWrite
                && !self.writable(path)
            {
                return SandboxDecision::Deny(SandboxViolation::new(
                    "sandbox.path_denied",
                    "the write path is outside the authorized workspace roots",
                ));
            }
        }
        SandboxDecision::Allow { exceptions }
    }

    /// Check one command against the length and prefix ceilings.
    pub fn check_command(&self, command: &str) -> Result<(), SandboxViolation> {
        if command.len() > self.command_max_bytes {
            return Err(SandboxViolation::new(
                "sandbox.command_limit",
                "the command exceeds the sandbox command length ceiling",
            ));
        }
        if let Some(prefixes) = &self.command_allow {
            let command = command.trim_start();
            if !prefixes
                .iter()
                .any(|prefix| command.starts_with(prefix.trim()))
            {
                return Err(SandboxViolation::new(
                    "sandbox.command_denied",
                    "the command is not in the sandbox command allowlist",
                ));
            }
        }
        Ok(())
    }

    /// Return rules that never widen `self`.
    ///
    /// A Runtime Profile, plugin, command, or subagent supplies an overlay; the
    /// result is the intersection. There is no `widen` operation.
    pub fn narrowed_by(&self, overlay: &SandboxRules) -> SandboxRules {
        SandboxRules {
            profile: self.profile.narrowed(overlay.profile),
            network: self.network.narrowed_by(&overlay.network),
            writable_roots: narrow_roots(
                self.writable_roots.as_deref(),
                overlay.writable_roots.as_deref(),
            ),
            protected_paths: union_paths(&self.protected_paths, &overlay.protected_paths),
            protected_grants: self
                .protected_grants
                .iter()
                .filter(|grant| overlay.protected_grants.iter().any(|other| other == *grant))
                .cloned()
                .collect(),
            command_allow: narrow_prefixes(
                self.command_allow.as_deref(),
                overlay.command_allow.as_deref(),
            ),
            command_max_bytes: self.command_max_bytes.min(overlay.command_max_bytes),
            limits: self.limits.narrowed_by(&overlay.limits),
        }
    }

    /// Bounded JSON summary recorded in task evidence and audit.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "profile": self.profile.as_str(),
            "digest": self.digest(),
            "network": {
                "enabled": self.network.enabled,
                "allow_hosts": self.network.allow_hosts,
                "proxy": self.network.proxy.is_some(),
            },
            "writable_roots": self.writable_roots,
            "protected_paths": self.protected_paths,
            "protected_grants": self.protected_grants.len(),
            "command_allow": self.command_allow.as_ref().map(Vec::len),
            "command_max_bytes": self.command_max_bytes,
            "limits": {
                "timeout_ms": self.limits.timeout.as_millis() as u64,
                "max_output_bytes": self.limits.max_output_bytes,
                "memory_bytes": self.limits.memory_bytes,
                "cpu_seconds": self.limits.cpu_seconds,
                "processes": self.limits.max_processes,
                "file_bytes": self.limits.max_file_bytes,
                "open_files": self.limits.max_open_files,
            },
        })
    }

    fn is_protected(&self, component: &str) -> bool {
        self.protected_paths
            .iter()
            .any(|path| path == component || path.ends_with(&format!("/{component}")))
    }

    fn is_granted(&self, component: &str) -> bool {
        self.protected_grants
            .iter()
            .any(|path| path == component || path.ends_with(&format!("/{component}")))
    }

    fn writable(&self, path: &str) -> bool {
        let Some(roots) = &self.writable_roots else {
            return true;
        };
        roots.iter().any(|root| path_within(path, root))
    }
}

fn validate_relative(value: &str, kind: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.trim().is_empty() {
        return Err(format!("sandbox {kind} entries must not be empty"));
    }
    if path.is_absolute() {
        return Err(format!("sandbox {kind} entries must be workspace-relative"));
    }
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(_) => {}
            Component::ParentDir => {
                return Err(format!("sandbox {kind} entries must not contain '..'"))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("sandbox {kind} entries must be workspace-relative"))
            }
        }
    }
    Ok(())
}

fn validate_observed_path(path: &str) -> Result<(), SandboxViolation> {
    if path.trim().is_empty() {
        return Err(SandboxViolation::new(
            "sandbox.path_denied",
            "the tool declared an empty path",
        ));
    }
    let candidate = Path::new(path);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(SandboxViolation::new(
            "sandbox.path_denied",
            "the path escapes the workspace root",
        ));
    }
    Ok(())
}

fn path_components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
}

fn path_within(path: &str, root: &str) -> bool {
    let root = root.trim().trim_matches('/');
    let path = path.trim().trim_matches('/');
    if root.is_empty() {
        return true;
    }
    path == root
        || (path.len() > root.len()
            && path.starts_with(root)
            && path.as_bytes().get(root.len()) == Some(&b'/'))
}

fn host_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    match pattern.strip_prefix("*.") {
        Some(suffix) => host == suffix || host.ends_with(&format!(".{suffix}")),
        None => pattern == host,
    }
}

fn union_paths(left: &[String], right: &[String]) -> Vec<String> {
    let mut paths = left.to_vec();
    for path in right {
        if !paths.iter().any(|candidate| candidate == path) {
            paths.push(path.clone());
        }
    }
    paths
}

fn narrow_roots(base: Option<&[String]>, overlay: Option<&[String]>) -> Option<Vec<String>> {
    match (base, overlay) {
        (None, None) => None,
        (None, Some(overlay)) => Some(overlay.to_vec()),
        (Some(base), None) => Some(base.to_vec()),
        (Some(base), Some(overlay)) => Some(
            base.iter()
                .filter(|root| overlay.iter().any(|other| path_within(root, other)))
                .cloned()
                .collect(),
        ),
    }
}

fn narrow_prefixes(base: Option<&[String]>, overlay: Option<&[String]>) -> Option<Vec<String>> {
    // A prefix can be kept only when a narrower overlay also authorizes every
    // command it would match. When the intersection is empty nothing is
    // authorized, which `Some([])` expresses distinctly from `None`.
    let narrowed = match (base, overlay) {
        (None, None) => None,
        (None, Some(overlay)) => Some(overlay.to_vec()),
        (Some(base), None) => Some(base.to_vec()),
        (Some(base), Some(overlay)) => Some(
            base.iter()
                .filter(|prefix| {
                    overlay
                        .iter()
                        .any(|other| prefix.trim().starts_with(other.trim()))
                })
                .cloned()
                .collect(),
        ),
    };
    narrowed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(profile: SandboxProfile) -> SandboxRules {
        SandboxRules {
            profile,
            ..SandboxRules::default()
        }
    }

    fn request<'a>(
        action: SandboxAction,
        paths: &'a [String],
        command: Option<&'a str>,
        domain: Option<&'a str>,
        network_tool: bool,
    ) -> SandboxRequest<'a> {
        SandboxRequest {
            action,
            paths,
            command,
            domain,
            network_tool,
        }
    }

    #[test]
    fn read_only_blocks_writes_and_keeps_reads() {
        let rules = rules(SandboxProfile::ReadOnly);
        let paths = vec!["notes/a.txt".to_string()];
        assert!(rules
            .evaluate(&request(SandboxAction::Read, &paths, None, None, false))
            .is_allowed());
        let denied = rules.evaluate(&request(SandboxAction::Write, &paths, None, None, false));
        assert_eq!(
            denied,
            SandboxDecision::Deny(SandboxViolation::new(
                "sandbox.read_only",
                "the read-only sandbox profile blocks workspace writes"
            ))
        );
    }

    #[test]
    fn workspace_write_rejects_protected_metadata_without_a_grant() {
        let rules = rules(SandboxProfile::WorkspaceWrite);
        for path in [".git/config", "sub/.git/config", ".aws/credentials"] {
            let paths = vec![path.to_string()];
            let denied = rules.evaluate(&request(SandboxAction::Write, &paths, None, None, false));
            assert!(
                matches!(&denied, SandboxDecision::Deny(violation) if violation.code == "sandbox.protected_path"),
                "{path}: {denied:?}"
            );
        }
        let granted = SandboxRules {
            protected_grants: vec![".git".into()],
            ..rules
        };
        let paths = vec![".git/config".to_string()];
        assert!(granted
            .evaluate(&request(SandboxAction::Write, &paths, None, None, false))
            .is_allowed());
    }

    #[test]
    fn workspace_write_honors_the_root_allowlist() {
        let rules = SandboxRules {
            writable_roots: Some(vec!["notes".into()]),
            ..rules(SandboxProfile::WorkspaceWrite)
        };
        let allowed = vec!["notes/a.txt".to_string()];
        assert!(rules
            .evaluate(&request(SandboxAction::Write, &allowed, None, None, false))
            .is_allowed());
        let denied = vec!["other/a.txt".to_string()];
        assert!(matches!(
            rules.evaluate(&request(SandboxAction::Write, &denied, None, None, false)),
            SandboxDecision::Deny(violation) if violation.code == "sandbox.path_denied"
        ));
    }

    #[test]
    fn writes_outside_the_workspace_are_denied() {
        let rules = rules(SandboxProfile::FullAccess);
        for path in ["../etc/passwd", "/etc/passwd"] {
            let paths = vec![path.to_string()];
            assert!(matches!(
                rules.evaluate(&request(SandboxAction::Write, &paths, None, None, false)),
                SandboxDecision::Deny(violation) if violation.code == "sandbox.path_denied"
            ));
        }
    }

    #[test]
    fn full_access_records_protected_path_exceptions() {
        let rules = rules(SandboxProfile::FullAccess);
        let paths = vec![".git/config".to_string()];
        let decision = rules.evaluate(&request(SandboxAction::Write, &paths, None, None, false));
        assert!(decision.is_allowed());
        assert_eq!(decision.exceptions().len(), 1);
        assert_eq!(decision.exceptions()[0].kind, "protected_path");
    }

    #[test]
    fn network_disabled_denies_network_tools_and_hosts() {
        let rules = rules(SandboxProfile::WorkspaceWrite);
        let no_paths: Vec<String> = Vec::new();
        let denied = rules.evaluate(&request(
            SandboxAction::Read,
            &no_paths,
            None,
            Some("example.com"),
            false,
        ));
        assert!(matches!(
            denied,
            SandboxDecision::Deny(violation) if violation.code == "sandbox.network_disabled"
        ));
        let denied = rules.evaluate(&request(SandboxAction::Read, &no_paths, None, None, true));
        assert!(matches!(
            denied,
            SandboxDecision::Deny(violation) if violation.code == "sandbox.network_disabled"
        ));
    }

    #[test]
    fn network_allowlist_limits_enabled_network() {
        let rules = SandboxRules {
            network: NetworkRules {
                enabled: true,
                allow_hosts: Some(vec!["*.example.com".into()]),
                ..NetworkRules::default()
            },
            ..rules(SandboxProfile::WorkspaceWrite)
        };
        let no_paths: Vec<String> = Vec::new();
        assert!(rules
            .evaluate(&request(
                SandboxAction::Read,
                &no_paths,
                None,
                Some("api.example.com"),
                false
            ))
            .is_allowed());
        assert!(matches!(
            rules.evaluate(&request(
                SandboxAction::Read,
                &no_paths,
                None,
                Some("evil.test"),
                false
            )),
            SandboxDecision::Deny(violation) if violation.code == "sandbox.network_denied"
        ));
    }

    #[test]
    fn command_ceiling_denies_unlisted_and_oversized_commands() {
        let rules = SandboxRules {
            command_allow: Some(vec!["git status".into(), "ls".into()]),
            command_max_bytes: 32,
            ..rules(SandboxProfile::WorkspaceWrite)
        };
        let no_paths: Vec<String> = Vec::new();
        assert!(rules
            .evaluate(&request(
                SandboxAction::Read,
                &no_paths,
                Some("git status --short"),
                None,
                false
            ))
            .is_allowed());
        assert!(matches!(
            rules.evaluate(&request(
                SandboxAction::Read,
                &no_paths,
                Some("git push"),
                None,
                false
            )),
            SandboxDecision::Deny(violation) if violation.code == "sandbox.command_denied"
        ));
        assert!(matches!(
            rules.evaluate(&request(
                SandboxAction::Read,
                &no_paths,
                Some("ls -la /a/very/long/path/that/exceeds/the/ceiling"),
                None,
                false
            )),
            SandboxDecision::Deny(violation) if violation.code == "sandbox.command_limit"
        ));
    }

    #[test]
    fn narrowing_never_widens_a_profile_network_path_command_or_limit() {
        let base = SandboxRules {
            profile: SandboxProfile::WorkspaceWrite,
            network: NetworkRules {
                enabled: true,
                allow_hosts: Some(vec!["api.example.com".into(), "cdn.example.com".into()]),
                proxy: Some("http://127.0.0.1:3128".into()),
                proxy_bypass: vec!["localhost".into()],
            },
            writable_roots: Some(vec!["notes".into(), "src".into()]),
            protected_paths: vec![".git".into()],
            protected_grants: vec![".git".into()],
            command_allow: Some(vec!["git status".into(), "git push".into()]),
            command_max_bytes: 1024,
            limits: SandboxRules::default().limits,
        };
        // A wider overlay changes nothing.
        let wider = SandboxRules {
            profile: SandboxProfile::FullAccess,
            network: NetworkRules {
                enabled: true,
                allow_hosts: None,
                proxy: None,
                proxy_bypass: Vec::new(),
            },
            writable_roots: None,
            protected_paths: Vec::new(),
            protected_grants: vec![".git".into(), ".aws".into()],
            command_allow: None,
            command_max_bytes: 4096,
            limits: SandboxRules::default().limits,
        };
        let narrowed = base.narrowed_by(&wider);
        assert_eq!(narrowed.profile, SandboxProfile::WorkspaceWrite);
        assert!(narrowed.network.enabled);
        assert_eq!(
            narrowed.network.allow_hosts,
            Some(vec!["api.example.com".into(), "cdn.example.com".into()])
        );
        assert_eq!(narrowed.writable_roots, base.writable_roots);
        assert_eq!(narrowed.command_max_bytes, 1024);
        assert!(narrowed.protected_paths.iter().any(|path| path == ".git"));

        // A narrower overlay intersects every dimension.
        let narrower = SandboxRules {
            profile: SandboxProfile::ReadOnly,
            network: NetworkRules {
                enabled: false,
                allow_hosts: Some(vec!["api.example.com".into()]),
                proxy: None,
                proxy_bypass: vec!["localhost".into()],
            },
            writable_roots: Some(vec!["notes".into()]),
            protected_paths: vec![".aws".into()],
            protected_grants: vec![".git".into()],
            command_allow: Some(vec!["git status".into()]),
            command_max_bytes: 128,
            limits: base.limits.clone(),
        };
        let narrowed = base.narrowed_by(&narrower);
        assert_eq!(narrowed.profile, SandboxProfile::ReadOnly);
        assert!(!narrowed.network.enabled);
        assert_eq!(
            narrowed.network.allow_hosts,
            Some(vec!["api.example.com".into()])
        );
        assert_eq!(narrowed.writable_roots, Some(vec!["notes".into()]));
        assert_eq!(narrowed.protected_grants, vec![".git".to_string()]);
        assert_eq!(narrowed.command_allow, Some(vec!["git status".to_string()]));
        assert_eq!(narrowed.command_max_bytes, 128);
        assert!(narrowed.limits.max_processes <= base.limits.max_processes);
    }

    #[test]
    fn an_empty_intersection_authorizes_nothing() {
        let base = SandboxRules {
            command_allow: Some(vec!["git status".into()]),
            writable_roots: Some(vec!["notes".into()]),
            ..rules(SandboxProfile::WorkspaceWrite)
        };
        let overlay = SandboxRules {
            command_allow: Some(vec!["cargo ".into()]),
            writable_roots: Some(vec!["src".into()]),
            ..rules(SandboxProfile::WorkspaceWrite)
        };
        let narrowed = base.narrowed_by(&overlay);
        assert_eq!(narrowed.command_allow, Some(Vec::new()));
        assert_eq!(narrowed.writable_roots, Some(Vec::new()));
        let no_paths: Vec<String> = Vec::new();
        assert!(matches!(
            narrowed.evaluate(&request(
                SandboxAction::Read,
                &no_paths,
                Some("git status"),
                None,
                false
            )),
            SandboxDecision::Deny(violation) if violation.code == "sandbox.command_denied"
        ));
        let paths = vec!["notes/a.txt".to_string()];
        assert!(matches!(
            narrowed.evaluate(&request(SandboxAction::Write, &paths, None, None, false)),
            SandboxDecision::Deny(violation) if violation.code == "sandbox.path_denied"
        ));
    }

    #[test]
    fn validation_rejects_unsafe_rules() {
        let empty_command = SandboxRules {
            command_max_bytes: 0,
            ..SandboxRules::default()
        };
        assert!(empty_command.validate().is_err());
        let traversal = SandboxRules {
            writable_roots: Some(vec!["../escape".into()]),
            ..SandboxRules::default()
        };
        assert!(traversal.validate().is_err());
        let wildcard = SandboxRules {
            network: NetworkRules {
                enabled: true,
                allow_hosts: Some(vec!["*".into()]),
                ..NetworkRules::default()
            },
            ..SandboxRules::default()
        };
        assert!(wildcard.validate().is_err());
        assert!(SandboxRules::default().validate().is_ok());
    }
}
