//! Runtime configuration and local-safe defaults.

use crate::mcp::McpServerPolicy;
use common::{scopes, CapabilityDescriptor, Principal};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Provider capability declaration used for model preflight and construction.
///
/// Fields are operator configuration, never secret values. `credential` is a
/// reference such as `env:NAME`; it is resolved only when a model is built and
/// is never stored on a task, event, or inspection view.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderProfile {
    /// Native adapter kind: `deterministic`, `http-json`, `ollama`,
    /// `openai-compatible`, or `openrouter`.
    #[serde(default = "default_adapter")]
    pub adapter: String,
    /// Concrete model identifier sent to the provider.
    pub model: String,
    /// Endpoint base URL for endpoint-backed adapters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Supported modalities.
    #[serde(default)]
    pub modalities: Vec<String>,
    /// Whether function calling is supported.
    #[serde(default)]
    pub tool_calling: bool,
    /// Whether the provider is local.
    #[serde(default)]
    pub local: bool,
    /// Residency label.
    #[serde(default)]
    pub residency: String,
    /// Declared context window, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Credential reference, never a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    /// Relative cost rank. Higher is more expensive; a fallback may not raise it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_rank: Option<u32>,
    /// Operator-declared input price in millionths of a currency unit per
    /// million tokens. Used only for harness-side cost estimates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cost_micros_per_million: Option<u64>,
    /// Operator-declared candidate price in millionths of a currency unit per
    /// million tokens. Used only for harness-side cost estimates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_cost_micros_per_million: Option<u64>,
    /// Data-handling class. A fallback may not change it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_class: Option<String>,
    /// Explicit fallback destinations allowed by policy.
    #[serde(default)]
    pub fallbacks: Vec<String>,
}

fn default_adapter() -> String {
    "deterministic".into()
}

impl ProviderProfile {
    /// Whether this adapter reads a network endpoint.
    pub fn endpoint_backed(&self) -> bool {
        matches!(
            self.adapter.as_str(),
            "http-json" | "ollama" | "openai-compatible" | "openrouter"
        )
    }

    /// Whether this adapter requires a credential reference.
    pub fn requires_credential(&self) -> bool {
        matches!(self.adapter.as_str(), "openai-compatible" | "openrouter")
    }
}

/// Per-agent requirements surfaced to admission-time provider preflight.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AgentRequirement {
    /// Number of directly bound tools.
    #[serde(default)]
    pub tools: usize,
    /// Number of bound native toolsets.
    #[serde(default)]
    pub toolsets: usize,
}

/// Retention policy per authoritative store. Zero disables deletion for that
/// category, but unfinished recovery state is never discarded.
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    /// Terminal task retention in days.
    pub tasks_days: i64,
    /// Session retention in days. Native session history is owned by the
    /// native session store; applying this policy to it belongs to the
    /// retention slice rather than the harness metadata store.
    pub sessions_days: i64,
    /// Native graph checkpoint retention in days. The newest checkpoint of an
    /// unfinished task is never removed.
    pub checkpoints_days: i64,
    /// Durable event retention in days.
    pub events_days: i64,
    /// Artifact retention in days.
    pub artifacts_days: i64,
    /// Native long-term memory retention in days. Memory is owned by the
    /// native memory adapter; applying this policy to it belongs to the
    /// retention slice rather than the harness metadata store.
    pub memory_days: i64,
    /// Ingested document and derived-index retention in days.
    pub documents_days: i64,
    /// Audit retention in days.
    pub audit_days: i64,
    /// Trace and telemetry retention in days.
    pub traces_days: i64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            tasks_days: 30,
            sessions_days: 90,
            checkpoints_days: 30,
            events_days: 30,
            artifacts_days: 30,
            memory_days: 30,
            documents_days: 30,
            audit_days: 180,
            traces_days: 7,
        }
    }
}

/// Default number of concurrently open event streams per Worker process.
pub const DEFAULT_MAX_STREAMS: usize = 64;
/// Default bounded broadcast capacity for live task events.
pub const DEFAULT_STREAM_BUFFER: usize = 512;
/// Default SSE keepalive interval.
pub const DEFAULT_STREAM_KEEPALIVE_MS: u64 = 15_000;

/// Process configuration. Secrets are references, not values, except the local
/// development token which is explicitly restricted to loopback listeners.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Stable instance identity.
    pub instance_id: String,
    /// Installed package identity.
    pub package: String,
    /// Installed package version.
    pub package_version: String,
    /// Process incarnation. Changes on every start.
    pub incarnation: String,
    /// Current configuration revision.
    pub config_revision: u64,
    /// Data directory.
    pub data_dir: PathBuf,
    /// Token bindings.
    pub tokens: BTreeMap<String, Principal>,
    /// Registered binary capabilities.
    pub capabilities: Vec<CapabilityDescriptor>,
    /// Internal command routes admitted by this revision but not advertised as
    /// capabilities. Every route resolves to a prebuilt, narrowed command agent.
    pub routing_agents: std::collections::BTreeSet<String>,
    /// Instance grants. A validated reload narrows these for future profiles.
    pub instance_grants: Arc<RwLock<Vec<String>>>,
    /// Discovered native skill names available to profiles.
    pub skills: Vec<String>,
    /// Active execution ceiling.
    pub max_active: usize,
    /// Pending queue ceiling.
    pub max_queue: usize,
    /// Per-principal queued+running ceiling.
    pub per_principal: usize,
    /// Reserved admission capacity for runtime-local subagent tasks.
    ///
    /// Children are exempt from their parent's per-principal quota but bounded
    /// by this reserved reserve, so a parent that already owns its full quota
    /// can still admit the child it waits on.
    pub max_subagents: usize,
    /// Durable event retention per stream.
    pub event_retention: usize,
    /// Maximum time admitted work may finish after admission closes.
    pub drain_timeout_ms: u64,
    /// Maximum concurrently open event streams in this process.
    pub max_streams: usize,
    /// Bounded broadcast capacity for live task events.
    pub stream_buffer: usize,
    /// SSE keepalive interval.
    pub stream_keepalive_ms: u64,
    /// Worker lease duration.
    pub lease_ttl_ms: i64,
    /// Trusted signer public keys, base64 encoded.
    pub trusted_signers: BTreeMap<String, String>,
    /// Logical provider selected for newly bootstrapped default profiles.
    pub default_model: String,
    /// Provider policies.
    pub providers: BTreeMap<String, ProviderProfile>,
    /// Requirements of file-loaded agents, used for admission preflight.
    pub agent_requirements: BTreeMap<String, AgentRequirement>,
    /// Provider selected by package-registered manual agents.
    pub agent_providers: BTreeMap<String, String>,
    /// Configured local MCP connection policy, including disabled entries.
    /// Interior mutability lets a validated reload disable a connection for
    /// future admissions without rebuilding the process configuration.
    pub mcp_policies: Arc<RwLock<BTreeMap<String, McpServerPolicy>>>,
    /// Effective permission policy shared with the tool kernel.
    ///
    /// A validated replacement applies to subsequent tool calls and advances
    /// the recorded policy revision.
    pub permissions: crate::policy::PolicyHandle,
    /// Authorized MCP servers and owned credential references.
    pub mcp: crate::mcp::McpConfig,
    /// Explicitly authorized static remote peers. Presence is the
    /// authorization decision; no Gateway discovery is required.
    pub peers: BTreeMap<String, crate::peer::PeerConfig>,
    /// Gateway-to-Worker signing secret.
    pub delegation_secret: Vec<u8>,
    /// Whether remote (non-loopback) listeners were explicitly approved.
    pub allow_remote: bool,
    /// Explicit local development mode: unauthenticated native ADK UI and A2A
    /// routes are allowed from loopback peers. Harness `/v1` routes always
    /// require a token.
    pub dev_ui: bool,
    /// Whether TLS termination is configured by the deployment.
    pub tls_configured: bool,
    /// Retention policy.
    pub retention: RetentionPolicy,
}

impl RuntimeConfig {
    /// Local development configuration. One admin token is bound to the local
    /// namespace and is intended only for loopback access.
    pub fn local(data_dir: impl Into<PathBuf>) -> Self {
        let namespace = std::env::var("GOLDENBEE_NAMESPACE").unwrap_or_else(|_| "local".into());
        let token = std::env::var("GOLDENBEE_TOKEN").unwrap_or_else(|_| "local-dev-token".into());
        let mut tokens = BTreeMap::new();
        tokens.insert(token, Principal::new("local", namespace.clone(), &["*"]));
        let mut providers = BTreeMap::new();
        providers.insert(
            "deterministic".into(),
            ProviderProfile {
                adapter: "deterministic".into(),
                model: "deterministic".into(),
                endpoint: None,
                modalities: vec!["text".into()],
                tool_calling: true,
                local: true,
                residency: "local".into(),
                context_window: Some(131_072),
                credential: None,
                cost_rank: Some(0),
                input_cost_micros_per_million: None,
                candidate_cost_micros_per_million: None,
                data_class: Some("local".into()),
                fallbacks: Vec::new(),
            },
        );
        providers.insert(
            "echo".into(),
            ProviderProfile {
                adapter: "deterministic".into(),
                model: "echo".into(),
                endpoint: None,
                modalities: vec!["text".into()],
                tool_calling: true,
                local: true,
                residency: "local".into(),
                context_window: Some(131_072),
                credential: None,
                cost_rank: Some(0),
                input_cost_micros_per_million: None,
                candidate_cost_micros_per_million: None,
                data_class: Some("local".into()),
                fallbacks: Vec::new(),
            },
        );
        let capabilities = vec![
            CapabilityDescriptor {
                id: "general".into(),
                kind: "agent".into(),
                description: "General purpose native ADK agent".into(),
                permissions: vec![scopes::TASK_SUBMIT.into()],
            },
            CapabilityDescriptor {
                id: "echo".into(),
                kind: "agent".into(),
                description: "Deterministic echo agent".into(),
                permissions: vec![scopes::TASK_SUBMIT.into()],
            },
        ];
        Self {
            instance_id: std::env::var("GOLDENBEE_INSTANCE_ID")
                .unwrap_or_else(|_| format!("worker-{}", short_id())),
            package: String::new(),
            package_version: String::new(),
            incarnation: format!("inc-{}", uuid::Uuid::new_v4()),
            config_revision: 1,
            data_dir: data_dir.into(),
            tokens,
            capabilities,
            routing_agents: std::collections::BTreeSet::new(),
            instance_grants: Arc::new(RwLock::new(vec!["*".into()])),
            skills: Vec::new(),
            max_active: 4,
            max_queue: 32,
            per_principal: 8,
            max_subagents: 8,
            event_retention: 2_000,
            drain_timeout_ms: 10_000,
            max_streams: DEFAULT_MAX_STREAMS,
            stream_buffer: DEFAULT_STREAM_BUFFER,
            stream_keepalive_ms: DEFAULT_STREAM_KEEPALIVE_MS,
            lease_ttl_ms: 30_000,
            trusted_signers: BTreeMap::new(),
            default_model: "deterministic".into(),
            providers,
            agent_requirements: BTreeMap::new(),
            agent_providers: BTreeMap::new(),
            mcp_policies: Arc::new(RwLock::new(BTreeMap::new())),
            permissions: crate::policy::default_handle(),
            mcp: crate::mcp::McpConfig::default(),
            peers: BTreeMap::new(),
            delegation_secret: std::env::var("GOLDENBEE_DELEGATION_SECRET")
                .map(|secret| secret.into_bytes())
                .unwrap_or_else(|_| b"local-development-delegation-secret".to_vec()),
            allow_remote: false,
            dev_ui: std::env::var("GOLDENBEE_DEV_UI").is_ok_and(|value| value == "1"),
            tls_configured: false,
            retention: RetentionPolicy::default(),
        }
    }

    /// Add a token binding.
    pub fn with_token(mut self, token: impl Into<String>, principal: Principal) -> Self {
        self.tokens.insert(token.into(), principal);
        self
    }

    /// Set the instance-scoped admission ceilings.
    ///
    /// `max_active` is the number of concurrent native executions. `max_queue`
    /// is the finite number of admitted tasks waiting for a slot. `per_principal`
    /// bounds the non-terminal work owned by one authenticated caller. Zero
    /// active and per-principal values are raised to one; an explicit zero queue
    /// still admits work that can start immediately.
    pub fn with_limits(
        mut self,
        max_active: usize,
        max_queue: usize,
        per_principal: usize,
    ) -> Self {
        self.max_active = max_active.max(1);
        self.max_queue = max_queue;
        self.per_principal = per_principal.max(1);
        self
    }

    /// Restrict the process to loopback unless remote deployment is approved.
    pub fn remote_approved(mut self) -> Self {
        self.allow_remote = true;
        self
    }
}

fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}
