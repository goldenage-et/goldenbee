//! Operator TOML configuration and resolved Worker settings.

use crate::mcp::{McpConfig, McpServerConfig};
use crate::worker::assets::Assets;
use crate::worker::WorkerResult;
use crate::{ProviderProfile, RuntimeConfig};
use common::{ApiError, CapabilityDescriptor, Principal};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Resolved operator configuration for one instance.
#[derive(Debug, Clone)]
pub struct WorkerSettings {
    /// Canonical configuration file path.
    pub config_path: PathBuf,
    /// Directory used to resolve relative paths.
    pub config_dir: PathBuf,
    /// Raw file values.
    pub file: FileConfig,
    /// Resolved agent directory.
    pub agents_dir: Option<PathBuf>,
    /// Resolved prompt directory.
    pub prompts_dir: Option<PathBuf>,
    /// Resolved skills directory.
    pub skills_dir: Option<PathBuf>,
    /// Resolved Worker command asset directory.
    pub commands_dir: Option<PathBuf>,
    /// Resolved plugin bundle root, when configured.
    pub plugins_dir: Option<PathBuf>,
    /// Additional skill roots resolved from trusted plugin bundles.
    pub(crate) plugin_skills: Vec<PathBuf>,
    /// Additional command roots resolved from trusted plugin bundles.
    pub(crate) plugin_commands: Vec<PathBuf>,
    /// Resolved storage directory.
    pub data_dir: PathBuf,
    /// Validated optional browser capability.
    pub browser: Option<crate::browser::BrowserSettings>,
    /// Process-wide tool pipeline, when package agent factories need it.
    pub(crate) kernel: Option<crate::tools::ToolKernel>,
}

impl WorkerSettings {
    pub(crate) fn load(path: &Path) -> WorkerResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            ApiError::new(
                "config.read_failed",
                format!("cannot read configuration '{}': {error}", path.display()),
            )
        })?;
        let file: FileConfig = toml::from_str(&text).map_err(|error| {
            ApiError::new(
                "config.parse_failed",
                format!(
                    "configuration '{}' is invalid TOML: {error}",
                    path.display()
                ),
            )
        })?;
        let config_dir = path
            .parent()
            .ok_or_else(|| {
                ApiError::new(
                    "config.path_invalid",
                    "configuration file has no parent directory",
                )
            })?
            .to_path_buf();
        let resolve = |value: &str| {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                config_dir.join(path)
            }
        };
        if let Some(gateway) = &file.gateway {
            if !gateway.url.starts_with("http://") && !gateway.url.starts_with("https://") {
                return Err(ApiError::new(
                    "config.invalid",
                    "gateway.url must be an HTTP or HTTPS base URL",
                ));
            }
            if gateway.control_token.trim().is_empty() {
                return Err(ApiError::new(
                    "config.invalid",
                    "gateway.control_token must not be empty",
                ));
            }
        }
        validate_models(&file)?;
        validate_mcp(&file.mcp)?;
        file.mcp.validate()?;
        file.permissions.validate().map_err(|message| {
            ApiError::new(
                "config.permissions",
                format!("permission policy is invalid: {message}"),
            )
        })?;
        validate_peers(&file.peers, file.allow_remote)?;
        validate_skills(&file.skills)?;
        validate_hooks(&file.hooks)?;
        validate_plugins(&file.plugins)?;
        Ok(Self {
            config_path: path.to_path_buf(),
            config_dir: config_dir.clone(),
            data_dir: resolve(&file.storage.directory),
            agents_dir: file.paths.agents.as_deref().map(resolve),
            prompts_dir: file.paths.prompts.as_deref().map(resolve),
            skills_dir: file.paths.skills.as_deref().map(resolve),
            commands_dir: file.paths.commands.as_deref().map(resolve),
            plugins_dir: file.plugins.directory.as_deref().map(resolve),
            plugin_skills: Vec::new(),
            plugin_commands: Vec::new(),
            browser: match &file.browser {
                Some(options) => Some(options.resolve(&config_dir)?),
                None => None,
            },
            kernel: None,
            file,
        })
    }

    /// Workspace used by generic file/search tools. Defaults below storage so
    /// an instance never writes into the package or configuration directory.
    pub(crate) fn tool_workspace(&self) -> std::path::PathBuf {
        match self.file.tools.workspace.as_deref() {
            Some(value) => self.resolve_path(value),
            None => self.data_dir.join("workspace"),
        }
    }

    /// Process-wide tool pipeline, when startup configured it.
    pub fn tool_kernel(&self) -> Option<crate::tools::ToolKernel> {
        self.kernel.clone()
    }

    /// Resolved skill disclosure behavior for this revision.
    pub(crate) fn skill_disclosure(&self) -> crate::agents::skills::SkillDisclosure {
        let skills = &self.file.skills;
        crate::agents::skills::SkillDisclosure {
            progressive: skills.disclosure == "progressive",
            max_instruction_chars: skills.max_instruction_chars,
            max_resource_chars: skills.max_resource_chars,
        }
    }

    /// Effective provider profiles: local-safe defaults overridden by operator
    /// configuration. No secret value is read here.
    pub(super) fn provider_profiles(&self) -> BTreeMap<String, ProviderProfile> {
        let mut providers = RuntimeConfig::local(self.data_dir.clone()).providers;
        for (id, profile) in &self.file.providers {
            providers.insert(id.clone(), profile.clone());
        }
        providers
    }

    /// Resolved MCP policy projection for revision validation and reload.
    pub(super) fn mcp_policies(&self) -> BTreeMap<String, crate::mcp::McpServerPolicy> {
        self.file
            .mcp
            .servers
            .iter()
            .map(|(id, server)| (id.clone(), server.policy()))
            .collect()
    }

    /// Resolved hook deadline policy.
    pub(crate) fn hook_policy(&self) -> crate::worker::HookPolicy {
        self.file.hooks.policy()
    }

    /// Merge trusted plugin assets into this revision.
    ///
    /// Plugin MCP ids and asset roots never replace a Worker-owned definition;
    /// the loader rejects collisions before this method runs.
    pub(crate) fn apply_plugins(
        &mut self,
        plugins: &crate::plugins::LoadedPlugins,
    ) -> WorkerResult<()> {
        self.plugin_skills = plugins.skills.clone();
        self.plugin_commands = plugins.commands.clone();
        for (id, server) in &plugins.mcp {
            if self.file.mcp.servers.contains_key(id) {
                return Err(ApiError::new(
                    "plugin.collision",
                    format!("plugin MCP server '{id}' would overwrite a Worker-owned definition"),
                ));
            }
            self.file.mcp.servers.insert(id.clone(), server.clone());
        }
        validate_mcp(&self.file.mcp)?;
        self.file.mcp.validate()?;
        Ok(())
    }

    pub(super) fn runtime_config(&self, name: &str, assets: &Assets) -> RuntimeConfig {
        let mut config = RuntimeConfig::local(self.data_dir.clone());
        let _ = name;
        config.instance_id = self.file.instance.clone();
        config.config_revision = self.file.config_revision.max(1);
        config.default_model = self.file.default_model.clone();
        config.providers = self.provider_profiles();
        config.allow_remote = self.file.allow_remote;
        config.agent_providers = assets.agent_providers.clone();
        config.agent_requirements = assets.agent_requirements.clone();
        config.tokens.clear();
        if let Some(token) = &self.file.token {
            config.tokens.insert(
                token.clone(),
                Principal::new(
                    "local",
                    self.file.namespace.clone(),
                    &self
                        .file
                        .token_scopes
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                ),
            );
        }
        for token in &self.file.tokens {
            config.tokens.insert(
                token.token.clone(),
                Principal::new(
                    token.principal.clone(),
                    token.namespace.clone(),
                    &token.scopes.iter().map(String::as_str).collect::<Vec<_>>(),
                ),
            );
        }
        config.skills = assets
            .skills
            .iter()
            .map(|skill| skill.name.clone())
            .collect();
        config.capabilities = assets
            .catalog
            .values()
            .map(|descriptor| CapabilityDescriptor {
                id: descriptor.id.clone(),
                kind: if descriptor.origin == crate::agents::AgentOrigin::Graph {
                    "workflow".into()
                } else {
                    "agent".into()
                },
                description: descriptor.description.clone(),
                permissions: Vec::new(),
            })
            .collect();
        config.instance_grants =
            std::sync::Arc::new(std::sync::RwLock::new(self.file.grants.clone()));
        config.mcp_policies = std::sync::Arc::new(std::sync::RwLock::new(self.mcp_policies()));
        config.permissions = match &self.kernel {
            Some(kernel) => kernel.permission_handle(),
            None => crate::policy::default_handle(),
        };
        config.mcp = self.file.mcp.clone();
        config.peers = self.file.peers.clone();
        for (id, server) in &self.file.mcp.servers {
            if !server.is_remote() {
                continue;
            }
            config.capabilities.push(CapabilityDescriptor {
                id: format!("mcp:{id}"),
                kind: "mcp".into(),
                description: "authorized remote MCP connection".into(),
                permissions: vec![server.grant_for(id)],
            });
        }
        config.routing_agents = assets.command_agents.keys().cloned().collect();
        config.max_active = self.file.max_active.max(1);
        config.max_queue = self.file.max_queue;
        config.per_principal = self.file.per_principal.max(1);
        config.event_retention = self.file.event_retention.max(1);
        config.drain_timeout_ms = self.file.drain_timeout_ms.max(1);
        config.max_streams = self.file.max_streams.max(1);
        config.stream_buffer = self.file.stream_buffer.max(1);
        config.stream_keepalive_ms = self.file.stream_keepalive_ms.max(1);
        config.retention = crate::RetentionPolicy {
            tasks_days: self.file.retention.tasks_days,
            sessions_days: self.file.retention.sessions_days,
            checkpoints_days: self.file.retention.checkpoints_days,
            events_days: self.file.retention.events_days,
            artifacts_days: self.file.retention.artifacts_days,
            memory_days: self.file.retention.memory_days,
            documents_days: self.file.retention.documents_days,
            audit_days: self.file.retention.audit_days,
            traces_days: self.file.retention.traces_days,
        };
        config
    }
}

/// Raw TOML configuration schema. Unknown fields are rejected so a typo does
/// not silently select a bundled or default asset directory.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    /// Optional package name check.
    #[serde(default)]
    pub name: Option<String>,
    /// Stable instance identity.
    #[serde(default = "default_instance")]
    pub instance: String,
    /// Authenticated namespace used for the development token.
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// Agent used when a command omits `--agent`.
    #[serde(default = "default_agent")]
    pub default_agent: String,
    /// Development token. Production credential references are a later slice.
    #[serde(default)]
    pub token: Option<String>,
    /// Scopes bound to the development token.
    #[serde(default = "default_scopes")]
    pub token_scopes: Vec<String>,
    /// Default loopback listener.
    #[serde(default = "default_addr")]
    pub addr: String,
    /// Asset directories.
    #[serde(default)]
    pub paths: PathsConfig,
    /// Instance storage.
    #[serde(default)]
    pub storage: StorageConfig,
    /// Package-defined settings exposed through context.
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    /// Additional token bindings for multi-caller tests and local development.
    #[serde(default)]
    pub tokens: Vec<TokenConfig>,
    /// Instance grants. Profile grants are intersected with these.
    #[serde(default = "default_grants")]
    pub grants: Vec<String>,
    /// Whether remote (non-loopback) providers were explicitly approved.
    #[serde(default)]
    pub allow_remote: bool,
    /// Logical provider used by newly bootstrapped default profiles.
    #[serde(default = "default_model")]
    pub default_model: String,
    /// Provider profiles selected by logical reference.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderProfile>,
    /// Configuration revision label carried by admitted tasks.
    #[serde(default = "default_revision")]
    pub config_revision: u64,
    /// Active execution ceiling.
    #[serde(default = "default_max_active")]
    pub max_active: usize,
    /// Pending queue ceiling.
    #[serde(default = "default_max_queue")]
    pub max_queue: usize,
    /// Per-principal admitted work ceiling.
    #[serde(default = "default_per_principal")]
    pub per_principal: usize,
    /// Durable event retention per stream.
    #[serde(default = "default_event_retention")]
    pub event_retention: usize,
    /// Per-owner retention policy. Missing values use conservative defaults.
    #[serde(default)]
    pub retention: RetentionConfig,
    /// Permission mode, ordered rules, and approval bounds.
    #[serde(default)]
    pub permissions: crate::policy::PermissionPolicy,
    /// Maximum time admitted work may finish after admission closes. The
    /// remaining work is left recoverable, not marked failed or succeeded.
    #[serde(default = "default_drain_timeout")]
    pub drain_timeout_ms: u64,
    /// Maximum concurrently open task event streams.
    #[serde(default = "default_max_streams")]
    pub max_streams: usize,
    /// Bounded live event broadcast capacity.
    #[serde(default = "default_stream_buffer")]
    pub stream_buffer: usize,
    /// SSE keepalive interval.
    #[serde(default = "default_stream_keepalive_ms")]
    pub stream_keepalive_ms: u64,
    /// Optional Gateway registration and lease heartbeat.
    #[serde(default)]
    pub gateway: Option<GatewayConfig>,
    /// Model credential source. Local provider configuration remains the
    /// default; `gateway` fetches encrypted per-Worker envelopes.
    #[serde(default)]
    pub models: ModelsConfig,
    /// MCP connections and owned credential references.
    #[serde(default)]
    pub mcp: McpConfig,
    /// Explicitly authorized static remote peers.
    #[serde(default)]
    pub peers: BTreeMap<String, crate::peer::PeerConfig>,
    /// Optional native browser capability.
    #[serde(default)]
    pub browser: Option<crate::browser::BrowserOptions>,
    /// Generic file/search tool workspace and runtime limits.
    #[serde(default)]
    pub tools: ToolsConfig,
    /// Skill disclosure and bounded skill output.
    #[serde(default)]
    pub skills: SkillsConfig,
    /// Hook deadline and external-hook output bounds.
    #[serde(default)]
    pub hooks: HooksConfig,
    /// Operator-selected plugin bundles and trust roots.
    #[serde(default)]
    pub plugins: crate::plugins::PluginsConfig,
}

/// Hook handler bounds.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HooksConfig {
    /// Maximum time one hook handler may run.
    #[serde(default = "default_hook_timeout_ms")]
    pub timeout_ms: u64,
    /// Maximum bytes retained from one external hook's stdout or stderr.
    #[serde(default = "default_hook_output_bytes")]
    pub max_output_bytes: usize,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            timeout_ms: default_hook_timeout_ms(),
            max_output_bytes: default_hook_output_bytes(),
        }
    }
}

impl HooksConfig {
    /// Resolved runtime policy.
    pub(crate) fn policy(self) -> crate::worker::HookPolicy {
        crate::worker::HookPolicy {
            timeout: std::time::Duration::from_millis(self.timeout_ms.max(1)),
            max_output_bytes: self.max_output_bytes.max(1),
        }
    }
}

fn default_hook_timeout_ms() -> u64 {
    5_000
}

fn default_hook_output_bytes() -> usize {
    65_536
}

fn validate_hooks(hooks: &HooksConfig) -> WorkerResult<()> {
    if hooks.timeout_ms == 0 || hooks.timeout_ms > 600_000 {
        return Err(ApiError::new(
            "config.hooks",
            "hooks.timeout_ms must be 1-600000",
        ));
    }
    if hooks.max_output_bytes == 0 || hooks.max_output_bytes > 16_777_216 {
        return Err(ApiError::new(
            "config.hooks",
            "hooks.max_output_bytes must be 1-16777216",
        ));
    }
    Ok(())
}

fn validate_plugins(plugins: &crate::plugins::PluginsConfig) -> WorkerResult<()> {
    for (name, pin) in &plugins.allow {
        if name.trim().is_empty() {
            return Err(ApiError::new(
                "config.plugins",
                "plugins.allow contains an empty plugin id",
            ));
        }
        if pin != "*" && !pin.starts_with("sha256:") {
            return Err(ApiError::new(
                "config.plugins",
                format!("plugins.allow['{name}'] must be '*' or a sha256 content digest"),
            ));
        }
    }
    for signer in plugins.trusted_signers.keys() {
        if signer.trim().is_empty() {
            return Err(ApiError::new(
                "config.plugins",
                "plugins.trusted_signers contains an empty signer id",
            ));
        }
    }
    Ok(())
}

/// Skill loading policy. Progressive disclosure is a package-declared
/// capability; configuration selects it but cannot create it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsConfig {
    /// `inherit` keeps the pinned eager selection/injection behavior.
    /// `progressive` exposes L1 summaries, L2 instructions, and L3 resources.
    #[serde(default = "default_skill_disclosure")]
    pub disclosure: String,
    /// Maximum characters returned by `load_skill`.
    #[serde(default = "default_skill_instruction_chars")]
    pub max_instruction_chars: usize,
    /// Maximum characters returned by `load_skill_resource`.
    #[serde(default = "default_skill_resource_chars")]
    pub max_resource_chars: usize,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            disclosure: default_skill_disclosure(),
            max_instruction_chars: default_skill_instruction_chars(),
            max_resource_chars: default_skill_resource_chars(),
        }
    }
}

fn default_skill_disclosure() -> String {
    "inherit".into()
}

fn default_skill_instruction_chars() -> usize {
    8_000
}

fn default_skill_resource_chars() -> usize {
    16_000
}

/// Generic file/search tool configuration. It narrows or configures compiled
/// package capabilities; it cannot create a tool the package did not declare.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    /// Workspace root. Relative paths resolve against the configuration file.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Per-call wall-clock timeout in milliseconds.
    #[serde(default = "default_tool_timeout_ms")]
    pub timeout_ms: u64,
    /// Serialized result ceiling in bytes.
    #[serde(default = "default_tool_output_bytes")]
    pub max_output_bytes: usize,
    /// Maximum concurrently executing wrapped tools.
    #[serde(default = "default_tool_parallel")]
    pub max_parallel: usize,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            workspace: None,
            timeout_ms: default_tool_timeout_ms(),
            max_output_bytes: default_tool_output_bytes(),
            max_parallel: default_tool_parallel(),
        }
    }
}

fn default_tool_timeout_ms() -> u64 {
    120_000
}

fn default_tool_output_bytes() -> usize {
    1_048_576
}

fn default_tool_parallel() -> usize {
    4
}

/// Optional per-owner retention overrides loaded from Worker configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionConfig {
    /// Terminal task metadata retention in days. Zero disables deletion.
    #[serde(default = "default_retention_days")]
    pub tasks_days: i64,
    /// Native session history retention in days.
    #[serde(default = "default_session_days")]
    pub sessions_days: i64,
    /// Native graph checkpoint retention in days.
    #[serde(default = "default_retention_days")]
    pub checkpoints_days: i64,
    /// Durable event retention in days.
    #[serde(default = "default_retention_days")]
    pub events_days: i64,
    /// Artifact metadata and content retention in days.
    #[serde(default = "default_retention_days")]
    pub artifacts_days: i64,
    /// Native long-term memory retention in days.
    #[serde(default = "default_retention_days")]
    pub memory_days: i64,
    /// Ingested document and derived-index retention in days.
    #[serde(default = "default_retention_days")]
    pub documents_days: i64,
    /// Security audit retention in days.
    #[serde(default = "default_audit_days")]
    pub audit_days: i64,
    /// Trace and telemetry retention in days.
    #[serde(default = "default_trace_days")]
    pub traces_days: i64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        let policy = crate::RetentionPolicy::default();
        Self {
            tasks_days: policy.tasks_days,
            sessions_days: policy.sessions_days,
            checkpoints_days: policy.checkpoints_days,
            events_days: policy.events_days,
            artifacts_days: policy.artifacts_days,
            memory_days: policy.memory_days,
            documents_days: policy.documents_days,
            audit_days: policy.audit_days,
            traces_days: policy.traces_days,
        }
    }
}

fn default_retention_days() -> i64 {
    30
}

fn default_session_days() -> i64 {
    90
}

fn default_audit_days() -> i64 {
    180
}

fn default_trace_days() -> i64 {
    7
}

/// Optional Gateway control-plane registration for this Worker Instance.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// Gateway base URL.
    pub url: String,
    /// Control token bound to this Worker identity by the Gateway operator.
    pub control_token: String,
    /// Lease renewal interval. The Gateway still measures expiry.
    #[serde(default = "default_renew_interval")]
    pub renew_interval_ms: u64,
    /// URL the Gateway should verify. Defaults to the bound listener.
    #[serde(default)]
    pub advertise_url: Option<String>,
}

fn default_renew_interval() -> u64 {
    15_000
}

/// Gateway-managed model provider configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsConfig {
    /// `local` (default) keeps `[providers]` and environment references.
    /// `gateway` fetches metadata and per-Worker encrypted credentials.
    #[serde(default = "default_models_source")]
    pub source: String,
    /// Gateway base URL. Defaults to `[gateway].url`.
    #[serde(default)]
    pub gateway_url: Option<String>,
    /// Worker control token. Defaults to `[gateway].control_token`.
    #[serde(default)]
    pub control_token: Option<String>,
    /// Gateway-assigned Worker id. Defaults to the configured instance.
    #[serde(default)]
    pub worker_id: Option<String>,
    /// X25519 private key path. Defaults to `<storage>/provider-key`.
    #[serde(default)]
    pub private_key_path: Option<String>,
    /// Encrypted provider bundle cache path. Defaults to
    /// `<storage>/provider-cache.json`.
    #[serde(default)]
    pub cache_path: Option<String>,
    /// Maximum age of a cached encrypted bundle before refresh.
    #[serde(default = "default_provider_refresh_ttl")]
    pub refresh_ttl_ms: u64,
    /// Behavior when the Gateway is unavailable and no unexpired cache exists:
    /// `deny` fails startup; `deterministic` uses the local deterministic
    /// adapter under the configured default model id.
    #[serde(default = "default_provider_fallback")]
    pub fallback: String,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            source: default_models_source(),
            gateway_url: None,
            control_token: None,
            worker_id: None,
            private_key_path: None,
            cache_path: None,
            refresh_ttl_ms: default_provider_refresh_ttl(),
            fallback: default_provider_fallback(),
        }
    }
}

fn default_models_source() -> String {
    "local".into()
}

pub(crate) fn default_provider_refresh_ttl() -> u64 {
    60_000
}

fn default_provider_fallback() -> String {
    "deny".into()
}

/// Resolved Gateway-managed model settings.
#[derive(Debug, Clone)]
pub struct ResolvedModels {
    /// Selected source. Always `gateway` for a resolved value.
    pub source: String,
    /// Gateway base URL.
    pub gateway_url: String,
    /// Worker control token. Never logged.
    pub control_token: String,
    /// Gateway-assigned Worker id.
    pub worker_id: String,
    /// Resolved X25519 private key path.
    pub private_key_path: PathBuf,
    /// Resolved encrypted cache path.
    pub cache_path: PathBuf,
    /// Cached-bundle refresh TTL.
    pub refresh_ttl_ms: u64,
    /// Explicit unavailable-Gateway policy.
    pub fallback: String,
}

impl WorkerSettings {
    /// Resolve Gateway-managed model settings. `None` keeps local provider
    /// configuration authoritative.
    pub fn models(&self) -> WorkerResult<Option<ResolvedModels>> {
        let models = &self.file.models;
        if models.source != "gateway" {
            return Ok(None);
        }
        let gateway_url = models
            .gateway_url
            .clone()
            .or_else(|| {
                self.file
                    .gateway
                    .as_ref()
                    .map(|gateway| gateway.url.clone())
            })
            .ok_or_else(|| {
                ApiError::new(
                    "config.models_gateway_url",
                    "models.source = \"gateway\" requires gateway_url or a [gateway] section",
                )
            })?;
        let control_token = models
            .control_token
            .clone()
            .or_else(|| {
                self.file
                    .gateway
                    .as_ref()
                    .map(|gateway| gateway.control_token.clone())
            })
            .ok_or_else(|| {
                ApiError::new(
                    "config.models_control_token",
                    "models.source = \"gateway\" requires control_token or a [gateway] section",
                )
            })?;
        let worker_id = models
            .worker_id
            .clone()
            .unwrap_or_else(|| self.file.instance.clone());
        let private_key_path = models
            .private_key_path
            .as_deref()
            .map(|value| self.resolve_path(value))
            .unwrap_or_else(|| self.data_dir.join("provider-key"));
        let cache_path = models
            .cache_path
            .as_deref()
            .map(|value| self.resolve_path(value))
            .unwrap_or_else(|| self.data_dir.join("provider-cache.json"));
        Ok(Some(ResolvedModels {
            source: models.source.clone(),
            gateway_url,
            control_token,
            worker_id,
            private_key_path,
            cache_path,
            refresh_ttl_ms: models.refresh_ttl_ms,
            fallback: models.fallback.clone(),
        }))
    }

    fn resolve_path(&self, value: &str) -> PathBuf {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            self.config_dir.join(path)
        }
    }
}

/// Configured asset directories.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathsConfig {
    /// Agent definition directory.
    #[serde(default)]
    pub agents: Option<String>,
    /// Prompt directory.
    #[serde(default)]
    pub prompts: Option<String>,
    /// Skill directory containing `*/SKILL.md`.
    #[serde(default)]
    pub skills: Option<String>,
    /// Worker-defined command directory containing `*.toml` commands.
    #[serde(default)]
    pub commands: Option<String>,
}

/// Configured storage.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// Worker Instance storage directory.
    #[serde(default = "default_storage")]
    pub directory: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            directory: default_storage(),
        }
    }
}

/// One additional authenticated token binding.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenConfig {
    /// Bearer token.
    pub token: String,
    /// Application namespace.
    pub namespace: String,
    /// Authenticated principal id.
    pub principal: String,
    /// Granted scopes. Defaults to all scopes for local development.
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
}

fn default_instance() -> String {
    format!("worker-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

fn default_namespace() -> String {
    "local".into()
}

fn default_agent() -> String {
    "general".into()
}

fn default_addr() -> String {
    "127.0.0.1:8080".into()
}

fn default_storage() -> String {
    "state".into()
}

fn default_model() -> String {
    "deterministic".into()
}

fn default_grants() -> Vec<String> {
    vec!["*".into()]
}

fn default_revision() -> u64 {
    1
}

fn default_max_active() -> usize {
    4
}

fn default_max_queue() -> usize {
    32
}

fn default_per_principal() -> usize {
    8
}

fn default_event_retention() -> usize {
    2_000
}

fn default_drain_timeout() -> u64 {
    10_000
}

fn default_max_streams() -> usize {
    crate::DEFAULT_MAX_STREAMS
}

fn default_stream_buffer() -> usize {
    crate::DEFAULT_STREAM_BUFFER
}

fn default_stream_keepalive_ms() -> u64 {
    crate::DEFAULT_STREAM_KEEPALIVE_MS
}

fn default_scopes() -> Vec<String> {
    vec!["*".into()]
}

fn validate_skills(skills: &SkillsConfig) -> WorkerResult<()> {
    if !matches!(skills.disclosure.as_str(), "inherit" | "progressive") {
        return Err(ApiError::new(
            "config.skill_disclosure",
            "skills.disclosure must be 'inherit' or 'progressive'",
        ));
    }
    if !(256..=100_000).contains(&skills.max_instruction_chars) {
        return Err(ApiError::new(
            "config.skill_limit",
            "skills.max_instruction_chars must be between 256 and 100000",
        ));
    }
    if !(256..=1_000_000).contains(&skills.max_resource_chars) {
        return Err(ApiError::new(
            "config.skill_limit",
            "skills.max_resource_chars must be between 256 and 1000000",
        ));
    }
    Ok(())
}

fn validate_models(file: &FileConfig) -> WorkerResult<()> {
    let models = &file.models;
    if !matches!(models.source.as_str(), "local" | "gateway") {
        return Err(ApiError::new(
            "config.models_source",
            "models.source must be 'local' or 'gateway'",
        ));
    }
    if !matches!(models.fallback.as_str(), "deny" | "deterministic") {
        return Err(ApiError::new(
            "config.models_fallback",
            "models.fallback must be 'deny' or 'deterministic'",
        ));
    }
    if !(1_000..=3_600_000).contains(&models.refresh_ttl_ms) {
        return Err(ApiError::new(
            "config.models_refresh_ttl",
            "models.refresh_ttl_ms must be between 1000 and 3600000",
        ));
    }
    if models.source == "gateway" {
        let url = models
            .gateway_url
            .as_deref()
            .or_else(|| file.gateway.as_ref().map(|gateway| gateway.url.as_str()));
        let Some(url) = url else {
            return Err(ApiError::new(
                "config.models_gateway_url",
                "models.source = \"gateway\" requires gateway_url or a [gateway] section",
            ));
        };
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(ApiError::new(
                "config.models_gateway_url",
                "models.gateway_url must be an HTTP or HTTPS base URL",
            ));
        }
        let token = models.control_token.as_deref().or_else(|| {
            file.gateway
                .as_ref()
                .map(|gateway| gateway.control_token.as_str())
        });
        if token.is_none_or(|token| token.trim().is_empty()) {
            return Err(ApiError::new(
                "config.models_control_token",
                "models.source = \"gateway\" requires a non-empty Worker control token",
            ));
        }
        if models
            .private_key_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(ApiError::new(
                "config.models_private_key",
                "models.private_key_path must not be empty",
            ));
        }
        if models
            .cache_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(ApiError::new(
                "config.models_cache_path",
                "models.cache_path must not be empty",
            ));
        }
    }
    Ok(())
}

fn validate_peers(
    peers: &BTreeMap<String, crate::peer::PeerConfig>,
    allow_remote: bool,
) -> WorkerResult<()> {
    for (id, peer) in peers {
        peer.validate(id, allow_remote)?;
    }
    Ok(())
}

fn validate_mcp(mcp: &McpConfig) -> WorkerResult<()> {
    for (id, server) in &mcp.servers {
        if !valid_id(id, 64) {
            return Err(ApiError::new(
                "config.invalid",
                format!("MCP server id '{id}' must be 1-64 characters of [A-Za-z0-9_-]"),
            ));
        }
        validate_mcp_server(id, server)?;
    }
    Ok(())
}

fn validate_mcp_server(id: &str, server: &McpServerConfig) -> WorkerResult<()> {
    if server.is_remote() {
        // Remote definitions are validated by `McpConfig::validate`, which
        // owns endpoint, credential, and destination policy.
        return Ok(());
    }
    if server.transport != "stdio" {
        return Err(ApiError::new(
            "mcp.transport_unsupported",
            format!("MCP server '{id}' must use the stdio transport"),
        ));
    }
    if server.command().trim().is_empty() || !Path::new(server.command()).is_absolute() {
        return Err(ApiError::new(
            "config.invalid",
            format!("MCP server '{id}' command must be an absolute executable path"),
        ));
    }
    for argument in &server.args {
        if looks_like_secret(argument) {
            return Err(ApiError::new(
                "config.secret_value_rejected",
                format!("MCP server '{id}' arguments must not carry secret values"),
            ));
        }
    }
    if server.allowed_tools.is_empty() {
        return Err(ApiError::new(
            "mcp.allowed_tools_required",
            format!("MCP server '{id}' must list at least one allowed tool"),
        ));
    }
    let mut allowed = std::collections::BTreeSet::new();
    for tool in &server.allowed_tools {
        if !valid_tool(tool) {
            return Err(ApiError::new(
                "config.invalid",
                format!("MCP server '{id}' has an invalid allowed tool name '{tool}'"),
            ));
        }
        if !allowed.insert(tool) {
            return Err(ApiError::new(
                "config.invalid",
                format!("MCP server '{id}' lists tool '{tool}' more than once"),
            ));
        }
    }
    for tool in &server.protected_tools {
        if !allowed.contains(tool) {
            return Err(ApiError::new(
                "config.invalid",
                format!("MCP server '{id}' protects tool '{tool}' but does not allow it"),
            ));
        }
    }
    if server.startup_timeout_ms == 0 || server.startup_timeout_ms > 300_000 {
        return Err(ApiError::new(
            "config.invalid",
            format!("MCP server '{id}' startup_timeout_ms must be 1-300000"),
        ));
    }
    if server.call_timeout_ms == 0 || server.call_timeout_ms > 3_600_000 {
        return Err(ApiError::new(
            "config.invalid",
            format!("MCP server '{id}' call_timeout_ms must be 1-3600000"),
        ));
    }
    if server.max_output_bytes == 0 || server.max_output_bytes > 16 * 1024 * 1024 {
        return Err(ApiError::new(
            "config.invalid",
            format!("MCP server '{id}' max_output_bytes must be 1-16777216"),
        ));
    }
    for (name, value) in &server.env {
        if name.trim().is_empty() || name.contains('=') || name.chars().any(char::is_control) {
            return Err(ApiError::new(
                "config.invalid",
                format!("MCP server '{id}' has an invalid environment name"),
            ));
        }
        if let Some(reference) = value.strip_prefix("env:") {
            if reference.trim().is_empty() {
                return Err(ApiError::new(
                    "config.invalid",
                    format!("MCP server '{id}' environment reference '{name}' is empty"),
                ));
            }
        } else if looks_like_secret(value) {
            return Err(ApiError::new(
                "config.secret_value_rejected",
                format!(
                    "MCP server '{id}' environment '{name}' must use an env: reference, not a value"
                ),
            ));
        }
    }
    if let Some(credential) = &server.credential {
        let is_reference = credential.starts_with("secret://")
            || credential.starts_with("env:")
            || credential.starts_with("vault:");
        if !is_reference || looks_like_secret(credential) {
            return Err(ApiError::new(
                "config.secret_value_rejected",
                format!(
                    "MCP server '{id}' credential must use a secret://, env:, or vault: reference"
                ),
            ));
        }
    }
    Ok(())
}

fn valid_id(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
}

fn valid_tool(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "*"
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character == '_'
                || character == '-'
                || character == '.'
        })
}

fn looks_like_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    value.starts_with("sk-")
        || value.starts_with("Bearer ")
        || value.starts_with("AKIA")
        || lower.contains("password=")
        || lower.contains("api_key=")
        || lower.contains("token=")
}

pub(super) fn canonical_config(path: &Path) -> WorkerResult<PathBuf> {
    std::fs::canonicalize(path).map_err(|error| {
        ApiError::new(
            "config.read_failed",
            format!("cannot open configuration '{}': {error}", path.display()),
        )
    })
}
