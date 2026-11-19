//! Provider vault behavior tests: encryption at rest, pairing, assignment, and
//! envelope addressing.

use super::*;
use common::{
    CreatePairCodeRequest, CreateProviderRequest, SetProviderCredentialRequest,
    UpdateProviderRequest,
};
use provider_crypto::{generate_private_key, open_envelope, public_key};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

const FIXTURE_KEY: &str = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";
const OTHER_KEY: &str = "ZmVkY2JhOTg3NjU0MzIxMGZlZGNiYTk4NzY1NDMyMTA=";

fn master() -> MasterKey {
    MasterKey::parse(FIXTURE_KEY).expect("fixture master key")
}

fn clock(start: i64) -> (Clock, Arc<AtomicI64>) {
    let now = Arc::new(AtomicI64::new(start));
    let reader = now.clone();
    (Arc::new(move || reader.load(Ordering::SeqCst)), now)
}

fn vault(now: &Arc<AtomicI64>) -> (Vault, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("vault directory");
    let reader = now.clone();
    let clock: Clock = Arc::new(move || reader.load(Ordering::SeqCst));
    let vault =
        Vault::open_path(directory.path().join(VAULT_FILE), master(), clock).expect("vault opens");
    (vault, directory)
}

fn provider(id: &str, workers: &[&str]) -> CreateProviderRequest {
    CreateProviderRequest {
        id: id.into(),
        adapter: "openrouter".into(),
        model: "openrouter/auto".into(),
        endpoint: None,
        modalities: vec!["text".into()],
        tool_calling: true,
        context_window: Some(128_000),
        local: false,
        residency: "remote".into(),
        allowed_workers: workers.iter().map(|worker| (*worker).to_string()).collect(),
        allowed_models: None,
        allow_auto_default: false,
    }
}

fn discover(vault: &Vault, id: &str, models: &[&str]) {
    vault
        .record_discovery(
            id,
            models.iter().map(|model| (*model).to_string()).collect(),
            None,
        )
        .expect("discovery records");
}

fn enroll(vault: &Vault, worker: &str, instance: &str, code: &str, public: [u8; 32]) {
    vault
        .enroll(&WorkerEnrollRequest {
            code: code.into(),
            worker_id: worker.into(),
            instance: instance.into(),
            public_key: encode(&public),
        })
        .expect("worker enrolls");
}

fn pair(vault: &Vault, worker: &str, instance: &str) -> String {
    vault
        .create_pair_code(CreatePairCodeRequest {
            worker_id: worker.into(),
            instance: instance.into(),
        })
        .expect("pair code")
        .code
}

#[test]
fn credential_is_encrypted_at_rest_and_metadata_is_non_secret() {
    let now = Arc::new(AtomicI64::new(1_000));
    let (vault, directory) = vault(&now);
    vault
        .create(provider("openrouter", &["*"]))
        .expect("create");
    let summary = vault
        .set_credential(
            "openrouter",
            &SetProviderCredentialRequest::new("sk-super-secret-value"),
        )
        .expect("credential");
    discover(&vault, "openrouter", &["openrouter/auto"]);
    assert_eq!(summary.version, 1);
    assert!(summary.fingerprint.starts_with("sha256:"));
    assert!(!summary.fingerprint.contains("sk-super-secret-value"));

    let bytes = std::fs::read(directory.path().join(VAULT_FILE)).expect("vault document");
    let text = String::from_utf8(bytes).expect("utf8 json");
    assert!(
        !text.contains("sk-super-secret-value"),
        "plaintext credential must never reach the vault file"
    );
    assert!(!text.contains("Authorization"));

    let metadata = vault.provider("openrouter").expect("metadata");
    assert_eq!(metadata.credential_version, Some(1));
    assert!(!metadata.credential_revoked);
    let json = serde_json::to_string(&vault.list()).expect("json");
    assert!(!json.contains("sk-super-secret-value"));
    assert!(!json.contains("ciphertext"));
}

#[test]
fn master_key_opens_only_the_original_document() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join(VAULT_FILE);
    let clock = clock(1_000).0;
    Vault::open_path(path.clone(), master(), clock.clone()).expect("first open");
    let wrong = MasterKey::parse(OTHER_KEY).expect("other key");
    let error = match Vault::open_path(path, wrong, clock) {
        Ok(_) => panic!("wrong master key must fail"),
        Err(error) => error,
    };
    assert_eq!(error.code, "vault.master_key_mismatch");
}

#[test]
fn pair_code_is_single_use_expires_and_binds_instance() {
    let now = Arc::new(AtomicI64::new(1_000));
    let (vault, _directory) = vault(&now);
    vault
        .create(provider("openrouter", &["*"]))
        .expect("create");

    let code = pair(&vault, "worker-a", "worker-a");
    let worker = generate_private_key();
    enroll(&vault, "worker-a", "worker-a", &code, public_key(&worker));
    let replay = vault.enroll(&WorkerEnrollRequest {
        code: code.clone(),
        worker_id: "worker-a".into(),
        instance: "worker-a".into(),
        public_key: encode(&public_key(&worker)),
    });
    assert_eq!(
        replay.expect_err("single use").code,
        "provider.pair_code_invalid"
    );

    let expired = pair(&vault, "worker-b", "worker-b");
    now.store(1_000 + PAIR_CODE_TTL_MS + 1, Ordering::SeqCst);
    let late = vault.enroll(&WorkerEnrollRequest {
        code: expired,
        worker_id: "worker-b".into(),
        instance: "worker-b".into(),
        public_key: encode(&public_key(&worker)),
    });
    assert_eq!(
        late.expect_err("expired").code,
        "provider.pair_code_invalid"
    );

    let wrong = pair(&vault, "worker-c", "worker-c");
    let mismatch = vault.enroll(&WorkerEnrollRequest {
        code: wrong,
        worker_id: "worker-c".into(),
        instance: "another-instance".into(),
        public_key: encode(&public_key(&worker)),
    });
    assert_eq!(
        mismatch.expect_err("wrong instance").code,
        "provider.pair_code_invalid"
    );
}

#[test]
fn worker_receives_only_assigned_providers_and_only_its_key_decrypts() {
    let now = Arc::new(AtomicI64::new(1_000));
    let (vault, _directory) = vault(&now);
    vault
        .create(provider("assigned", &["worker-a"]))
        .expect("create assigned");
    vault
        .create(provider("other", &["worker-b"]))
        .expect("create other");
    vault
        .set_credential(
            "assigned",
            &SetProviderCredentialRequest::new("sk-assigned"),
        )
        .expect("assigned key");
    vault
        .set_credential("other", &SetProviderCredentialRequest::new("sk-other"))
        .expect("other key");
    discover(&vault, "assigned", &["openrouter/auto"]);
    discover(&vault, "other", &["openrouter/auto"]);

    let worker = generate_private_key();
    let code = pair(&vault, "worker-a", "worker-a");
    enroll(&vault, "worker-a", "worker-a", &code, public_key(&worker));

    let bundle = vault
        .bundle("worker-a", "worker-a", PAIR_CODE_TTL_MS)
        .expect("bundle");
    assert_eq!(bundle.providers.len(), 1);
    let assignment = &bundle.providers[0];
    assert_eq!(assignment.metadata.id, "assigned");
    assert!(bundle.expires_at <= now.load(Ordering::SeqCst) + PROVIDER_ENVELOPE_TTL_MS);

    let envelope = &assignment.envelope;
    let aad = envelope_aad(
        "assigned",
        envelope.version,
        "worker-a",
        "worker-a",
        envelope.expires_at,
    );
    let plaintext = open_envelope(
        &worker,
        &decode(&envelope.ephemeral_public_key)
            .expect("ephemeral")
            .try_into()
            .expect("32 bytes"),
        &decode(&envelope.nonce).expect("nonce"),
        &decode(&envelope.ciphertext).expect("ciphertext"),
        &aad,
    )
    .expect("worker decrypts");
    assert_eq!(plaintext.as_slice(), b"sk-assigned");

    let wrong = generate_private_key();
    let denied = open_envelope(
        &wrong,
        &decode(&envelope.ephemeral_public_key)
            .expect("ephemeral")
            .try_into()
            .expect("32 bytes"),
        &decode(&envelope.nonce).expect("nonce"),
        &decode(&envelope.ciphertext).expect("ciphertext"),
        &aad,
    );
    assert!(denied.is_err(), "another Worker key must not decrypt");
}

#[test]
fn revoked_or_inactive_credentials_are_not_enveloped() {
    let now = Arc::new(AtomicI64::new(1_000));
    let (vault, _directory) = vault(&now);
    vault.create(provider("p", &["*"])).expect("create");
    vault
        .set_credential("p", &SetProviderCredentialRequest::new("sk-value"))
        .expect("key");
    discover(&vault, "p", &["openrouter/auto"]);
    let worker = generate_private_key();
    let code = pair(&vault, "worker-a", "worker-a");
    enroll(&vault, "worker-a", "worker-a", &code, public_key(&worker));

    let revoked = vault.revoke_credential("p").expect("revoke");
    assert!(revoked.revoked_at.is_some());
    let bundle = vault
        .bundle("worker-a", "worker-a", 60_000)
        .expect("bundle");
    assert!(bundle.providers.is_empty());

    let metadata = vault.provider("p").expect("metadata");
    assert!(metadata.credential_revoked);
    vault
        .set_credential("p", &SetProviderCredentialRequest::new("sk-new"))
        .expect("rotate");
    discover(&vault, "p", &["openrouter/auto"]);
    let rotated = vault.provider("p").expect("metadata");
    assert!(!rotated.credential_revoked);
    assert_eq!(rotated.credential_version, Some(2));
    let bundle = vault
        .bundle("worker-a", "worker-a", 60_000)
        .expect("bundle");
    assert_eq!(bundle.providers.len(), 1);
}

#[test]
fn discovery_selects_only_filtered_models_and_requires_a_default() {
    let now = Arc::new(AtomicI64::new(1_000));
    let (vault, _directory) = vault(&now);
    let mut request = provider("p", &["*"]);
    request.model.clear();
    request.allowed_models = Some(vec!["alpha".into()]);
    request.allow_auto_default = true;
    vault.create(request).expect("create");
    vault
        .set_credential("p", &SetProviderCredentialRequest::new("sk-value"))
        .expect("credential");
    let metadata = vault
        .record_discovery(
            "p",
            vec![
                "gamma".into(),
                "beta".into(),
                "alpha".into(),
                "alpha".into(),
            ],
            None,
        )
        .expect("discovery");
    assert!(metadata.ready);
    assert_eq!(metadata.model, "alpha");
    assert_eq!(metadata.models, vec!["alpha".to_string()]);

    let mut missing = provider("q", &["*"]);
    missing.model = "not-discovered".into();
    vault.create(missing).expect("create");
    vault
        .set_credential("q", &SetProviderCredentialRequest::new("sk-other"))
        .expect("credential");
    let metadata = vault
        .record_discovery("q", vec!["alpha".into()], None)
        .expect("discovery");
    assert!(!metadata.ready);
    assert_eq!(
        metadata.discovery_error_code.as_deref(),
        Some("provider.default_model_unavailable")
    );

    let failure = vault
        .record_discovery(
            "q",
            Vec::new(),
            Some((
                "provider.discovery_auth".into(),
                "provider discovery was rejected".into(),
            )),
        )
        .expect("failure records");
    assert!(!failure.ready);
    assert!(failure.models.is_empty());
    assert_eq!(
        failure.discovery_error_code.as_deref(),
        Some("provider.discovery_auth")
    );
}

#[test]
fn update_requires_the_observed_revision_and_can_disable() {
    let now = Arc::new(AtomicI64::new(1_000));
    let (vault, _directory) = vault(&now);
    let created = vault.create(provider("p", &["*"])).expect("create");
    let error = vault
        .update(
            "p",
            UpdateProviderRequest {
                revision: created.revision + 1,
                adapter: None,
                model: None,
                endpoint: None,
                modalities: None,
                tool_calling: None,
                context_window: None,
                local: None,
                residency: None,
                allowed_workers: None,
                allowed_models: None,
                allow_auto_default: None,
                active: Some(false),
            },
        )
        .expect_err("stale revision");
    assert_eq!(error.code, "provider.revision_conflict");
    let updated = vault
        .update(
            "p",
            UpdateProviderRequest {
                revision: created.revision,
                adapter: None,
                model: None,
                endpoint: None,
                modalities: None,
                tool_calling: None,
                context_window: None,
                local: None,
                residency: None,
                allowed_workers: None,
                allowed_models: None,
                allow_auto_default: None,
                active: Some(false),
            },
        )
        .expect("update");
    assert!(!updated.active);
    assert_eq!(updated.revision, created.revision + 1);
}

#[test]
fn credential_debug_and_pair_response_debug_redact_secrets() {
    let request = SetProviderCredentialRequest::new("sk-hidden");
    assert!(!format!("{request:?}").contains("sk-hidden"));
}
