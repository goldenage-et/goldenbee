//! Gateway-owned provider catalog and encrypted credential vault.
//!
//! The vault stores non-secret provider metadata, versioned ciphertext, Worker
//! enrollment public keys, and one-time pairing-code hashes. It never stores a
//! plaintext credential. A Worker fetches only its assigned providers as short
//! lived X25519 envelopes and decrypts them locally; the Gateway never proxies a
//! prompt or a completion.
//!
//! The file-backed store is the initial adapter. The operations expose a stable
//! seam so a shared database can implement the same catalog later. Multi-replica
//! operation is not claimed for this file adapter.

mod model;
#[cfg(test)]
mod tests;

use base64::Engine;
use common::{
    adapter_discovers_models, ApiError, CreatePairCodeRequest, CreateProviderRequest,
    PairCodeResponse, ProviderAssignment, ProviderBundle, ProviderCredentialSummary,
    ProviderEnvelope, ProviderMetadata, SetProviderCredentialRequest, UpdateProviderRequest,
    WorkerEnrollRequest, WorkerEnrollResponse, MAX_DISCOVERED_MODELS, PAIR_CODE_TTL_MS,
    PROVIDER_ADAPTERS, PROVIDER_ENVELOPE_TTL_MS,
};
use model::{
    Document, PairCodeRecord, ProviderRecord, SecretVersion, StoredSecret, WorkerRecord,
    VAULT_SCHEMA,
};
use provider_crypto::{
    at_rest_aad, envelope_aad, fingerprint, seal_at_rest, seal_envelope, seal_master_check,
    verify_master_check, MasterKey,
};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::registry::Clock;

const MAX_CREDENTIAL_BYTES: usize = 8192;
const VAULT_FILE: &str = "provider-vault.json";

/// File-backed provider catalog and encrypted credential vault.
pub struct Vault {
    clock: Clock,
    path: PathBuf,
    master: MasterKey,
    inner: Mutex<Document>,
}

impl Vault {
    /// Open the configured vault. `None` means provider management is disabled.
    ///
    /// When enabled the master key is resolved once from the configured
    /// environment variable. A missing or malformed key fails startup closed;
    /// the value is never read from Gateway JSON.
    pub fn open(
        config: Option<&crate::config::VaultConfig>,
        base_dir: Option<&Path>,
        clock: Clock,
    ) -> Result<Option<Self>, ApiError> {
        let Some(config) = config else {
            return Ok(None);
        };
        if !config.enabled {
            return Ok(None);
        }
        let value = std::env::var(&config.master_key_env).map_err(|_| {
            ApiError::new(
                "vault.master_key_missing",
                format!(
                    "provider vault is enabled but environment variable '{}' is not set",
                    config.master_key_env
                ),
            )
            .with(
                "hint",
                "set the configured environment variable to a base64-encoded 32-byte key",
            )
        })?;
        let master = MasterKey::parse(&value).map_err(|_| {
            ApiError::new(
                "vault.master_key_invalid",
                "provider vault master key is not a base64-encoded 32-byte value",
            )
        })?;
        let directory = resolve(&config.directory, base_dir);
        Self::open_path(directory.join(VAULT_FILE), master, clock).map(Some)
    }

    /// Open a specific vault document. Used by the Gateway and focused tests.
    pub fn open_path(path: PathBuf, master: MasterKey, clock: Clock) -> Result<Self, ApiError> {
        let mut document = if path.exists() {
            let bytes = std::fs::read(&path).map_err(|_| {
                ApiError::new(
                    "vault.unreadable",
                    "provider vault document could not be read",
                )
            })?;
            let document: Document = serde_json::from_slice(&bytes).map_err(|_| {
                ApiError::new("vault.invalid", "provider vault document is not valid JSON")
            })?;
            if document.version > VAULT_SCHEMA {
                return Err(ApiError::new(
                    "vault.schema_unsupported",
                    format!(
                        "provider vault schema {} is newer than supported {}",
                        document.version, VAULT_SCHEMA
                    ),
                ));
            }
            document
        } else {
            Document::default()
        };
        document.version = VAULT_SCHEMA;
        match &document.master_check {
            Some(check) => {
                let nonce = decode(&check.nonce)?;
                let ciphertext = decode(&check.ciphertext)?;
                verify_master_check(&master, &nonce, &ciphertext).map_err(|_| {
                    ApiError::new(
                        "vault.master_key_mismatch",
                        "provider vault master key does not match this vault document",
                    )
                })?;
            }
            None => {
                let check = seal_master_check(&master).map_err(crypto_error)?;
                document.master_check = Some(StoredSecret {
                    nonce: encode(&check.nonce),
                    ciphertext: encode(&check.ciphertext),
                });
            }
        }
        let vault = Self {
            clock,
            path,
            master,
            inner: Mutex::new(document),
        };
        let snapshot = vault.lock().clone();
        vault.write(&snapshot)?;
        Ok(vault)
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    /// Non-secret catalog in stable id order.
    pub fn list(&self) -> Vec<ProviderMetadata> {
        self.lock()
            .providers
            .values()
            .map(|record| record.metadata.clone())
            .collect()
    }

    /// One non-secret provider entry.
    pub fn provider(&self, id: &str) -> Result<ProviderMetadata, ApiError> {
        self.lock()
            .providers
            .get(id)
            .map(|record| record.metadata.clone())
            .ok_or_else(|| provider_not_found(id))
    }

    /// Create a catalog entry without any credential.
    pub fn create(&self, request: CreateProviderRequest) -> Result<ProviderMetadata, ApiError> {
        validate_provider(
            &request.id,
            &request.adapter,
            &request.model,
            request.endpoint.as_deref(),
            &request.modalities,
            request.context_window,
            &request.residency,
            &request.allowed_workers,
            request.allowed_models.as_deref(),
        )?;
        let now = self.now();
        let discovers = adapter_discovers_models(&request.adapter);
        self.mutate(|document| {
            if document.providers.contains_key(&request.id) {
                return Err(ApiError::new(
                    "provider.exists",
                    format!("provider '{}' already exists", request.id),
                ));
            }
            let models = if discovers {
                Vec::new()
            } else {
                vec![request.model.clone()]
            };
            let metadata = ProviderMetadata {
                id: request.id.clone(),
                adapter: request.adapter,
                model: request.model,
                endpoint: request.endpoint,
                modalities: request.modalities,
                tool_calling: request.tool_calling,
                context_window: request.context_window,
                local: request.local,
                residency: request.residency,
                allowed_workers: request.allowed_workers,
                models,
                models_source: if discovers { "pending" } else { "explicit" }.into(),
                discovered_at: None,
                allowed_models: request.allowed_models,
                allow_auto_default: request.allow_auto_default,
                ready: !discovers,
                discovery_error: discovers
                    .then(|| "write a provider credential to run model discovery".into()),
                discovery_error_code: discovers
                    .then(|| "provider.discovery_credential_missing".into()),
                revision: 1,
                active: true,
                credential_version: None,
                credential_fingerprint: None,
                credential_revoked: false,
                created_at: now,
                updated_at: now,
            };
            document.providers.insert(
                request.id,
                ProviderRecord {
                    metadata: metadata.clone(),
                    secrets: Default::default(),
                },
            );
            Ok(metadata)
        })
    }

    /// Update non-secret metadata with an optimistic revision check.
    pub fn update(
        &self,
        id: &str,
        request: UpdateProviderRequest,
    ) -> Result<ProviderMetadata, ApiError> {
        let now = self.now();
        self.mutate(|document| {
            let record = document
                .providers
                .get_mut(id)
                .ok_or_else(|| provider_not_found(id))?;
            if record.metadata.revision != request.revision {
                return Err(ApiError::new(
                    "provider.revision_conflict",
                    "provider metadata was changed by another decision",
                )
                .with("expected", record.metadata.revision.to_string())
                .with("observed", request.revision.to_string()));
            }
            let previous = record.metadata.clone();
            let mut candidate = record.metadata.clone();
            if let Some(adapter) = request.adapter {
                candidate.adapter = adapter;
            }
            if let Some(model) = request.model {
                candidate.model = model;
            }
            if let Some(endpoint) = request.endpoint {
                candidate.endpoint = (!endpoint.trim().is_empty()).then_some(endpoint);
            }
            if let Some(allowed_models) = request.allowed_models {
                candidate.allowed_models = Some(allowed_models);
            }
            if let Some(allow_auto_default) = request.allow_auto_default {
                candidate.allow_auto_default = allow_auto_default;
            }
            if let Some(modalities) = request.modalities {
                candidate.modalities = modalities;
            }
            if let Some(tool_calling) = request.tool_calling {
                candidate.tool_calling = tool_calling;
            }
            if let Some(context_window) = request.context_window {
                candidate.context_window = (context_window > 0).then_some(context_window);
            }
            if let Some(local) = request.local {
                candidate.local = local;
            }
            if let Some(residency) = request.residency {
                candidate.residency = residency;
            }
            if let Some(allowed_workers) = request.allowed_workers {
                candidate.allowed_workers = allowed_workers;
            }
            if let Some(active) = request.active {
                candidate.active = active;
            }
            validate_provider(
                id,
                &candidate.adapter,
                &candidate.model,
                candidate.endpoint.as_deref(),
                &candidate.modalities,
                candidate.context_window,
                &candidate.residency,
                &candidate.allowed_workers,
                candidate.allowed_models.as_deref(),
            )?;
            let discovery_changed = previous.adapter != candidate.adapter
                || previous.endpoint != candidate.endpoint
                || previous.allowed_models != candidate.allowed_models;
            if adapter_discovers_models(&candidate.adapter) {
                if discovery_changed {
                    candidate.models.clear();
                    candidate.models_source = "pending".into();
                    candidate.discovered_at = None;
                    candidate.discovery_error = Some(
                        "provider endpoint, adapter, or model filter changed; run discovery".into(),
                    );
                    candidate.discovery_error_code = Some("provider.discovery_pending".into());
                    candidate.ready = false;
                } else {
                    recompute_ready(&mut candidate);
                }
            } else {
                explicit_ready(&mut candidate);
            }
            candidate.revision = record.metadata.revision + 1;
            candidate.updated_at = now;
            record.metadata = candidate.clone();
            Ok(candidate)
        })
    }

    /// Encrypt and store a new credential version. Previous versions are
    /// revoked immediately; only the newest version is enveloped.
    pub fn set_credential(
        &self,
        id: &str,
        request: &SetProviderCredentialRequest,
    ) -> Result<ProviderCredentialSummary, ApiError> {
        let value = request.key().trim();
        if value.is_empty() {
            return Err(ApiError::new(
                "provider.credential_empty",
                "provider credential must not be empty",
            ));
        }
        if value.len() > MAX_CREDENTIAL_BYTES {
            return Err(ApiError::new(
                "provider.credential_too_large",
                "provider credential exceeds the accepted size",
            ));
        }
        let now = self.now();
        self.mutate(|document| {
            let record = document
                .providers
                .get_mut(id)
                .ok_or_else(|| provider_not_found(id))?;
            let version = record
                .secrets
                .keys()
                .next_back()
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            let sealed = seal_at_rest(&self.master, &at_rest_aad(id, version), value.as_bytes())
                .map_err(crypto_error)?;
            for secret in record.secrets.values_mut() {
                if secret.revoked_at.is_none() {
                    secret.revoked_at = Some(now);
                }
            }
            let digest = fingerprint(value.as_bytes());
            record.secrets.insert(
                version,
                SecretVersion {
                    nonce: encode(&sealed.nonce),
                    ciphertext: encode(&sealed.ciphertext),
                    fingerprint: digest.clone(),
                    created_at: now,
                    revoked_at: None,
                },
            );
            record.metadata.credential_version = Some(version);
            record.metadata.credential_fingerprint = Some(digest.clone());
            record.metadata.credential_revoked = false;
            record.metadata.revision = record.metadata.revision.saturating_add(1);
            record.metadata.updated_at = now;
            Ok(summary_of(&record.metadata, version, digest, now, None))
        })
    }

    /// Revoke the active credential. New envelopes stop immediately.
    pub fn revoke_credential(&self, id: &str) -> Result<ProviderCredentialSummary, ApiError> {
        let now = self.now();
        self.mutate(|document| {
            let record = document
                .providers
                .get_mut(id)
                .ok_or_else(|| provider_not_found(id))?;
            let version = record
                .secrets
                .iter()
                .rev()
                .find(|(_, secret)| secret.revoked_at.is_none())
                .map(|(version, _)| *version)
                .or_else(|| record.secrets.keys().next_back().copied())
                .ok_or_else(|| {
                    ApiError::new(
                        "provider.credential_missing",
                        "provider has no credential to revoke",
                    )
                })?;
            let already = {
                let secret = record
                    .secrets
                    .get(&version)
                    .ok_or_else(|| provider_not_found(id))?;
                secret.revoked_at.is_some()
            };
            if !already {
                if let Some(secret) = record.secrets.get_mut(&version) {
                    secret.revoked_at = Some(now);
                }
                record.metadata.credential_revoked = true;
                record.metadata.ready = false;
                record.metadata.discovery_error =
                    Some("credential is revoked; write a new credential and run discovery".into());
                record.metadata.discovery_error_code = Some("provider.credential_revoked".into());
                record.metadata.revision = record.metadata.revision.saturating_add(1);
                record.metadata.updated_at = now;
            }
            let secret = record
                .secrets
                .get(&version)
                .ok_or_else(|| provider_not_found(id))?;
            Ok(summary_of(
                &record.metadata,
                version,
                secret.fingerprint.clone(),
                secret.created_at,
                secret.revoked_at,
            ))
        })
    }

    /// Mint a one-time pairing code bound to a Worker and instance. Only the
    /// hash is stored; the code is returned exactly once.
    pub fn create_pair_code(
        &self,
        request: CreatePairCodeRequest,
    ) -> Result<PairCodeResponse, ApiError> {
        if request.worker_id.trim().is_empty() || request.worker_id.len() > 128 {
            return Err(ApiError::new(
                "provider.worker_id_invalid",
                "pair code Worker id must be 1-128 characters",
            ));
        }
        if request.instance.trim().is_empty() || request.instance.len() > 128 {
            return Err(ApiError::new(
                "provider.instance_invalid",
                "pair code instance must be 1-128 characters",
            ));
        }
        let now = self.now();
        let expires_at = now.saturating_add(PAIR_CODE_TTL_MS);
        let code = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let hash = common::bytes_digest(code.as_bytes());
        self.mutate(|document| {
            document
                .pair_codes
                .retain(|_, record| record.expires_at > now);
            document.pair_codes.insert(
                hash,
                PairCodeRecord {
                    worker_id: request.worker_id.clone(),
                    instance: request.instance.clone(),
                    expires_at,
                    created_at: now,
                    used_at: None,
                },
            );
            Ok(())
        })?;
        let link = format!(
            "goldenbee://pair/provider?code={}&worker={}&instance={}",
            code, request.worker_id, request.instance
        );
        Ok(PairCodeResponse {
            code,
            link,
            worker_id: request.worker_id,
            instance: request.instance,
            expires_at,
        })
    }

    /// Redeem a one-time pairing code and bind the Worker's public key.
    pub fn enroll(&self, request: &WorkerEnrollRequest) -> Result<WorkerEnrollResponse, ApiError> {
        let public_key = decode(&request.public_key).map_err(|_| {
            ApiError::new(
                "provider.public_key_invalid",
                "Worker public key must be base64-encoded 32-byte X25519 material",
            )
        })?;
        if public_key.len() != 32 || public_key.iter().all(|byte| *byte == 0) {
            return Err(ApiError::new(
                "provider.public_key_invalid",
                "Worker public key must be base64-encoded 32-byte X25519 material",
            ));
        }
        let hash = common::bytes_digest(request.code.as_bytes());
        let now = self.now();
        self.mutate(|document| {
            let record = document
                .pair_codes
                .get(&hash)
                .ok_or_else(pair_code_invalid)?;
            if record.used_at.is_some() || record.expires_at <= now {
                return Err(pair_code_invalid());
            }
            if record.worker_id != request.worker_id || record.instance != request.instance {
                return Err(pair_code_invalid());
            }
            if let Some(code) = document.pair_codes.get_mut(&hash) {
                code.used_at = Some(now);
            }
            document.workers.insert(
                request.worker_id.clone(),
                WorkerRecord {
                    worker_id: request.worker_id.clone(),
                    instance: request.instance.clone(),
                    public_key: request.public_key.clone(),
                    enrolled_at: now,
                },
            );
            Ok(WorkerEnrollResponse {
                worker_id: request.worker_id.clone(),
                instance: request.instance.clone(),
                enrolled_at: now,
            })
        })
    }

    /// Record one discovery attempt and recompute readiness.
    ///
    /// `failure` stores a stable code and a redacted actionable message. A
    /// failure clears the discovered model list so a Worker never receives
    /// stale aliases from a previous endpoint generation.
    pub(crate) fn record_discovery(
        &self,
        id: &str,
        models: Vec<String>,
        failure: Option<(String, String)>,
    ) -> Result<ProviderMetadata, ApiError> {
        let now = self.now();
        self.mutate(|document| {
            let record = document
                .providers
                .get_mut(id)
                .ok_or_else(|| provider_not_found(id))?;
            record.metadata.models_source = "discovered".into();
            record.metadata.discovered_at = Some(now);
            record.metadata.updated_at = now;
            if let Some((code, message)) = failure {
                record.metadata.models.clear();
                record.metadata.ready = false;
                record.metadata.discovery_error = Some(message);
                record.metadata.discovery_error_code = Some(code);
            } else {
                let mut models = normalize_models(models)?;
                if let Some(allowed) = &record.metadata.allowed_models {
                    models.retain(|model| allowed.iter().any(|value| value == model));
                }
                record.metadata.models = models;
                record.metadata.discovery_error = None;
                record.metadata.discovery_error_code = None;
                recompute_ready(&mut record.metadata);
            }
            record.metadata.revision = record.metadata.revision.saturating_add(1);
            Ok(record.metadata.clone())
        })
    }

    /// Read the active credential for a server-side discovery request. The
    /// returned value is zeroized on drop and is never logged or serialized.
    pub(crate) fn credential_plaintext(
        &self,
        id: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, ApiError> {
        let document = self.lock();
        let record = document
            .providers
            .get(id)
            .ok_or_else(|| provider_not_found(id))?;
        let Some((version, secret)) = record.active_secret() else {
            return Ok(None);
        };
        let nonce = decode(&secret.nonce)?;
        let ciphertext = decode(&secret.ciphertext)?;
        let plaintext = provider_crypto::open_at_rest(
            &self.master,
            &at_rest_aad(id, version),
            &nonce,
            &ciphertext,
        )
        .map_err(|_| {
            ApiError::new(
                "provider.credential_unavailable",
                "stored provider credential could not be decrypted",
            )
        })?;
        Ok(Some(plaintext))
    }

    /// Issue short-lived envelopes for the providers assigned to one enrolled
    /// Worker. Revoked, inactive, or not-ready credentials are omitted.
    pub fn bundle(
        &self,
        worker_id: &str,
        instance: &str,
        ttl_ms: i64,
    ) -> Result<ProviderBundle, ApiError> {
        let now = self.now();
        let document = self.lock();
        let worker = document.workers.get(worker_id).ok_or_else(|| {
            ApiError::new(
                "provider.worker_not_enrolled",
                "Worker has no enrolled provider encryption key",
            )
        })?;
        if worker.instance != instance {
            return Err(ApiError::new(
                "provider.instance_mismatch",
                "Worker instance does not match its provider enrollment",
            ));
        }
        let public_key = decode(&worker.public_key)?;
        let public_key: [u8; 32] = public_key.try_into().map_err(|_| {
            ApiError::new(
                "provider.public_key_invalid",
                "enrolled Worker public key is not 32 bytes",
            )
        })?;
        let ttl = ttl_ms.clamp(1, PROVIDER_ENVELOPE_TTL_MS);
        let expires_at = now.saturating_add(ttl);
        let mut providers = Vec::new();
        for record in document.providers.values() {
            let metadata = &record.metadata;
            if !metadata.active || !metadata.ready || !assigned(metadata, worker_id) {
                continue;
            }
            let Some((version, secret)) = record.active_secret() else {
                continue;
            };
            if secret.revoked_at.is_some() {
                continue;
            }
            let nonce = decode(&secret.nonce)?;
            let ciphertext = decode(&secret.ciphertext)?;
            let plaintext = provider_crypto::open_at_rest(
                &self.master,
                &at_rest_aad(&metadata.id, version),
                &nonce,
                &ciphertext,
            )
            .map_err(|_| {
                ApiError::new(
                    "provider.credential_unavailable",
                    "stored provider credential could not be decrypted",
                )
            })?;
            let sealed = seal_envelope(
                &public_key,
                &envelope_aad(&metadata.id, version, worker_id, instance, expires_at),
                plaintext.as_slice(),
            )
            .map_err(crypto_error)?;
            providers.push(ProviderAssignment {
                metadata: metadata.clone(),
                envelope: ProviderEnvelope {
                    provider: metadata.id.clone(),
                    version,
                    ephemeral_public_key: encode(&sealed.ephemeral_public_key),
                    nonce: encode(&sealed.nonce),
                    ciphertext: encode(&sealed.ciphertext),
                    expires_at,
                },
            });
        }
        providers.sort_by(|left, right| left.metadata.id.cmp(&right.metadata.id));
        Ok(ProviderBundle {
            worker: worker_id.to_string(),
            instance: instance.to_string(),
            issued_at: now,
            expires_at,
            providers,
        })
    }

    fn mutate<T>(
        &self,
        change: impl FnOnce(&mut Document) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let mut inner = self.lock();
        let value = change(&mut inner)?;
        inner.version = VAULT_SCHEMA;
        self.write(&inner)?;
        Ok(value)
    }

    fn lock(&self) -> MutexGuard<'_, Document> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn write(&self, document: &Document) -> Result<(), ApiError> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(persist_error)?;
        let bytes = serde_json::to_vec(document).map_err(|_| {
            ApiError::new(
                "storage.unavailable",
                "provider vault state could not be serialized",
            )
        })?;
        let temporary = self.path.with_extension("tmp");
        let result = (|| -> std::io::Result<()> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            restrict_permissions(&temporary)?;
            std::fs::rename(&temporary, &self.path)?;
            restrict_permissions(&self.path)?;
            sync_parent(&self.path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map_err(persist_error)
    }
}

fn assigned(metadata: &ProviderMetadata, worker_id: &str) -> bool {
    metadata
        .allowed_workers
        .iter()
        .any(|allowed| allowed == "*" || allowed == worker_id)
}

fn normalize_models(models: Vec<String>) -> Result<Vec<String>, ApiError> {
    let mut unique = BTreeSet::new();
    for model in models {
        let model = model.trim();
        if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
            continue;
        }
        unique.insert(model.to_string());
        if unique.len() > MAX_DISCOVERED_MODELS {
            return Err(ApiError::new(
                "provider.discovery_too_many",
                "provider discovery returned more models than this Gateway accepts; set allowed_models",
            ));
        }
    }
    Ok(unique.into_iter().collect())
}

fn explicit_ready(metadata: &mut ProviderMetadata) {
    metadata.models_source = "explicit".into();
    metadata.models = if metadata.model.is_empty() {
        Vec::new()
    } else {
        vec![metadata.model.clone()]
    };
    metadata.discovered_at = None;
    metadata.ready = !metadata.model.trim().is_empty();
    metadata.discovery_error = None;
    metadata.discovery_error_code = None;
}

fn recompute_ready(metadata: &mut ProviderMetadata) {
    metadata.models_source = "discovered".into();
    let needs_credential = matches!(
        metadata.adapter.as_str(),
        "openrouter" | "openai-compatible"
    );
    if needs_credential && (metadata.credential_version.is_none() || metadata.credential_revoked) {
        metadata.ready = false;
        metadata.discovery_error =
            Some("write a provider credential to run model discovery".into());
        metadata.discovery_error_code = Some("provider.discovery_credential_missing".into());
        return;
    }
    if metadata.models.is_empty() {
        metadata.ready = false;
        metadata.discovery_error =
            Some("provider discovery has not returned any usable model".into());
        metadata.discovery_error_code = Some("provider.discovery_empty".into());
        return;
    }
    if metadata.model.trim().is_empty() {
        if metadata.allow_auto_default {
            metadata.model = metadata.models[0].clone();
        } else {
            metadata.ready = false;
            metadata.discovery_error = Some(
                "discovery found models but no default model is selected; set model or allow_auto_default"
                    .into(),
            );
            metadata.discovery_error_code = Some("provider.default_model_required".into());
            return;
        }
    }
    if !metadata.models.iter().any(|model| model == &metadata.model) {
        metadata.ready = false;
        metadata.discovery_error =
            Some("configured default model is not present in discovered models".into());
        metadata.discovery_error_code = Some("provider.default_model_unavailable".into());
        return;
    }
    metadata.ready = true;
    metadata.discovery_error = None;
    metadata.discovery_error_code = None;
}

fn summary_of(
    metadata: &ProviderMetadata,
    version: u64,
    fingerprint: String,
    created_at: i64,
    revoked_at: Option<i64>,
) -> ProviderCredentialSummary {
    ProviderCredentialSummary {
        provider: metadata.id.clone(),
        version,
        fingerprint,
        created_at,
        revoked_at,
        revision: metadata.revision,
        ready: metadata.ready,
        models: metadata.models.clone(),
        discovered_at: metadata.discovered_at,
        discovery_error: metadata.discovery_error.clone(),
        discovery_error_code: metadata.discovery_error_code.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_provider(
    id: &str,
    adapter: &str,
    model: &str,
    endpoint: Option<&str>,
    modalities: &[String],
    context_window: Option<u64>,
    residency: &str,
    allowed_workers: &[String],
    allowed_models: Option<&[String]>,
) -> Result<(), ApiError> {
    if !valid_id(id, 64) {
        return Err(ApiError::new(
            "provider.id_invalid",
            "provider id must be 1-64 characters of [A-Za-z0-9_-]",
        ));
    }
    if !PROVIDER_ADAPTERS.contains(&adapter) {
        return Err(ApiError::new(
            "provider.adapter_unsupported",
            format!("provider adapter '{adapter}' is not supported"),
        ));
    }
    if model.len() > 256 || model.chars().any(char::is_control) {
        return Err(ApiError::new(
            "provider.model_invalid",
            "provider model must be at most 256 visible characters",
        ));
    }
    if !adapter_discovers_models(adapter) && model.trim().is_empty() {
        return Err(ApiError::new(
            "provider.model_invalid",
            "this adapter requires an explicit model",
        ));
    }
    if let Some(allowed_models) = allowed_models {
        if allowed_models.len() > MAX_DISCOVERED_MODELS {
            return Err(ApiError::new(
                "provider.allowed_models_invalid",
                "allowed_models contains too many entries",
            ));
        }
        for allowed in allowed_models {
            if allowed.trim().is_empty()
                || allowed.len() > 256
                || allowed.chars().any(char::is_control)
            {
                return Err(ApiError::new(
                    "provider.allowed_models_invalid",
                    "allowed model names must be 1-256 visible characters",
                ));
            }
        }
    }
    if residency.trim().is_empty() || residency.len() > 64 {
        return Err(ApiError::new(
            "provider.residency_invalid",
            "provider residency must be 1-64 characters",
        ));
    }
    if modalities.is_empty() {
        return Err(ApiError::new(
            "provider.modalities_invalid",
            "provider must declare at least one modality",
        ));
    }
    for modality in modalities {
        if modality.trim().is_empty() || modality.len() > 32 {
            return Err(ApiError::new(
                "provider.modalities_invalid",
                "provider modality names must be 1-32 characters",
            ));
        }
    }
    if context_window.is_some_and(|window| window == 0 || window > 10_000_000) {
        return Err(ApiError::new(
            "provider.context_window_invalid",
            "provider context window must be between 1 and 10000000",
        ));
    }
    if let Some(endpoint) = endpoint.filter(|value| !value.trim().is_empty()) {
        let url = reqwest::Url::parse(endpoint).map_err(|_| {
            ApiError::new(
                "provider.endpoint_invalid",
                "provider endpoint must be an HTTP or HTTPS base URL",
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
                "provider endpoint must not embed credentials",
            ));
        }
    }
    for worker in allowed_workers {
        if worker != "*" && !valid_id(worker, 128) {
            return Err(ApiError::new(
                "provider.worker_assignment_invalid",
                "provider assignments must be '*' or approved Worker ids",
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

fn provider_not_found(id: &str) -> ApiError {
    ApiError::new(
        "provider.not_found",
        format!("provider '{id}' is not configured"),
    )
}

fn pair_code_invalid() -> ApiError {
    ApiError::new(
        "provider.pair_code_invalid",
        "pairing code is unknown, expired, used, or bound to another Worker",
    )
}

fn crypto_error(error: provider_crypto::CryptoError) -> ApiError {
    ApiError::new("provider.crypto_failed", error.to_string())
}

fn persist_error(error: std::io::Error) -> ApiError {
    ApiError::retryable(
        "storage.unavailable",
        format!("provider vault state could not be persisted: {error}"),
    )
}

fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode(value: &str) -> Result<Vec<u8>, ApiError> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| {
            ApiError::new(
                "provider.envelope_invalid",
                "provider vault value is not valid base64",
            )
        })
}

fn resolve(value: &str, base: Option<&Path>) -> PathBuf {
    let path = Path::new(value);
    match (base, path.is_absolute()) {
        (Some(base), false) => base.join(path),
        _ => path.to_path_buf(),
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
