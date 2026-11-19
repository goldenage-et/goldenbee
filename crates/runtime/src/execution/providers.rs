//! Logical provider profiles, preflight, native adapter construction, and
//! policy-constrained fallback.
//!
//! A profile revision names a logical provider. This module turns that name
//! into exactly one concrete provider record and one native `adk_core::Llm`
//! implementation. Construction reads credential *references* through a
//! [`SecretSource`]; values are held only inside the adapter and are never
//! copied onto a task, event, inspection view, or error.
//!
//! Fallback is an explicit policy intersection, not a boolean. Only providers
//! named by the primary profile are candidates, and a candidate may not widen
//! locality, residency, credential authority, cost, or data handling.

use crate::execution::model::{DeterministicModel, HttpModel};
use crate::{ApiError, ProviderProfile};
use adk_rust::{AdkError, ErrorCategory, ErrorComponent, Llm, LlmRequest, LlmResponseStream};
use async_trait::async_trait;
use common::{ContextCompaction, ResolvedModel};
use futures::StreamExt;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

/// Supported native adapter kinds.
pub const ADAPTERS: &[&str] = &[
    "deterministic",
    "http-json",
    "ollama",
    "openai-compatible",
    "openrouter",
];

/// A secret *reference*, resolved at model construction.
pub trait SecretSource: Send + Sync {
    /// Whether the reference currently resolves to a value.
    fn available(&self, reference: &str) -> bool;
    /// Resolve the value. Implementations must not log or persist it.
    fn value(&self, reference: &str) -> std::result::Result<String, String>;
}

/// Environment-backed secret source.
///
/// `env:NAME` resolves from the process environment. `secret://` and `vault:`
/// are recognized references whose providers are not installed in this slice,
/// so they fail with an explicit unavailable error instead of a silent empty
/// value.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnvironmentSecrets;

impl SecretSource for EnvironmentSecrets {
    fn available(&self, reference: &str) -> bool {
        match parse_reference(reference) {
            Some(("env", name)) => std::env::var(name).is_ok_and(|value| !value.is_empty()),
            _ => false,
        }
    }

    fn value(&self, reference: &str) -> std::result::Result<String, String> {
        match parse_reference(reference) {
            Some(("env", name)) => std::env::var(name)
                .ok()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("credential reference '{reference}' is not available")),
            Some((scheme, _)) => Err(format!(
                "credential provider '{scheme}:' is not installed; reference '{reference}' cannot be resolved"
            )),
            None => Err(format!(
                "credential reference '{reference}' is not a supported reference"
            )),
        }
    }
}

/// In-memory secret source for Gateway-decrypted provider credentials.
///
/// Entries are keyed by `gateway:<provider>` references. Values are zeroized on
/// drop and never appear in `Debug`, task projections, or errors.
pub struct MemorySecrets {
    values: BTreeMap<String, String>,
}

impl MemorySecrets {
    /// Build a secret source from already-decrypted values.
    pub fn new(values: BTreeMap<String, String>) -> Self {
        Self { values }
    }

    /// Number of resident references.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether no reference is available.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl SecretSource for MemorySecrets {
    fn available(&self, reference: &str) -> bool {
        self.values
            .get(reference)
            .is_some_and(|value| !value.is_empty())
    }

    fn value(&self, reference: &str) -> std::result::Result<String, String> {
        self.values
            .get(reference)
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or_else(|| format!("credential reference '{reference}' is not available"))
    }
}

impl std::fmt::Debug for MemorySecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemorySecrets")
            .field("references", &self.values.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Drop for MemorySecrets {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        for value in self.values.values_mut() {
            value.zeroize();
        }
    }
}

fn parse_reference(reference: &str) -> Option<(&str, &str)> {
    let (scheme, name) = reference.split_once(':')?;
    if name.is_empty() {
        return None;
    }
    Some((scheme, name))
}

/// Credential reference rules shared with profile validation.
pub fn validate_credential_reference(reference: &str) -> bool {
    let looks_like_value = reference.starts_with("sk-")
        || reference.starts_with("Bearer ")
        || reference.starts_with("AKIA")
        || reference.len() > 256;
    let is_reference = reference.starts_with("secret://")
        || reference.starts_with("env:")
        || reference.starts_with("vault:")
        || reference.starts_with("gateway:");
    !looks_like_value && is_reference
}

/// Invocation requirements checked before admission and before construction.
#[derive(Debug, Clone, Default)]
pub struct ProviderRequirements {
    /// Modalities the invocation needs, such as `text`.
    pub modalities: Vec<String>,
    /// Number of bound tools or toolsets.
    pub tools: usize,
    /// Maximum declared token budget, zero when unset.
    pub max_tokens: u64,
}

impl ProviderRequirements {
    /// The harness requires text for every agent invocation.
    pub fn text() -> Self {
        Self {
            modalities: vec!["text".into()],
            tools: 0,
            max_tokens: 0,
        }
    }
}

/// Observable notification that a policy-allowed fallback was used.
pub type FallbackReporter = Arc<dyn Fn(ResolvedModel) + Send + Sync>;

/// One concrete provider decision plus its native adapter.
pub struct ResolvedProvider {
    /// Concrete provider identity recorded for the invocation.
    pub record: ResolvedModel,
    /// Native model implementation.
    pub model: Arc<dyn Llm>,
}

/// Secret-safe provider inspection.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderView {
    /// Logical provider id.
    pub id: String,
    /// Native adapter kind.
    pub adapter: String,
    /// Concrete model identifier.
    pub model: String,
    /// Sanitized endpoint identity.
    pub endpoint: Option<String>,
    /// Supported modalities.
    pub modalities: Vec<String>,
    /// Whether function calling is supported.
    pub tool_calling: bool,
    /// Whether the deployment is local.
    pub local: bool,
    /// Declared residency.
    pub residency: String,
    /// Declared context window.
    pub context_window: Option<u64>,
    /// Credential reference, never a value.
    pub credential: Option<String>,
    /// Whether the credential reference is currently revoked.
    pub credential_revoked: bool,
    /// Explicitly allowed fallbacks.
    pub fallbacks: Vec<String>,
}

/// Provider registry shared by admission preflight and native construction.
pub struct ProviderRegistry {
    profiles: BTreeMap<String, ProviderProfile>,
    allow_remote: bool,
    secrets: Arc<dyn SecretSource>,
    revoked: Arc<RwLock<BTreeSet<String>>>,
}

impl ProviderRegistry {
    /// Build a registry from configured profiles with environment secrets.
    pub fn from_profiles(
        profiles: BTreeMap<String, ProviderProfile>,
        allow_remote: bool,
    ) -> std::result::Result<Arc<Self>, ApiError> {
        Self::new(profiles, allow_remote, Arc::new(EnvironmentSecrets))
    }

    /// Build a registry with an injected secret source.
    pub fn new(
        profiles: BTreeMap<String, ProviderProfile>,
        allow_remote: bool,
        secrets: Arc<dyn SecretSource>,
    ) -> std::result::Result<Arc<Self>, ApiError> {
        for (id, profile) in &profiles {
            validate_profile(id, &profiles, profile)?;
        }
        Ok(Arc::new(Self {
            profiles,
            allow_remote,
            secrets,
            revoked: Arc::new(RwLock::new(BTreeSet::new())),
        }))
    }

    /// Configured profiles.
    pub fn profiles(&self) -> &BTreeMap<String, ProviderProfile> {
        &self.profiles
    }

    /// Whether remote deployments were explicitly approved.
    pub fn remote_approved(&self) -> bool {
        self.allow_remote
    }

    /// Revoke one credential reference. New provider constructions and new
    /// model calls through already-built adapters fail until it is restored.
    pub fn revoke(&self, reference: &str) {
        self.revoked
            .write()
            .expect("credential revocation lock is not poisoned")
            .insert(reference.to_string());
    }

    /// Restore one revoked credential reference.
    pub fn restore(&self, reference: &str) {
        self.revoked
            .write()
            .expect("credential revocation lock is not poisoned")
            .remove(reference);
    }

    /// Whether a credential reference is revoked.
    pub fn revoked(&self, reference: &str) -> bool {
        self.revoked
            .read()
            .expect("credential revocation lock is not poisoned")
            .contains(reference)
    }

    /// Inspect one provider without exposing secrets.
    pub fn view(&self, id: &str) -> std::result::Result<ProviderView, ApiError> {
        let profile = self.profiles.get(id).ok_or_else(|| {
            ApiError::new(
                "provider.unknown",
                format!("provider '{id}' is not configured"),
            )
        })?;
        Ok(ProviderView {
            id: id.to_string(),
            adapter: profile.adapter.clone(),
            model: profile.model.clone(),
            endpoint: profile
                .endpoint
                .as_deref()
                .and_then(|value| reqwest::Url::parse(value).ok())
                .map(|url| endpoint_identity(&url)),
            modalities: profile.modalities.clone(),
            tool_calling: profile.tool_calling,
            local: profile.local,
            residency: profile.residency.clone(),
            context_window: profile.context_window,
            credential: profile.credential.clone(),
            credential_revoked: profile
                .credential
                .as_deref()
                .is_some_and(|reference| self.revoked(reference)),
            fallbacks: profile.fallbacks.clone(),
        })
    }

    /// Inspect every provider in stable order.
    pub fn views(&self) -> Vec<ProviderView> {
        self.profiles
            .keys()
            .filter_map(|id| self.view(id).ok())
            .collect()
    }

    /// Resolve and preflight the concrete record without reading a secret value.
    pub fn record(
        &self,
        id: &str,
        profile_revision: u64,
        requirements: &ProviderRequirements,
    ) -> std::result::Result<ResolvedModel, ApiError> {
        let profile = self.preflight(id, requirements)?;
        let endpoint = self.endpoint(profile)?;
        Ok(ResolvedModel {
            provider: id.to_string(),
            adapter: profile.adapter.clone(),
            model: profile.model.clone(),
            local: profile.local,
            residency: profile.residency.clone(),
            endpoint: endpoint.as_ref().map(endpoint_identity),
            credential: profile.credential.clone(),
            input_cost_micros_per_million: profile.input_cost_micros_per_million,
            candidate_cost_micros_per_million: profile.candidate_cost_micros_per_million,
            profile_revision,
        })
    }

    /// Resolve one provider into a native adapter.
    pub fn resolve(
        &self,
        id: &str,
        profile_revision: u64,
        requirements: &ProviderRequirements,
    ) -> std::result::Result<ResolvedProvider, ApiError> {
        self.resolve_inner(id, profile_revision, requirements, true, None)
    }

    /// Resolve only the primary provider.
    ///
    /// Agent-level references use this path so the invocation record always
    /// names the provider that actually ran. Policy fallback is applied to
    /// profile-composition invocations, where the executor can report the
    /// selected fallback and update the task projection.
    pub fn resolve_primary(
        &self,
        id: &str,
        profile_revision: u64,
        requirements: &ProviderRequirements,
    ) -> std::result::Result<ResolvedProvider, ApiError> {
        self.resolve_inner(id, profile_revision, requirements, false, None)
    }

    /// Resolve one provider, notifying the caller when an allowed fallback is
    /// actually used before any output was produced.
    pub fn resolve_with_observer(
        &self,
        id: &str,
        profile_revision: u64,
        requirements: &ProviderRequirements,
        observer: Option<FallbackReporter>,
    ) -> std::result::Result<ResolvedProvider, ApiError> {
        self.resolve_inner(id, profile_revision, requirements, true, observer)
    }

    fn resolve_inner(
        &self,
        id: &str,
        profile_revision: u64,
        requirements: &ProviderRequirements,
        include_fallbacks: bool,
        observer: Option<FallbackReporter>,
    ) -> std::result::Result<ResolvedProvider, ApiError> {
        let record = self.record(id, profile_revision, requirements)?;
        let primary = self.build(id)?;
        let mut fallbacks = Vec::new();
        if include_fallbacks {
            for fallback_id in &self.profiles[id].fallbacks {
                let fallback_profile = self.profiles.get(fallback_id).ok_or_else(|| {
                    ApiError::new(
                        "provider.fallback_unknown",
                        format!("fallback provider '{fallback_id}' is not configured"),
                    )
                })?;
                fallback_policy(&self.profiles[id], fallback_profile)?;
                let fallback_record = self.record(fallback_id, profile_revision, requirements)?;
                let model = self.build(fallback_id)?;
                fallbacks.push(Slot {
                    record: fallback_record,
                    model,
                    credential: fallback_profile.credential.clone(),
                    secrets: self.credential_secrets(fallback_profile),
                });
            }
        }
        let model: Arc<dyn Llm> = Arc::new(ProviderModel {
            name: record.model.clone(),
            primary: Slot {
                record: record.clone(),
                model: primary,
                credential: self.profiles[id].credential.clone(),
                secrets: self.credential_secrets(&self.profiles[id]),
            },
            fallbacks,
            revoked: self.revoked.clone(),
            observer,
        });
        Ok(ResolvedProvider { record, model })
    }

    /// Admission preflight for one logical provider and its allowed fallbacks.
    pub fn preflight(
        &self,
        id: &str,
        requirements: &ProviderRequirements,
    ) -> std::result::Result<&ProviderProfile, ApiError> {
        let profile = self.check(id, requirements)?;
        for fallback in &profile.fallbacks {
            let target = self.profiles.get(fallback).ok_or_else(|| {
                ApiError::new(
                    "provider.fallback_unknown",
                    format!("provider '{id}' names unknown fallback '{fallback}'"),
                )
            })?;
            fallback_policy(profile, target)?;
            self.check(fallback, requirements)?;
        }
        Ok(profile)
    }

    /// Verify that a profile's native context compaction can stay below every
    /// provider that may serve the invocation.
    ///
    /// The pinned Runner estimates tokens before the request and compacts with
    /// the native truncation strategy. A provider without a declared window, or
    /// with a window at or below the configured budget, makes that guarantee
    /// false; this rejects the profile before any unsafe continuation.
    pub fn validate_compaction(
        &self,
        id: &str,
        compaction: &ContextCompaction,
    ) -> std::result::Result<(), ApiError> {
        if compaction.strategy != "truncation" {
            return Err(ApiError::new(
                "profile.compaction_strategy_unsupported",
                format!(
                    "context compaction strategy '{}' is not supported; only native 'truncation' is available",
                    compaction.strategy
                ),
            ));
        }
        if compaction.context_budget == 0 {
            return Err(ApiError::new(
                "profile.compaction_budget_invalid",
                "context compaction requires a positive token budget",
            ));
        }
        if compaction.context_budget > 10_000_000 {
            return Err(ApiError::new(
                "profile.compaction_budget_invalid",
                "context compaction budget exceeds the supported ceiling",
            ));
        }
        if compaction.preserve_recent == 0 || compaction.preserve_recent > 10_000 {
            return Err(ApiError::new(
                "profile.compaction_preserve_invalid",
                "context compaction preserve_recent must be between 1 and 10000",
            ));
        }
        if compaction.max_retries == 0 || compaction.max_retries > 8 {
            return Err(ApiError::new(
                "profile.compaction_retries_invalid",
                "context compaction max_retries must be between 1 and 8",
            ));
        }
        let profile = self.profiles.get(id).ok_or_else(|| {
            ApiError::new(
                "provider.unknown",
                format!("provider '{id}' is not configured"),
            )
        })?;
        self.check_compaction_window(profile, id, compaction.context_budget)?;
        // A fallback may serve the same run when the primary fails before
        // output. It must not silently reintroduce an over-limit request.
        for fallback in &profile.fallbacks {
            let target = self.profiles.get(fallback).ok_or_else(|| {
                ApiError::new(
                    "provider.fallback_unknown",
                    format!("provider '{id}' names unknown fallback '{fallback}'"),
                )
            })?;
            self.check_compaction_window(target, fallback, compaction.context_budget)?;
        }
        Ok(())
    }

    fn check_compaction_window(
        &self,
        profile: &ProviderProfile,
        id: &str,
        budget: u64,
    ) -> std::result::Result<(), ApiError> {
        match profile.context_window {
            Some(window) if window > budget => Ok(()),
            Some(window) => Err(ApiError::new(
                "provider.compaction_context_unsupported",
                format!(
                    "provider '{id}' declares a {window}-token context window that does not leave room above the {budget}-token compaction budget"
                ),
            )
            .with("provider", id)
            .with("context_window", window.to_string())
            .with("context_budget", budget.to_string())),
            None => Err(ApiError::new(
                "provider.compaction_context_unknown",
                format!(
                    "provider '{id}' does not declare the context window required by context compaction"
                ),
            )
            .with("provider", id)
            .with("context_budget", budget.to_string())),
        }
    }

    fn check(
        &self,
        id: &str,
        requirements: &ProviderRequirements,
    ) -> std::result::Result<&ProviderProfile, ApiError> {
        let profile = self.profiles.get(id).ok_or_else(|| {
            ApiError::new(
                "provider.unknown",
                format!("provider '{id}' is not configured"),
            )
        })?;
        for modality in &requirements.modalities {
            if !profile.modalities.iter().any(|value| value == modality) {
                return Err(ApiError::new(
                    "provider.modality_unsupported",
                    format!("provider '{id}' does not support required modality '{modality}'"),
                ));
            }
        }
        if requirements.tools > 0 && !profile.tool_calling {
            return Err(ApiError::new(
                "provider.tool_schema_unsupported",
                format!("provider '{id}' does not support the bound tool schema"),
            ));
        }
        if profile.residency.trim().is_empty() {
            return Err(ApiError::new(
                "provider.residency_unknown",
                format!("provider '{id}' does not declare a residency"),
            ));
        }
        if requirements.max_tokens > 0 {
            match profile.context_window {
                Some(window) if window >= requirements.max_tokens => {}
                Some(window) => {
                    return Err(ApiError::new(
                        "provider.context_unsupported",
                        format!(
                            "provider '{id}' declares a {window}-token context window below the requested {} tokens",
                            requirements.max_tokens
                        ),
                    ))
                }
                None => {
                    return Err(ApiError::new(
                        "provider.context_unknown",
                        format!(
                            "provider '{id}' does not declare a context window for the requested {} tokens",
                            requirements.max_tokens
                        ),
                    ))
                }
            }
        }
        if let Some(reference) = &profile.credential {
            if !validate_credential_reference(reference) {
                return Err(ApiError::new(
                    "provider.credential_invalid",
                    "provider credential must be a secret://, env:, vault:, or gateway: reference",
                ));
            }
            if self.revoked(reference) {
                return Err(ApiError::new(
                    "provider.credential_revoked",
                    format!("credential reference '{reference}' is revoked"),
                ));
            }
            if !self.secrets.available(reference) {
                return Err(ApiError::new(
                    "provider.credential_unavailable",
                    format!("credential reference '{reference}' is not available"),
                ));
            }
        } else if profile.requires_credential() {
            return Err(ApiError::new(
                "provider.credential_missing",
                format!("provider '{id}' requires a credential reference"),
            ));
        }
        let endpoint = self.endpoint(profile)?;
        if profile.local {
            match &endpoint {
                Some(url) if !is_loopback(url) => {
                    return Err(ApiError::new(
                        "provider.locality_mismatch",
                        format!(
                            "provider '{id}' is declared local but its endpoint is not loopback"
                        ),
                    ))
                }
                Some(_) => {}
                None if profile.endpoint_backed() => {
                    return Err(ApiError::new(
                        "provider.endpoint_missing",
                        format!("provider '{id}' requires an endpoint"),
                    ))
                }
                None => {}
            }
        } else if profile.endpoint_backed() && !self.allow_remote {
            return Err(ApiError::new(
                "provider.remote_not_approved",
                format!(
                    "provider '{id}' is remote but this instance does not approve remote providers"
                ),
            ));
        }
        Ok(profile)
    }

    fn endpoint(
        &self,
        profile: &ProviderProfile,
    ) -> std::result::Result<Option<reqwest::Url>, ApiError> {
        let raw = match (&profile.endpoint, profile.adapter.as_str()) {
            (Some(value), _) => value.clone(),
            (None, "ollama") => "http://127.0.0.1:11434".into(),
            (None, "openrouter") => "https://openrouter.ai/api/v1".into(),
            (None, _) => return Ok(None),
        };
        let url = reqwest::Url::parse(&raw).map_err(|_| {
            ApiError::new(
                "provider.endpoint_invalid",
                "provider endpoint is not a valid URL",
            )
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ApiError::new(
                "provider.endpoint_invalid",
                "provider endpoint must use http or https",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(ApiError::new(
                "provider.endpoint_invalid",
                "provider endpoint must not embed credentials; use a credential reference",
            ));
        }
        Ok(Some(url))
    }

    fn credential_secrets(&self, profile: &ProviderProfile) -> Vec<String> {
        profile
            .credential
            .as_deref()
            .filter(|reference| validate_credential_reference(reference))
            .and_then(|reference| self.secrets.value(reference).ok())
            .into_iter()
            .collect()
    }

    fn build(&self, id: &str) -> std::result::Result<Arc<dyn Llm>, ApiError> {
        let profile = self.profiles.get(id).ok_or_else(|| {
            ApiError::new(
                "provider.unknown",
                format!("provider '{id}' is not configured"),
            )
        })?;
        let endpoint = self.endpoint(profile)?;
        match profile.adapter.as_str() {
            "deterministic" => Ok(Arc::new(DeterministicModel::named(
                profile.model.clone(),
                "echo",
            ))),
            "http-json" => {
                let endpoint = endpoint.expect("preflight requires an endpoint");
                Ok(Arc::new(HttpModel::new(
                    endpoint.to_string(),
                    profile.model.clone(),
                )))
            }
            "ollama" => {
                use adk_rust::model::ollama::{OllamaConfig, OllamaModel};
                let endpoint = endpoint.expect("ollama has a default endpoint");
                let mut config =
                    OllamaConfig::with_host(endpoint.to_string(), profile.model.clone());
                config.num_ctx = profile
                    .context_window
                    .and_then(|value| u32::try_from(value).ok());
                let model = OllamaModel::new(config).map_err(|error| {
                    ApiError::new(
                        "provider.construction_failed",
                        common::redact(&format!(
                            "ollama adapter could not be constructed: {error}"
                        )),
                    )
                })?;
                Ok(Arc::new(model))
            }
            "openai-compatible" => {
                use adk_rust::model::openai_compatible::{
                    OpenAICompatible, OpenAICompatibleConfig,
                };
                let endpoint = endpoint.expect("preflight requires an endpoint");
                let key = self.credential_value(profile)?;
                let mut config = OpenAICompatibleConfig::new(key, profile.model.clone())
                    .with_base_url(endpoint.to_string());
                config.parallel_tool_calls = profile.tool_calling;
                let model = OpenAICompatible::new(config).map_err(|error| {
                    ApiError::new(
                        "provider.construction_failed",
                        common::redact(&format!(
                            "openai-compatible adapter could not be constructed: {error}"
                        )),
                    )
                })?;
                Ok(Arc::new(model))
            }
            "openrouter" => {
                use adk_rust::model::openrouter::{OpenRouterClient, OpenRouterConfig};
                let key = self.credential_value(profile)?;
                let mut config = OpenRouterConfig::new(key, profile.model.clone())
                    .with_title("Goldenbee Worker")
                    .with_http_referer("https://github.com/goldenbee");
                if let Some(endpoint) = endpoint {
                    config = config.with_base_url(endpoint.to_string());
                }
                let model = OpenRouterClient::new(config).map_err(|error| {
                    ApiError::new(
                        "provider.construction_failed",
                        common::redact(&format!(
                            "openrouter adapter could not be constructed: {error}"
                        )),
                    )
                })?;
                Ok(Arc::new(model))
            }
            other => Err(ApiError::new(
                "provider.adapter_unsupported",
                format!("provider adapter '{other}' is not supported"),
            )),
        }
    }

    fn credential_value(&self, profile: &ProviderProfile) -> std::result::Result<String, ApiError> {
        let reference = profile.credential.as_deref().ok_or_else(|| {
            ApiError::new(
                "provider.credential_missing",
                "provider requires a credential reference",
            )
        })?;
        if self.revoked(reference) {
            return Err(ApiError::new(
                "provider.credential_revoked",
                format!("credential reference '{reference}' is revoked"),
            ));
        }
        self.secrets.value(reference).map_err(|message| {
            ApiError::new("provider.credential_unavailable", common::redact(&message))
        })
    }
}

fn validate_profile(
    id: &str,
    profiles: &BTreeMap<String, ProviderProfile>,
    profile: &ProviderProfile,
) -> std::result::Result<(), ApiError> {
    if !ADAPTERS.contains(&profile.adapter.as_str()) {
        return Err(ApiError::new(
            "provider.adapter_unsupported",
            format!(
                "provider '{id}' uses unsupported adapter '{}'",
                profile.adapter
            ),
        ));
    }
    if profile.model.trim().is_empty() {
        return Err(ApiError::new(
            "provider.model_missing",
            format!("provider '{id}' has no concrete model"),
        ));
    }
    if let Some(reference) = &profile.credential {
        if !validate_credential_reference(reference) {
            return Err(ApiError::new(
                "provider.credential_invalid",
                "provider credential must be a secret://, env:, vault:, or gateway: reference",
            ));
        }
    }
    for fallback in &profile.fallbacks {
        if fallback == id {
            return Err(ApiError::new(
                "provider.fallback_cycle",
                format!("provider '{id}' cannot fall back to itself"),
            ));
        }
        let target = profiles.get(fallback).ok_or_else(|| {
            ApiError::new(
                "provider.fallback_unknown",
                format!("provider '{id}' names unknown fallback '{fallback}'"),
            )
        })?;
        if !target.fallbacks.is_empty() {
            return Err(ApiError::new(
                "provider.fallback_chain",
                format!("fallback provider '{fallback}' must not declare further fallbacks"),
            ));
        }
    }
    Ok(())
}

/// Whether a fallback preserves every policy dimension of its primary.
pub fn fallback_policy(
    primary: &ProviderProfile,
    fallback: &ProviderProfile,
) -> std::result::Result<(), ApiError> {
    if primary.local && !fallback.local {
        return Err(ApiError::new(
            "provider.fallback_locality",
            "a local provider may only fall back to a local provider",
        ));
    }
    if primary.residency != fallback.residency {
        return Err(ApiError::new(
            "provider.fallback_residency",
            "a fallback may not change the declared residency",
        ));
    }
    if primary.credential != fallback.credential && fallback.requires_credential() {
        return Err(ApiError::new(
            "provider.fallback_authority",
            "a fallback may not require a credential outside the primary authority",
        ));
    }
    if primary.credential.is_none() && fallback.requires_credential() {
        return Err(ApiError::new(
            "provider.fallback_authority",
            "a fallback may not add credential authority",
        ));
    }
    if !cost_not_wider(primary.cost_rank, fallback.cost_rank) {
        return Err(ApiError::new(
            "provider.fallback_cost",
            "a fallback may not raise the declared cost rank",
        ));
    }
    if !data_not_wider(
        primary.data_class.as_deref(),
        fallback.data_class.as_deref(),
    ) {
        return Err(ApiError::new(
            "provider.fallback_data",
            "a fallback may not change the declared data-handling class",
        ));
    }
    Ok(())
}

fn cost_not_wider(primary: Option<u32>, fallback: Option<u32>) -> bool {
    match (primary, fallback) {
        (None, None) => true,
        (Some(primary), Some(fallback)) => fallback <= primary,
        _ => false,
    }
}

fn data_not_wider(primary: Option<&str>, fallback: Option<&str>) -> bool {
    match (primary, fallback) {
        (None, None) => true,
        (Some(primary), Some(fallback)) => primary == fallback,
        _ => false,
    }
}

fn is_loopback(url: &reqwest::Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}

fn endpoint_identity(url: &reqwest::Url) -> String {
    let mut clean = url.clone();
    let _ = clean.set_username("");
    let _ = clean.set_password(None);
    clean.set_query(None);
    clean.set_fragment(None);
    clean.to_string()
}

struct Slot {
    record: ResolvedModel,
    model: Arc<dyn Llm>,
    credential: Option<String>,
    secrets: Vec<String>,
}

struct ProviderModel {
    name: String,
    primary: Slot,
    fallbacks: Vec<Slot>,
    revoked: Arc<RwLock<BTreeSet<String>>>,
    observer: Option<FallbackReporter>,
}

impl ProviderModel {
    fn revoked(&self, slot: &Slot) -> bool {
        slot.credential.as_deref().is_some_and(|reference| {
            self.revoked
                .read()
                .expect("credential revocation lock is not poisoned")
                .contains(reference)
        })
    }
}

#[async_trait]
impl Llm for ProviderModel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream_response: bool,
    ) -> adk_rust::Result<LlmResponseStream> {
        if self.revoked(&self.primary) {
            return Err(revoked_error());
        }
        let secrets = self.primary.secrets.clone();
        match self
            .primary
            .model
            .generate_content(request.clone(), stream_response)
            .await
        {
            Ok(stream) => Ok(redact_stream(stream, secrets)),
            Err(error) if fallback_eligible(&error) && !self.fallbacks.is_empty() => {
                let mut last = redact_error(error, &secrets);
                for slot in &self.fallbacks {
                    if self.revoked(slot) {
                        continue;
                    }
                    match slot
                        .model
                        .generate_content(request.clone(), stream_response)
                        .await
                    {
                        Ok(stream) => {
                            if let Some(observer) = &self.observer {
                                observer(slot.record.clone());
                            }
                            return Ok(redact_stream(stream, slot.secrets.clone()));
                        }
                        Err(error) => {
                            last = redact_error(error, &slot.secrets);
                        }
                    }
                }
                Err(last)
            }
            Err(error) => Err(redact_error(error, &secrets)),
        }
    }
}

fn fallback_eligible(error: &AdkError) -> bool {
    matches!(
        error.category,
        ErrorCategory::RateLimited | ErrorCategory::Unavailable | ErrorCategory::Timeout
    )
}

fn revoked_error() -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unauthorized,
        "provider.credential_revoked",
        "provider credential is revoked",
    )
}

fn redact_error(error: AdkError, secrets: &[String]) -> AdkError {
    let mut message = common::redact(&error.message);
    for secret in secrets {
        if !secret.is_empty() {
            message = message.replace(secret, "[REDACTED]");
        }
    }
    AdkError::new(error.component, error.category, error.code, message)
        .with_retry(error.retry.clone())
        .with_details(*error.details)
}

fn redact_stream(stream: LlmResponseStream, secrets: Vec<String>) -> LlmResponseStream {
    Box::pin(stream.map(move |item| item.map_err(|error| redact_error(error, &secrets))))
}

/// Build the deterministic diagnostic model used by public proofs.
pub fn diagnostic_model(reference: &str) -> std::result::Result<Arc<dyn Llm>, AdkError> {
    match reference {
        "deterministic" => Ok(Arc::new(DeterministicModel::new())),
        "echo" => Ok(Arc::new(DeterministicModel::echo())),
        value if value.starts_with("http+") => Ok(Arc::new(HttpModel::new(
            value.trim_start_matches("http+"),
            "http",
        ))),
        other => Err(AdkError::config(format!(
            "model reference '{other}' is not configured as a diagnostic provider; configure a provider profile"
        ))),
    }
}
