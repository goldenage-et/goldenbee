//! Gateway-managed model-provider metadata and encrypted envelopes.
//!
//! These are wire shapes only. A provider credential value is accepted in one
//! request and never represented in a response, debug output, task, event, or
//! audit record. Envelopes contain ciphertext addressed to one Worker.

use serde::{Deserialize, Serialize};
use std::fmt;

/// HKDF context string used for a per-Worker provider envelope.
pub const PROVIDER_ENVELOPE_INFO: &str = "goldenbee-provider-key-v1";
/// Maximum lifetime of a per-Worker provider envelope.
pub const PROVIDER_ENVELOPE_TTL_MS: i64 = 300_000;
/// Maximum lifetime of a one-time Worker pairing code.
pub const PAIR_CODE_TTL_MS: i64 = 300_000;
/// Native adapter kinds a Gateway provider may select.
pub const PROVIDER_ADAPTERS: &[&str] = &[
    "deterministic",
    "http-json",
    "ollama",
    "openai-compatible",
    "openrouter",
];

/// Whether this adapter discovers its usable models from provider metadata
/// instead of an operator-declared single model.
pub fn adapter_discovers_models(adapter: &str) -> bool {
    matches!(adapter, "openrouter" | "openai-compatible" | "ollama")
}

/// Maximum discovered model identifiers retained per provider.
pub const MAX_DISCOVERED_MODELS: usize = 2048;

/// Non-secret provider catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderMetadata {
    /// Stable logical provider id selected by Worker configuration.
    pub id: String,
    /// Native adapter kind.
    pub adapter: String,
    /// Preferred concrete model identifier. Empty while discovery has not yet
    /// selected a usable default.
    #[serde(default)]
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
    /// Declared context window, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Whether the provider is a local deployment.
    #[serde(default)]
    pub local: bool,
    /// Declared residency.
    #[serde(default)]
    pub residency: String,
    /// Worker ids this provider is assigned to. `*` assigns every approved
    /// Worker; an empty list assigns none.
    #[serde(default)]
    pub allowed_workers: Vec<String>,
    /// Discovered or explicitly declared model identifiers. Bounded and
    /// deduplicated; never secret.
    #[serde(default)]
    pub models: Vec<String>,
    /// `explicit`, `discovered`, or `pending`.
    #[serde(default)]
    pub models_source: String,
    /// Last successful or failed discovery time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered_at: Option<i64>,
    /// Optional operator filter applied to discovered models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// Whether discovery may select the first model when no default is set.
    #[serde(default)]
    pub allow_auto_default: bool,
    /// Whether the provider has a usable default model and fresh credential.
    #[serde(default)]
    pub ready: bool,
    /// Redacted actionable discovery failure message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_error: Option<String>,
    /// Stable discovery failure code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_error_code: Option<String>,
    /// Optimistic concurrency revision.
    #[serde(default)]
    pub revision: u64,
    /// Whether new envelope fetches include this provider.
    #[serde(default)]
    pub active: bool,
    /// Active credential version. Only the version is exposed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_version: Option<u64>,
    /// SHA-256 fingerprint of the active credential. Never the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_fingerprint: Option<String>,
    /// Whether the active credential was revoked.
    #[serde(default)]
    pub credential_revoked: bool,
    /// Creation time.
    #[serde(default)]
    pub created_at: i64,
    /// Last metadata or credential change.
    #[serde(default)]
    pub updated_at: i64,
}

/// Admin request to create a provider catalog entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProviderRequest {
    /// Stable logical provider id.
    pub id: String,
    /// Native adapter kind.
    pub adapter: String,
    /// Preferred concrete model identifier. Omit to select a discovered model
    /// when `allow_auto_default` is explicitly true.
    #[serde(default)]
    pub model: String,
    /// Endpoint base URL for endpoint-backed adapters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Optional allow-list intersected with discovered models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// Whether discovery may choose the first model when no preferred model is
    /// set. Defaults to false so a model must be selected explicitly.
    #[serde(default)]
    pub allow_auto_default: bool,
    /// Supported modalities. Defaults to text.
    #[serde(default = "default_modalities")]
    pub modalities: Vec<String>,
    /// Whether function calling is supported.
    #[serde(default)]
    pub tool_calling: bool,
    /// Declared context window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Whether the provider is local.
    #[serde(default)]
    pub local: bool,
    /// Declared residency.
    pub residency: String,
    /// Worker assignments. `*` assigns every approved Worker.
    #[serde(default)]
    pub allowed_workers: Vec<String>,
}

fn default_modalities() -> Vec<String> {
    vec!["text".into()]
}

/// Admin request to update non-secret provider metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateProviderRequest {
    /// Revision the caller observed.
    pub revision: u64,
    /// New adapter kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    /// New preferred concrete model identifier. An empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// New endpoint. An empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// New discovered-model allow-list. An empty list allows no model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// New auto-default policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_auto_default: Option<bool>,
    /// New modalities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<Vec<String>>,
    /// New function-calling declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calling: Option<bool>,
    /// New context window. Zero clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// New locality declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<bool>,
    /// New residency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<String>,
    /// New Worker assignments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_workers: Option<Vec<String>>,
    /// Enable or disable new envelope fetches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

/// Write-only provider credential body. Never returned, logged, or formatted.
#[derive(Clone, Deserialize)]
pub struct SetProviderCredentialRequest {
    /// Provider API key. The only accepted transport is this request body.
    key: String,
}

impl SetProviderCredentialRequest {
    /// Construct a write-only request. Used by tests and owned callers.
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }

    /// Borrow the credential for encryption. Callers must not log it.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl fmt::Debug for SetProviderCredentialRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SetProviderCredentialRequest")
            .field("key", &"[REDACTED]")
            .finish()
    }
}

/// Non-secret result of writing or revoking a credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCredentialSummary {
    /// Provider id.
    pub provider: String,
    /// Credential version written or revoked.
    pub version: u64,
    /// SHA-256 fingerprint. Never the key.
    pub fingerprint: String,
    /// Write time.
    pub created_at: i64,
    /// Revocation time, when revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
    /// Provider revision after the write and any discovery attempt.
    #[serde(default)]
    pub revision: u64,
    /// Whether the provider is usable after discovery.
    #[serde(default)]
    pub ready: bool,
    /// Discovered models after filtering.
    #[serde(default)]
    pub models: Vec<String>,
    /// Last discovery time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered_at: Option<i64>,
    /// Redacted discovery failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_error: Option<String>,
    /// Stable discovery failure code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_error_code: Option<String>,
}

/// Admin request to mint a one-time Worker pairing code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePairCodeRequest {
    /// Approved Worker id.
    pub worker_id: String,
    /// Worker instance identity bound to the code.
    pub instance: String,
}

/// One-time pairing code response. The code alone is the enrollment authority;
/// no provider key is ever part of it or its link.
#[derive(Clone, Serialize, Deserialize)]
pub struct PairCodeResponse {
    /// High-entropy code, shown once.
    pub code: String,
    /// Pairing link containing only the code and binding identities.
    pub link: String,
    /// Approved Worker id.
    pub worker_id: String,
    /// Bound instance identity.
    pub instance: String,
    /// Expiry time.
    pub expires_at: i64,
}

impl fmt::Debug for PairCodeResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairCodeResponse")
            .field("code", &"[REDACTED]")
            .field("worker_id", &self.worker_id)
            .field("instance", &self.instance)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Worker enrollment body. Authenticated by the Worker control token and a
/// one-time pairing code.
#[derive(Clone, Serialize, Deserialize)]
pub struct WorkerEnrollRequest {
    /// One-time pairing code.
    pub code: String,
    /// Approved Worker id bound by the code.
    pub worker_id: String,
    /// Worker instance identity bound by the code.
    pub instance: String,
    /// Base64 X25519 public key.
    pub public_key: String,
}

impl fmt::Debug for WorkerEnrollRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerEnrollRequest")
            .field("code", &"[REDACTED]")
            .field("worker_id", &self.worker_id)
            .field("instance", &self.instance)
            .field("public_key", &self.public_key)
            .finish()
    }
}

/// Successful Worker enrollment. No key material is returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerEnrollResponse {
    /// Enrolled Worker id.
    pub worker_id: String,
    /// Bound instance identity.
    pub instance: String,
    /// Enrollment time.
    pub enrolled_at: i64,
}

/// Ciphertext envelope addressed to one Worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEnvelope {
    /// Provider id.
    pub provider: String,
    /// Credential version.
    pub version: u64,
    /// Base64 ephemeral X25519 public key.
    pub ephemeral_public_key: String,
    /// Base64 24-byte nonce.
    pub nonce: String,
    /// Base64 ciphertext.
    pub ciphertext: String,
    /// Envelope expiry. Never later than five minutes after issue.
    pub expires_at: i64,
}

/// One assigned provider with its non-secret metadata and envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAssignment {
    /// Non-secret provider metadata.
    pub metadata: ProviderMetadata,
    /// Credential ciphertext for this Worker.
    pub envelope: ProviderEnvelope,
}

/// Provider bundle returned to one enrolled Worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderBundle {
    /// Worker id.
    pub worker: String,
    /// Bound instance identity.
    pub instance: String,
    /// Issue time.
    pub issued_at: i64,
    /// Bundle expiry, equal to the earliest envelope expiry.
    pub expires_at: i64,
    /// Assigned providers only.
    pub providers: Vec<ProviderAssignment>,
}
