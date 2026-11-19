//! Public Worker backup, restore, and migration drills.
//!
//! These tests build real signed General releases, run the real Worker, stop it,
//! take a cold portable backup, and restore it onto a new instances root. They
//! also corrupt a backup, revoke a required credential, interrupt a migration,
//! and prove a newer schema is not rewritten.

use base64::Engine;
use common::{bytes_digest, ReleaseMetadata};
use ed25519_dalek::{Signer, SigningKey};
use install::{
    encode_metadata, seed_assets, write_layout_with_seeds, InstallOutcome, Installer, SeedFile,
    TrustPolicy,
};
use instance::{BackupRequest, CreateRequest, InstanceLimits, MigrateRequest, RestoreRequest};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[23_u8; 32])
}

fn public_key() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(signing_key().verifying_key().as_bytes())
}

fn policy(manifest: &common::PackageManifest) -> TrustPolicy {
    let mut permissions = manifest.required_permissions.clone();
    for capability in &manifest.capabilities {
        permissions.extend(capability.permissions.clone());
    }
    permissions.sort();
    permissions.dedup();
    TrustPolicy {
        trusted_signers: BTreeMap::from([("general-publisher".into(), public_key())]),
        targets: vec![TrustPolicy::host_target()],
        runtimes: vec![manifest.runtime.clone()],
        adks: vec![manifest.adk.clone()],
        protocols: vec![manifest.protocol.clone()],
        features: manifest.features.clone(),
        approved_permissions: permissions,
        sensitive_permissions: vec!["credential:read".into(), "exec:host".into()],
        ..TrustPolicy::default()
    }
}

fn seeds(prompt: &str) -> Vec<SeedFile> {
    vec![
        SeedFile::new(
            "assets/agents/general.toml",
            "name = \"general\"\ndescription = \"seeded general agent\"\nmodel = \"deterministic\"\nprompt = \"general\"\ntools = [\"exec_command\", \"save_artifact\", \"remember_memory\"]\nskills = [\"tone\"]\n",
        ),
        SeedFile::new("assets/prompts/general.md", prompt.to_string()),
        SeedFile::new(
            "assets/skills/tone/SKILL.md",
            "---\nname: tone\ndescription: Keep replies concise and factual.\n---\nPrefer short, direct answers.",
        ),
    ]
}

fn release(cache: &Path, prompt: &str, revision: &str) -> PathBuf {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_general")).expect("general binary");
    let source = serde_json::to_vec(&serde_json::json!({
        "revision": revision,
        "tree": bytes_digest(format!("general source tree {revision}").as_bytes()),
    }))
    .unwrap();
    let seeds = seeds(prompt);
    let package = general::manifest();
    let metadata = ReleaseMetadata {
        schema: common::RELEASE_METADATA_SCHEMA,
        package,
        source_digest: bytes_digest(&source),
        binary_digest: bytes_digest(&binary),
        source_revision: revision.into(),
        target: TrustPolicy::host_target(),
        protocol_min: common::PROTOCOL_VERSION.into(),
        protocol_max: common::PROTOCOL_VERSION.into(),
        signer: "general-publisher".into(),
        build: common::BuildEvidence::default(),
        seed_assets: seed_assets(&seeds),
    };
    let digest = bytes_digest(&encode_metadata(&metadata).unwrap());
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(signing_key().sign(digest.as_bytes()).to_bytes());
    let artifact = tempfile::tempdir().expect("artifact");
    write_layout_with_seeds(
        artifact.path(),
        &metadata,
        &binary,
        &source,
        &seeds,
        &signature,
    )
    .expect("layout");
    let installer = Installer::new(cache, policy(&metadata.package));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("install runtime");
    match runtime
        .block_on(installer.install(artifact.path()))
        .expect("verified install")
    {
        InstallOutcome::Installed(release) | InstallOutcome::Converged(release) => {
            release.directory
        }
    }
}

fn create(
    root: &Path,
    id: &str,
    release: &Path,
    secret_refs: Vec<String>,
) -> instance::InstanceRecord {
    let mut request = CreateRequest::new(root, id, release);
    request.grants = [
        "task:submit",
        "task:read",
        "task:cancel",
        "session:write",
        "session:read",
        "profile:read",
        "profile:write",
    ]
    .iter()
    .map(|grant| (*grant).into())
    .collect();
    request.limits = InstanceLimits {
        max_active: 1,
        max_queue: 4,
        per_principal: 4,
    };
    request.secret_refs = secret_refs;
    instance::create(request).expect("instance created").record
}

fn token(root: &Path, id: &str) -> String {
    std::fs::read_to_string(root.join(id).join("token"))
        .expect("token")
        .trim()
        .to_string()
}

fn submit(base: &str, token: &str, input: &str, key: &str) -> Value {
    let response = reqwest::blocking::Client::new()
        .post(format!("{base}/v1/tasks"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "agent": "general",
            "session_id": format!("session-{key}"),
            "input": input,
            "idempotency_key": key,
        }))
        .send()
        .expect("submit responds");
    assert!(response.status().is_success(), "{}", response.status());
    let submitted: Value = response.json().expect("submit body");
    let task_id = submitted["task_id"].as_str().expect("task id");
    for _ in 0..1000 {
        let task: Value = reqwest::blocking::Client::new()
            .get(format!("{base}/v1/tasks/{task_id}"))
            .bearer_auth(token)
            .send()
            .expect("task responds")
            .json()
            .expect("task body");
        if matches!(
            task["state"].as_str(),
            Some("succeeded" | "failed" | "cancelled" | "unknown")
        ) {
            return task;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("task did not finish");
}

fn run_task(root: &Path, id: &str, input: &str, key: &str) -> (String, Value) {
    let started = instance::start(root, id, None).expect("instance starts");
    let base = format!("http://{}", started.addr);
    let token = token(root, id);
    let task = submit(&base, &token, input, key);
    let stopped = instance::stop(root, id).expect("instance stops");
    assert_eq!(stopped.state.lifecycle, "stopped");
    (base, task)
}

fn backup(root: &Path, id: &str, output: &Path) -> instance::BackupOutcome {
    instance::create_backup(&BackupRequest {
        root: root.to_path_buf(),
        instance: id.into(),
        output: output.to_path_buf(),
    })
    .expect("backup created")
}

fn restore(
    backup: &Path,
    root: &Path,
    release: &Path,
    secret_refs: &[String],
) -> instance::RestoreReport {
    let mut request = RestoreRequest::new(backup, root);
    request.release = Some(release.to_path_buf());
    request.available_secrets = secret_refs.to_vec();
    instance::restore_backup(&request).expect("restore runs")
}

fn list_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            } else {
                files.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap()
                        .display()
                        .to_string(),
                );
            }
        }
    }
    files.sort();
    files
}

fn write_synthetic_backup(root: &Path, manifest: &common::BackupManifest, schema: u32) {
    std::fs::create_dir_all(root.join("instance/state")).unwrap();
    let files = [
        (
            "instance/worker.toml",
            b"instance = \"worker-a\"\n".to_vec(),
        ),
        (
            "instance/state/state.json",
            format!("{{\"schema\":{schema},\"tasks\":{{}},\"profiles\":{{}}}}").into_bytes(),
        ),
    ];
    let mut manifest = manifest.clone();
    manifest.files = files
        .iter()
        .map(|(path, bytes)| common::BackupFile {
            path: (*path).into(),
            role: "fixture".into(),
            digest: bytes_digest(bytes),
            size: bytes.len() as u64,
        })
        .collect();
    for (path, bytes) in files {
        std::fs::write(root.join(path), bytes).unwrap();
    }
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn workflow() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let release = release(cache.path(), "PROMPT-BACKUP", "git:backup");
    (cache, root, release)
}

#[test]
fn backup_restore_preserves_identity_history_profile_and_launcher() {
    let (_cache, root, release) = workflow();
    let record = create(root.path(), "worker-a", &release, Vec::new());
    let (_base, task) = run_task(root.path(), "worker-a", "prompt:", "before-backup");
    assert_eq!(task["state"], "succeeded");
    let profile = instance::profile(
        root.path(),
        "worker-a",
        &[
            "create".into(),
            "research".into(),
            "--instruction".into(),
            "PROFILE-BACKUP".into(),
            "--member".into(),
            "general".into(),
            "--capability".into(),
            "general".into(),
            "--grant".into(),
            "task:submit".into(),
            "--activate".into(),
        ],
    )
    .expect("profile creates");
    assert_eq!(profile.status, 0, "{}", profile.stderr);

    // SQLite WAL companions are archived with the main database as one set.
    let data = root.path().join("worker-a/state");
    std::fs::write(
        data.join("backup-fixture.sqlite3"),
        b"SQLite format 3\0fixture",
    )
    .unwrap();
    std::fs::write(data.join("backup-fixture.sqlite3-wal"), b"wal-fixture").unwrap();
    std::fs::write(data.join("backup-fixture.sqlite3-shm"), b"shm-fixture").unwrap();

    let backups = tempfile::tempdir().expect("backups");
    let outcome = backup(root.path(), "worker-a", &backups.path().join("worker-a"));
    let verification = instance::verify_backup(&outcome.path).expect("backup verifies");
    assert!(verification.ready, "{:?}", verification.checks);
    let worker = verification.manifest.worker.expect("worker section");
    assert_eq!(worker.instance, "worker-a");
    assert_eq!(worker.release.artifact_digest, record.artifact_digest);
    assert!(worker.data_schema > 0);
    assert!(!worker.tasks.is_empty());
    assert!(worker.profiles.iter().any(|item| item.name == "research"));
    assert!(verification
        .manifest
        .files
        .iter()
        .any(|file| file.path.ends_with("backup-fixture.sqlite3-wal")));
    let manifest_text =
        std::fs::read_to_string(outcome.path.join(instance::MANIFEST_FILE)).unwrap();
    assert!(!manifest_text.contains(&token(root.path(), "worker-a")));

    let clean = tempfile::tempdir().expect("clean host");
    let report = restore(&outcome.path, clean.path(), &release, &[]);
    assert_eq!(report.status, "ready", "{:?}", report.checks);
    assert!(report.ready, "{:?}", report.checks);
    assert_eq!(report.launcher.status, "ok", "{:?}", report.launcher);
    assert_eq!(report.gateway.status, "skipped");

    let restored_record: instance::InstanceRecord = serde_json::from_slice(
        &std::fs::read(clean.path().join("worker-a/instance.json")).expect("restored record"),
    )
    .expect("record JSON");
    assert_eq!(restored_record.id, "worker-a");
    assert_eq!(restored_record.package, record.package);
    assert_eq!(restored_record.artifact_digest, record.artifact_digest);
    assert_eq!(restored_record.grants, record.grants);
    assert_eq!(restored_record.secret_refs, record.secret_refs);
    assert_eq!(
        restored_record.config_path,
        clean.path().join("worker-a/worker.toml")
    );
    let lifecycle: Value = serde_json::from_slice(
        &std::fs::read(clean.path().join("worker-a/state.json")).expect("lifecycle"),
    )
    .expect("lifecycle JSON");
    assert_eq!(lifecycle["lifecycle"], "stopped");
    let state: Value = serde_json::from_slice(
        &std::fs::read(clean.path().join("worker-a/state/state.json")).expect("state"),
    )
    .expect("state JSON");
    assert!(state["tasks"]
        .as_object()
        .is_some_and(|tasks| !tasks.is_empty()));
    assert!(state["profiles"]
        .as_object()
        .is_some_and(|profiles| profiles
            .values()
            .any(|profile| profile["name"] == "research")));
    assert!(clean
        .path()
        .join("worker-a/state/backup-fixture.sqlite3-wal")
        .is_file());
    assert!(clean
        .path()
        .join("worker-a/state/backup-fixture.sqlite3-shm")
        .is_file());

    // The restored deployment starts from its native Launcher-compatible
    // composition and still exposes its history through the Worker API.
    let started = match instance::start(clean.path(), "worker-a", None) {
        Ok(started) => started,
        Err(error) => panic!(
            "restored starts: {error}\n{}",
            std::fs::read_to_string(clean.path().join("worker-a/process.log")).unwrap_or_default()
        ),
    };
    let status: Value = reqwest::blocking::Client::new()
        .get(format!("http://{}/v1/status", started.addr))
        .bearer_auth(token(clean.path(), "worker-a"))
        .send()
        .expect("status responds")
        .json()
        .expect("status JSON");
    assert_eq!(status["phase"], "ready");
    instance::stop(clean.path(), "worker-a").expect("restored stops");
    assert!(root.path().join("worker-a/instance.json").is_file());
}

#[test]
fn backup_of_an_unstarted_instance_restores_an_empty_ready_instance() {
    let (_cache, root, release) = workflow();
    create(root.path(), "worker-fresh", &release, Vec::new());
    let backups = tempfile::tempdir().expect("backups");
    let outcome = backup(
        root.path(),
        "worker-fresh",
        &backups.path().join("worker-fresh"),
    );
    let verification = instance::verify_backup(&outcome.path).expect("verify");
    assert!(verification.ready, "{:?}", verification.checks);
    assert_eq!(verification.manifest.worker.expect("worker").data_schema, 0);

    let clean = tempfile::tempdir().expect("clean host");
    let report = restore(&outcome.path, clean.path(), &release, &[]);
    assert!(report.ready, "{:?}", report.checks);
    let lifecycle: Value = serde_json::from_slice(
        &std::fs::read(clean.path().join("worker-fresh/state.json")).expect("lifecycle"),
    )
    .expect("lifecycle JSON");
    assert_eq!(lifecycle["lifecycle"], "stopped");
    instance::start(clean.path(), "worker-fresh", None).expect("restored starts");
    instance::stop(clean.path(), "worker-fresh").expect("restored stops");
}

#[test]
fn corrupted_or_incomplete_backup_is_rejected_without_writing_a_target() {
    let (_cache, root, release) = workflow();
    create(root.path(), "worker-a", &release, Vec::new());
    run_task(root.path(), "worker-a", "prompt:", "history");
    let backups = tempfile::tempdir().expect("backups");
    let outcome = backup(root.path(), "worker-a", &backups.path().join("worker-a"));

    let archived = outcome.path.join("instance/worker.toml");
    let mut bytes = std::fs::read(&archived).expect("config");
    bytes.extend_from_slice(b"\n# corruption");
    std::fs::write(&archived, bytes).unwrap();
    let verification = instance::verify_backup(&outcome.path).expect("manifest parses");
    assert!(!verification.ready);
    assert!(verification
        .checks
        .iter()
        .any(|check| check.status == "blocked" && check.name == "content"));

    let clean = tempfile::tempdir().expect("clean host");
    let report = restore(&outcome.path, clean.path(), &release, &[]);
    assert_eq!(report.status, "blocked");
    assert!(!clean.path().join("worker-a").exists());
}

#[test]
fn revoked_or_missing_secret_references_block_readiness() {
    let (_cache, root, release) = workflow();
    let reference = "env:GOLDENBEE_BACKUP_MODEL_KEY".to_string();
    create(root.path(), "worker-a", &release, vec![reference.clone()]);
    run_task(root.path(), "worker-a", "prompt:", "secret-history");
    let backups = tempfile::tempdir().expect("backups");
    let outcome = backup(root.path(), "worker-a", &backups.path().join("worker-a"));
    assert!(
        outcome.path.join("instance/state/state.json").is_file(),
        "archive tree: {:?}",
        list_files(&outcome.path)
    );

    let clean = tempfile::tempdir().expect("clean host");
    let mut request = RestoreRequest::new(&outcome.path, clean.path());
    request.release = Some(release.clone());
    request.revoked_secrets = vec![reference.clone()];
    let blocked = instance::restore_backup(&request).expect("restore runs");
    assert!(!blocked.ready);
    assert_eq!(blocked.status, "blocked");
    assert!(blocked
        .checks
        .iter()
        .any(|check| check.name == "secret-reference" && check.status == "blocked"));
    assert!(!clean.path().join("worker-a").exists());

    let mut request = RestoreRequest::new(&outcome.path, clean.path());
    request.release = Some(release);
    request.available_secrets = vec![reference];
    let ready = instance::restore_backup(&request).expect("restore runs");
    assert!(ready.ready, "{:?}", ready.checks);
}

#[test]
fn external_configuration_and_assets_require_explicit_path_mapping() {
    let (_cache, root, release) = workflow();
    let external = tempfile::tempdir().expect("external");
    let prompts = external.path().join("prompts");
    std::fs::create_dir_all(&prompts).expect("prompts");
    std::fs::write(prompts.join("general.md"), "EXTERNAL-PROMPT").expect("prompt");
    let config_dir = env!("CARGO_TARGET_TMPDIR");
    let config_path =
        Path::new(config_dir).join(format!("backup-external-{}.toml", std::process::id()));
    let data_dir = root.path().join("worker-ext/state");
    let token = "external-token-canary";
    std::fs::write(
        &config_path,
        format!(
            "instance = \"worker-ext\"\nnamespace = \"local\"\ndefault_agent = \"manual\"\ntoken = \"{token}\"\ntoken_scopes = [\"*\"]\ngrants = [\"task:submit\", \"task:read\", \"session:write\", \"profile:read\", \"profile:write\"]\n\n[paths]\nprompts = \"{}\"\n\n[storage]\ndirectory = \"{}\"\n",
            prompts.display(),
            data_dir.display()
        ),
    )
    .expect("external config");
    let mut request = CreateRequest::new(root.path(), "worker-ext", &release);
    request.grants = [
        "task:submit",
        "task:read",
        "session:write",
        "profile:read",
        "profile:write",
    ]
    .iter()
    .map(|grant| (*grant).into())
    .collect();
    request.config = Some(config_path.clone());
    request.default_agent = Some("manual".into());
    instance::create(request).expect("instance created");
    let started = instance::start(root.path(), "worker-ext", None).expect("instance starts");
    let _ = started;
    instance::stop(root.path(), "worker-ext").expect("instance stops");

    let backups = tempfile::tempdir().expect("backups");
    let outcome = backup(
        root.path(),
        "worker-ext",
        &backups.path().join("worker-ext"),
    );
    let verification = instance::verify_backup(&outcome.path).expect("verify");
    assert!(verification.ready, "{:?}", verification.checks);
    let worker = verification.manifest.worker.expect("worker");
    assert!(worker.external_assets.iter().any(
        |asset| asset.kind == "prompts" && asset.original_path == prompts.display().to_string()
    ));

    // Simulate a clean host: the source configuration and external assets are
    // gone, so an implicit fallback would silently change what is restored.
    std::fs::remove_file(&config_path).expect("remove source config");
    std::fs::remove_dir_all(&prompts).expect("remove source prompts");

    let clean = tempfile::tempdir().expect("clean host");
    let mut restore_request = RestoreRequest::new(&outcome.path, clean.path());
    restore_request.release = Some(release.clone());
    let blocked = instance::restore_backup(&restore_request).expect("restore");
    assert!(!blocked.ready);
    assert!(blocked
        .checks
        .iter()
        .any(|check| check.name == "external-path" && check.status == "blocked"));
    assert!(!clean.path().join("worker-ext").exists());

    let new_prompts = clean.path().join("assets/prompts");
    let new_config = clean.path().join("config/worker.toml");
    let mut restore_request = RestoreRequest::new(&outcome.path, clean.path());
    restore_request.release = Some(release);
    restore_request.config = Some(new_config.clone());
    restore_request
        .path_map
        .insert(prompts.display().to_string(), new_prompts.clone());
    let ready = instance::restore_backup(&restore_request).expect("restore");
    assert!(ready.ready, "{:?}", ready.checks);
    assert!(new_config.is_file());
    assert_eq!(
        std::fs::read_to_string(new_prompts.join("general.md")).expect("restored prompt"),
        "EXTERNAL-PROMPT"
    );
    let record: instance::InstanceRecord = serde_json::from_slice(
        &std::fs::read(clean.path().join("worker-ext/instance.json")).unwrap(),
    )
    .expect("record");
    assert_eq!(record.config_path, new_config);
    instance::start(clean.path(), "worker-ext", None).expect("restored starts");
    instance::stop(clean.path(), "worker-ext").expect("restored stops");
}

#[test]
fn interrupted_migration_resumes_and_newer_state_is_not_mutated() {
    let source = tempfile::tempdir().expect("source");
    let manifest = common::BackupManifest {
        format: common::BACKUP_FORMAT.into(),
        schema: common::BACKUP_SCHEMA,
        id: "worker-synthetic".into(),
        created_at: common::now_ms(),
        source_host: "host-a".into(),
        files: Vec::new(),
        worker: Some(common::WorkerBackup {
            instance: "worker-a".into(),
            source_instance: "/source/worker-a".into(),
            release: common::BackupRelease {
                package: "general".into(),
                version: "1.0.0".into(),
                artifact_digest: "sha256:fixture".into(),
                binary_digest: "sha256:binary".into(),
                ..common::BackupRelease::default()
            },
            data_schema: 4,
            config_path: "/source/worker-a/worker.toml".into(),
            config_archived: "instance/worker.toml".into(),
            data_dir: "/source/worker-a/state".into(),
            data_archived: "instance/state".into(),
            workspace_root: "/source/worker-a/workspace".into(),
            workspace_write: true,
            ..common::WorkerBackup::default()
        }),
        gateway: None,
    };
    write_synthetic_backup(source.path(), &manifest, 4);

    let interrupted = tempfile::tempdir().expect("migration");
    let target = interrupted.path().join("copy");
    std::env::set_var("GOLDENBEE_BACKUP_INTERRUPT", "2");
    let error = instance::migrate_backup(&MigrateRequest::new(source.path(), &target))
        .expect_err("interrupted");
    std::env::remove_var("GOLDENBEE_BACKUP_INTERRUPT");
    assert_eq!(error.code, "migration.interrupted");
    let journal: instance::MigrationJournal = serde_json::from_slice(
        &std::fs::read(target.join(instance::MIGRATION_JOURNAL)).expect("journal"),
    )
    .expect("journal JSON");
    assert_eq!(journal.status, "interrupted");
    assert_eq!(journal.completed.len(), 2);

    let resumed =
        instance::migrate_backup(&MigrateRequest::new(source.path(), &target)).expect("resumes");
    assert!(resumed.resumed);
    assert_eq!(resumed.journal.status, "complete");
    assert_eq!(
        resumed.journal.completed,
        vec![
            "verify-source",
            "copy-present",
            "schema-compatible",
            "normalize-manifest",
            "verify-copy"
        ]
    );
    assert!(resumed.verification.ready);

    let newer = tempfile::tempdir().expect("newer");
    let mut manifest = manifest;
    manifest.worker.as_mut().unwrap().data_schema = 99;
    write_synthetic_backup(newer.path(), &manifest, 99);
    let target = interrupted.path().join("newer-copy");
    let before = std::fs::read(newer.path().join("manifest.json")).unwrap();
    let error = instance::migrate_backup(&MigrateRequest::new(newer.path(), &target))
        .expect_err("irreversible");
    assert_eq!(error.code, "migration.irreversible");
    assert_eq!(
        std::fs::read(newer.path().join("manifest.json")).unwrap(),
        before
    );
    let journal: instance::MigrationJournal = serde_json::from_slice(
        &std::fs::read(target.join(instance::MIGRATION_JOURNAL)).expect("journal"),
    )
    .expect("journal JSON");
    assert_eq!(journal.status, "irreversible");
}
