//! Gateway-managed model provider loading.
//!
//! A Worker persists one X25519 private key with mode 0600, fetches its assigned
//! provider metadata and short-lived encrypted envelopes with its control token,
//! decrypts locally, and builds the existing native provider registry. The cache
//! holds only the encrypted bundle.

use crate::execution::providers::{MemorySecrets, ProviderRegistry};
use crate::worker::config::{ResolvedModels, WorkerSettings};
use crate::worker::WorkerResult;
use crate::ProviderProfile;
use base64::Engine;
use common::{ApiError, ProviderBundle, WorkerEnrollRequest, WorkerEnrollResponse};
use provider_crypto::{envelope_aad, open_envelope, public_key};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use zeroize::Zeroizing;

/// Load the effective provider registry for this instance.
///
/// `models.source = "local"` keeps the existing environment/provider behavior.
/// `gateway` fetches and decrypts the assigned bundle; when the Gateway cannot be
/// reached, an unexpired encrypted cache is used before the configured fallback
/// policy is applied.
pub(crate) async fn load(settings: &WorkerSettings) -> WorkerResult<Arc<ProviderRegistry>> {
    let Some(models) = settings.models()? else {
        return ProviderRegistry::from_profiles(
            settings.provider_profiles(),
            settings.file.allow_remote,
        );
    };
    match load_gateway(settings, &models).await {
        Ok(registry) => Ok(registry),
        Err(error) if models.fallback == "deterministic" => {
            eprintln!(
                "models: Gateway provider bundle unavailable ({}); using deterministic fallback",
                error.code
            );
            deterministic_fallback(settings)
        }
        Err(error) => Err(error),
    }
}

async fn load_gateway(
    settings: &WorkerSettings,
    models: &ResolvedModels,
) -> WorkerResult<Arc<ProviderRegistry>> {
    let private_key = load_or_create_private_key(&models.private_key_path)?;
    let now = common::now_ms();
    let cached = read_cache(&models.cache_path)?;
    let fresh = cached
        .as_ref()
        .filter(|bundle| usable(bundle, now, Some(models.refresh_ttl_ms)))
        .cloned();
    let bundle = match fresh {
        Some(bundle) => bundle,
        None => match fetch_bundle(models).await {
            Ok(bundle) => {
                write_cache(&models.cache_path, &bundle)?;
                bundle
            }
            Err(error) => match cached.filter(|bundle| usable(bundle, now, None)) {
                Some(bundle) => {
                    eprintln!(
                        "models: Gateway provider bundle unavailable ({}); using unexpired encrypted cache",
                        error.code
                    );
                    bundle
                }
                None => return Err(error),
            },
        },
    };
    registry_from_bundle(settings, models, &private_key, &bundle)
}

fn registry_from_bundle(
    settings: &WorkerSettings,
    models: &ResolvedModels,
    private_key: &[u8; 32],
    bundle: &ProviderBundle,
) -> WorkerResult<Arc<ProviderRegistry>> {
    if bundle.worker != models.worker_id {
        return Err(ApiError::new(
            "provider.bundle_worker_mismatch",
            "Gateway provider bundle is addressed to another Worker",
        ));
    }
    let instance = settings.file.instance.as_str();
    if bundle.instance != instance {
        return Err(ApiError::new(
            "provider.bundle_instance_mismatch",
            "Gateway provider bundle is addressed to another Worker instance",
        ));
    }
    let now = common::now_ms();
    if bundle.expires_at <= now {
        return Err(ApiError::new(
            "provider.bundle_expired",
            "Gateway provider bundle has expired; refresh it before serving",
        ));
    }
    let mut secrets = BTreeMap::new();
    // Built-in local diagnostic providers remain available when the Gateway
    // vault is the primary source. Packages such as `general` register a
    // manual diagnostic agent against the deterministic provider.
    let mut profiles: BTreeMap<String, ProviderProfile> = settings
        .provider_profiles()
        .into_iter()
        .filter(|(_, profile)| profile.local && profile.adapter == "deterministic")
        .collect();
    for assignment in &bundle.providers {
        let envelope = &assignment.envelope;
        if envelope.provider != assignment.metadata.id {
            return Err(ApiError::new(
                "provider.envelope_invalid",
                "provider envelope names a different provider",
            ));
        }
        if envelope.expires_at <= now {
            return Err(ApiError::new(
                "provider.envelope_expired",
                "provider envelope has expired; refresh the Worker bundle",
            ));
        }
        let ephemeral: [u8; 32] =
            decode(&envelope.ephemeral_public_key)?
                .try_into()
                .map_err(|_| {
                    ApiError::new(
                        "provider.envelope_invalid",
                        "provider envelope ephemeral key is not 32 bytes",
                    )
                })?;
        let nonce = decode(&envelope.nonce)?;
        let ciphertext = decode(&envelope.ciphertext)?;
        let aad = envelope_aad(
            &assignment.metadata.id,
            envelope.version,
            &bundle.worker,
            &bundle.instance,
            envelope.expires_at,
        );
        let plaintext =
            open_envelope(private_key, &ephemeral, &nonce, &ciphertext, &aad).map_err(|_| {
                ApiError::new(
                    "provider.envelope_decrypt_failed",
                    "provider envelope could not be decrypted with this Worker key",
                )
            })?;
        let value = String::from_utf8(plaintext.to_vec()).map_err(|_| {
            ApiError::new(
                "provider.credential_invalid",
                "decrypted provider credential is not UTF-8",
            )
        })?;
        let metadata = &assignment.metadata;
        if metadata.model.trim().is_empty() {
            return Err(ApiError::new(
                "provider.bundle_invalid",
                "assigned provider has no usable default model",
            ));
        }
        let models = if metadata.models.is_empty() {
            vec![metadata.model.clone()]
        } else {
            metadata.models.clone()
        };
        if !models.iter().any(|model| model == &metadata.model) {
            return Err(ApiError::new(
                "provider.bundle_invalid",
                "assigned provider default model is not in its model list",
            ));
        }
        let reference = format!("gateway:{}", metadata.id);
        secrets.insert(reference.clone(), value);
        let profile = |model: String| ProviderProfile {
            adapter: metadata.adapter.clone(),
            model,
            endpoint: metadata.endpoint.clone(),
            modalities: metadata.modalities.clone(),
            tool_calling: metadata.tool_calling,
            local: metadata.local,
            residency: metadata.residency.clone(),
            context_window: metadata.context_window,
            credential: Some(reference.clone()),
            cost_rank: None,
            input_cost_micros_per_million: None,
            candidate_cost_micros_per_million: None,
            data_class: None,
            fallbacks: Vec::new(),
        };
        profiles.insert(metadata.id.clone(), profile(metadata.model.clone()));
        for model in models {
            let alias = format!("{}/{}", metadata.id, model);
            profiles.insert(alias, profile(model));
        }
    }
    if bundle.providers.is_empty() {
        return Err(ApiError::new(
            "provider.no_providers",
            "Gateway assigned no provider credentials to this Worker",
        ));
    }
    // The Gateway operator approved these providers and their assignments. A
    // remote declaration is therefore explicit authority for this Worker.
    let allow_remote =
        settings.file.allow_remote || profiles.values().any(|profile| !profile.local);
    ProviderRegistry::new(
        profiles,
        allow_remote,
        Arc::new(MemorySecrets::new(secrets)),
    )
}

/// Enroll this Worker with a one-time pairing code. The private key is created
/// or loaded before the request and never transmitted.
pub(crate) async fn enroll(
    settings: &WorkerSettings,
    code: &str,
) -> WorkerResult<WorkerEnrollResponse> {
    let models = settings.models()?.ok_or_else(|| {
        ApiError::new(
            "provider.enrollment_disabled",
            "Worker model configuration does not use the Gateway provider vault",
        )
    })?;
    let private_key = load_or_create_private_key(&models.private_key_path)?;
    let public = public_key(&private_key);
    let request = WorkerEnrollRequest {
        code: code.trim().to_string(),
        worker_id: models.worker_id.clone(),
        instance: settings.file.instance.clone(),
        public_key: encode(&public),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| {
            ApiError::new(
                "provider.enroll_unavailable",
                "Worker provider enrollment HTTP client could not be built",
            )
        })?;
    let response = client
        .post(format!(
            "{}/v1/model-providers/enroll",
            models.gateway_url.trim_end_matches('/')
        ))
        .bearer_auth(&models.control_token)
        .json(&request)
        .send()
        .await
        .map_err(|_| {
            ApiError::new(
                "provider.enroll_unavailable",
                "Worker provider enrollment request did not reach the Gateway",
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        if let Ok(error) = response.json::<ApiError>().await {
            return Err(error);
        }
        return Err(ApiError::new(
            "provider.enroll_rejected",
            format!("Gateway rejected Worker provider enrollment with HTTP {status}"),
        ));
    }
    response.json::<WorkerEnrollResponse>().await.map_err(|_| {
        ApiError::new(
            "provider.enroll_invalid",
            "Gateway returned an invalid provider enrollment response",
        )
    })
}

async fn fetch_bundle(models: &ResolvedModels) -> WorkerResult<ProviderBundle> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| {
            ApiError::new(
                "provider.bundle_unavailable",
                "Worker provider bundle HTTP client could not be built",
            )
        })?;
    let response = client
        .get(format!(
            "{}/v1/model-providers",
            models.gateway_url.trim_end_matches('/')
        ))
        .bearer_auth(&models.control_token)
        .send()
        .await
        .map_err(|_| {
            ApiError::new(
                "provider.bundle_unavailable",
                "Gateway provider bundle request did not reach the Gateway",
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        if let Ok(error) = response.json::<ApiError>().await {
            return Err(error);
        }
        return Err(ApiError::new(
            "provider.bundle_unavailable",
            format!("Gateway provider bundle request failed with HTTP {status}"),
        ));
    }
    response.json::<ProviderBundle>().await.map_err(|_| {
        ApiError::new(
            "provider.bundle_invalid",
            "Gateway returned an invalid provider bundle",
        )
    })
}

fn deterministic_fallback(settings: &WorkerSettings) -> WorkerResult<Arc<ProviderRegistry>> {
    let mut profiles = settings.provider_profiles();
    let id = settings.file.default_model.clone();
    profiles.entry(id).or_insert_with(|| ProviderProfile {
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
    });
    ProviderRegistry::from_profiles(profiles, false)
}

fn usable(bundle: &ProviderBundle, now: i64, max_age_ms: Option<u64>) -> bool {
    if bundle.expires_at <= now
        || bundle
            .providers
            .iter()
            .any(|assignment| assignment.envelope.expires_at <= now)
    {
        return false;
    }
    match max_age_ms {
        Some(max_age) => now.saturating_sub(bundle.issued_at) <= max_age as i64,
        None => true,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedBundle {
    version: u64,
    bundle: ProviderBundle,
}

const CACHE_VERSION: u64 = 1;

fn read_cache(path: &Path) -> WorkerResult<Option<ProviderBundle>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ApiError::new(
                "provider.cache_read_failed",
                format!("encrypted provider cache could not be read: {error}"),
            ))
        }
    };
    let cached: CachedBundle = serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::new(
            "provider.cache_invalid",
            "encrypted provider cache is not valid JSON",
        )
    })?;
    if cached.version != CACHE_VERSION {
        return Ok(None);
    }
    Ok(Some(cached.bundle))
}

fn write_cache(path: &Path, bundle: &ProviderBundle) -> WorkerResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| {
        ApiError::new(
            "provider.cache_write_failed",
            format!("encrypted provider cache directory could not be created: {error}"),
        )
    })?;
    let bytes = serde_json::to_vec(&CachedBundle {
        version: CACHE_VERSION,
        bundle: bundle.clone(),
    })
    .map_err(|_| {
        ApiError::new(
            "provider.cache_write_failed",
            "encrypted provider bundle could not be serialized",
        )
    })?;
    let temporary = path.with_extension("tmp");
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
        std::fs::rename(&temporary, path)?;
        restrict_permissions(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| {
        ApiError::new(
            "provider.cache_write_failed",
            format!("encrypted provider cache could not be persisted: {error}"),
        )
    })
}

/// Load the Worker's X25519 private key, creating it with mode 0600 when absent.
pub(crate) fn load_or_create_private_key(path: &Path) -> WorkerResult<[u8; 32]> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let key = parse_private_key(&text)?;
            let _ = restrict_permissions(path);
            Ok(key)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => create_private_key(path),
        Err(error) => Err(ApiError::new(
            "provider.private_key_read_failed",
            format!("Worker provider private key could not be read: {error}"),
        )),
    }
}

fn create_private_key(path: &Path) -> WorkerResult<[u8; 32]> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| {
        ApiError::new(
            "provider.private_key_write_failed",
            format!("Worker provider key directory could not be created: {error}"),
        )
    })?;
    let key = Zeroizing::new(provider_crypto::generate_private_key());
    let text = format!("{}\n", encode(key.as_ref()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(text.as_bytes()).map_err(|error| {
                ApiError::new(
                    "provider.private_key_write_failed",
                    format!("Worker provider private key could not be written: {error}"),
                )
            })?;
            file.sync_all().map_err(|error| {
                ApiError::new(
                    "provider.private_key_write_failed",
                    format!("Worker provider private key could not be synced: {error}"),
                )
            })?;
            restrict_permissions(path).map_err(|error| {
                ApiError::new(
                    "provider.private_key_write_failed",
                    format!("Worker provider private key permissions could not be set: {error}"),
                )
            })?;
            Ok(*key)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let text = std::fs::read_to_string(path).map_err(|error| {
                ApiError::new(
                    "provider.private_key_read_failed",
                    format!("Worker provider private key could not be read: {error}"),
                )
            })?;
            parse_private_key(&text)
        }
        Err(error) => Err(ApiError::new(
            "provider.private_key_write_failed",
            format!("Worker provider private key could not be created: {error}"),
        )),
    }
}

fn parse_private_key(text: &str) -> WorkerResult<[u8; 32]> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|_| {
            ApiError::new(
                "provider.private_key_invalid",
                "Worker provider private key is not valid base64",
            )
        })?;
    bytes.try_into().map_err(|_| {
        ApiError::new(
            "provider.private_key_invalid",
            "Worker provider private key is not 32 bytes",
        )
    })
}

fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode(value: &str) -> WorkerResult<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| {
            ApiError::new(
                "provider.envelope_invalid",
                "provider envelope value is not valid base64",
            )
        })
}

fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::ProviderAssignment;
    use provider_crypto::{envelope_aad, seal_envelope};

    fn write_settings(directory: &Path) -> WorkerSettings {
        let agents = directory.join("agents");
        let prompts = directory.join("prompts");
        std::fs::create_dir_all(&agents).expect("agents");
        std::fs::create_dir_all(&prompts).expect("prompts");
        std::fs::write(
            agents.join("general.toml"),
            "name = \"general\"\ndescription = \"General\"\nmodel = \"openrouter\"\nprompt = \"general\"\n",
        )
        .expect("agent");
        std::fs::write(prompts.join("general.md"), "prompt").expect("prompt");
        let config = directory.join("worker.toml");
        std::fs::write(
            &config,
            "instance = \"general-a\"\ndefault_agent = \"general\"\ndefault_model = \"openrouter\"\nallow_remote = true\n[paths]\nagents = \"agents\"\nprompts = \"prompts\"\n[storage]\ndirectory = \"state\"\n[models]\nsource = \"gateway\"\ngateway_url = \"http://127.0.0.1:1\"\ncontrol_token = \"control\"\nfallback = \"deny\"\n",
        )
        .expect("config");
        WorkerSettings::load(&config).expect("settings")
    }

    fn assignment(worker: &str, private: &[u8; 32], expires_at: i64) -> ProviderAssignment {
        let metadata = common::ProviderMetadata {
            id: "openrouter".into(),
            adapter: "openrouter".into(),
            model: "test/model".into(),
            endpoint: None,
            modalities: vec!["text".into()],
            tool_calling: true,
            context_window: Some(128_000),
            local: false,
            residency: "remote".into(),
            allowed_workers: vec![worker.into()],
            models: vec!["test/model".into()],
            models_source: "discovered".into(),
            discovered_at: Some(0),
            allowed_models: None,
            allow_auto_default: false,
            ready: true,
            discovery_error: None,
            discovery_error_code: None,
            revision: 1,
            active: true,
            credential_version: Some(1),
            credential_fingerprint: Some("sha256:test".into()),
            credential_revoked: false,
            created_at: 0,
            updated_at: 0,
        };
        let aad = envelope_aad("openrouter", 1, worker, worker, expires_at);
        let sealed = seal_envelope(
            &provider_crypto::public_key(private),
            &aad,
            b"sk-gateway-key",
        )
        .expect("envelope");
        ProviderAssignment {
            metadata,
            envelope: common::ProviderEnvelope {
                provider: "openrouter".into(),
                version: 1,
                ephemeral_public_key: encode(&sealed.ephemeral_public_key),
                nonce: encode(&sealed.nonce),
                ciphertext: encode(&sealed.ciphertext),
                expires_at,
            },
        }
    }

    #[test]
    fn private_key_is_created_0600_and_reused() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("nested/provider-key");
        let first = load_or_create_private_key(&path).expect("created");
        let second = load_or_create_private_key(&path).expect("reused");
        assert_eq!(first, second);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn decrypted_bundle_builds_gateway_profiles_and_cache_has_no_plaintext() {
        let directory = tempfile::tempdir().expect("directory");
        let settings = write_settings(directory.path());
        let models = settings.models().expect("models").expect("gateway");
        let private = provider_crypto::generate_private_key();
        let expires_at = common::now_ms() + 60_000;
        let bundle = ProviderBundle {
            worker: "general-a".into(),
            instance: "general-a".into(),
            issued_at: common::now_ms(),
            expires_at,
            providers: vec![assignment("general-a", &private, expires_at)],
        };
        let registry =
            registry_from_bundle(&settings, &models, &private, &bundle).expect("registry");
        let view = registry.view("openrouter").expect("view");
        assert_eq!(view.credential.as_deref(), Some("gateway:openrouter"));
        assert_eq!(view.model, "test/model");
        let alias = registry
            .view("openrouter/test/model")
            .expect("concrete model alias");
        assert_eq!(alias.credential.as_deref(), Some("gateway:openrouter"));
        assert_eq!(alias.model, "test/model");
        assert!(registry
            .preflight("openrouter", &crate::ProviderRequirements::text())
            .is_ok());
        // Built-in local diagnostics remain available for packages that bind
        // a manual diagnostic agent while using the Gateway provider vault.
        assert!(registry
            .preflight("deterministic", &crate::ProviderRequirements::text())
            .is_ok());

        write_cache(&models.cache_path, &bundle).expect("cache");
        let cached = std::fs::read_to_string(&models.cache_path).expect("cache text");
        assert!(!cached.contains("sk-gateway-key"));
        assert!(read_cache(&models.cache_path).expect("read").is_some());
    }

    #[test]
    fn another_worker_key_cannot_decrypt_the_bundle() {
        let directory = tempfile::tempdir().expect("directory");
        let settings = write_settings(directory.path());
        let models = settings.models().expect("models").expect("gateway");
        let intended = provider_crypto::generate_private_key();
        let other = provider_crypto::generate_private_key();
        let expires_at = common::now_ms() + 60_000;
        let bundle = ProviderBundle {
            worker: "general-a".into(),
            instance: "general-a".into(),
            issued_at: common::now_ms(),
            expires_at,
            providers: vec![assignment("general-a", &intended, expires_at)],
        };
        let error = match registry_from_bundle(&settings, &models, &other, &bundle) {
            Ok(_) => panic!("another Worker key must not decrypt"),
            Err(error) => error,
        };
        assert_eq!(error.code, "provider.envelope_decrypt_failed");
    }

    #[test]
    fn cache_validity_honors_expiry_and_refresh_ttl() {
        let expires_at = 10_000;
        let bundle = ProviderBundle {
            worker: "w".into(),
            instance: "w".into(),
            issued_at: 1_000,
            expires_at,
            providers: Vec::new(),
        };
        assert!(usable(&bundle, 2_000, Some(2_000)));
        assert!(!usable(&bundle, 2_000, Some(500)));
        assert!(!usable(&bundle, expires_at, None));
    }
}
