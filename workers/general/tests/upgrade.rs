//! Public upgrade and rollback behavior for one verified Worker Package.
//!
//! These tests drive the real installed General binary through the host
//! instance tooling. They cover dry-run differences, explicit permission
//! authorization, pinned and drained work, successful activation, failed
//! readiness/capability verification with binary+data restoration, interrupted
//! recovery, external configuration preservation, and package isolation.

use base64::Engine;
use common::{bytes_digest, ReleaseMetadata};
use ed25519_dalek::{Signer, SigningKey};
use install::{
    encode_metadata, seed_assets, write_layout_with_seeds, InstallOutcome, Installer, SeedFile,
    TrustPolicy,
};
use instance::{
    CreateRequest, InstanceLimits, InstanceRecord, InstanceState, UpgradeJournal, UpgradeRequest,
    WorkPolicy,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[11_u8; 32])
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

fn seed_assets_for(prompt: &str) -> Vec<SeedFile> {
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

/// Install the real General binary as one signed release with seed defaults.
fn release(cache: &Path, prompt: &str, revision: &str, extra_permissions: &[&str]) -> PathBuf {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_general")).expect("general binary");
    let source = serde_json::to_vec(&serde_json::json!({
        "revision": revision,
        "tree": bytes_digest(format!("general source tree {revision}").as_bytes()),
    }))
    .unwrap();
    let seeds = seed_assets_for(prompt);
    let mut package = general::manifest();
    for permission in extra_permissions {
        if !package
            .required_permissions
            .iter()
            .any(|value| value == permission)
        {
            package.required_permissions.push((*permission).into());
        }
    }
    package.required_permissions.sort();
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
        build: common::BuildEvidence {
            toolchain: "rust 1.83".into(),
            profile: "release".into(),
            command: "gbb check -p general".into(),
            started_at: "2026-11-18T00:00:00Z".into(),
            finished_at: "2026-11-18T00:01:00Z".into(),
            reproducible: true,
            evidence_digest: bytes_digest(format!("general build evidence {revision}").as_bytes()),
        },
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
    let outcome = runtime
        .block_on(installer.install(artifact.path()))
        .expect("verified install");
    match outcome {
        InstallOutcome::Installed(release) | InstallOutcome::Converged(release) => {
            release.directory
        }
    }
}

struct Running {
    root: PathBuf,
    id: String,
    addr: String,
    token: String,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = instance::stop(&self.root, &self.id);
    }
}

fn attach(root: &Path, id: &str, addr: String) -> Running {
    let token = std::fs::read_to_string(root.join(id).join("token"))
        .expect("token")
        .trim()
        .to_string();
    Running {
        root: root.to_path_buf(),
        id: id.to_string(),
        addr,
        token,
    }
}

fn start(root: &Path, id: &str) -> Running {
    let started = instance::start(root, id, None).expect("instance starts");
    attach(root, id, started.addr)
}

impl Running {
    fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn submit(&self, input: &str, key: &str) -> (u16, Value) {
        let response = reqwest::blocking::Client::new()
            .post(format!("{}/v1/tasks", self.base()))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "agent": "general",
                "session_id": format!("session-{key}"),
                "input": input,
                "idempotency_key": key,
            }))
            .send()
            .expect("submit responds");
        let status = response.status().as_u16();
        let body: Value = response.json().unwrap_or(Value::Null);
        (status, body)
    }

    fn run(&self, input: &str, key: &str) -> Value {
        let (status, submitted) = self.submit(input, key);
        assert!(status == 200 || status == 201, "{status} {submitted}");
        let task_id = submitted["task_id"].as_str().expect("task id");
        for _ in 0..1000 {
            let task: Value = reqwest::blocking::Client::new()
                .get(format!("{}/v1/tasks/{task_id}", self.base()))
                .bearer_auth(&self.token)
                .send()
                .expect("task responds")
                .json()
                .expect("task JSON");
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

    fn task(&self, task_id: &str) -> (u16, Value) {
        let response = reqwest::blocking::Client::new()
            .get(format!("{}/v1/tasks/{task_id}", self.base()))
            .bearer_auth(&self.token)
            .send()
            .expect("task responds");
        let status = response.status().as_u16();
        (status, response.json().unwrap_or(Value::Null))
    }

    fn cancel(&self, task_id: &str) -> Value {
        reqwest::blocking::Client::new()
            .post(format!("{}/v1/tasks/{task_id}/cancel", self.base()))
            .bearer_auth(&self.token)
            .send()
            .expect("cancel responds")
            .json()
            .expect("cancel JSON")
    }

    fn status(&self) -> Value {
        reqwest::blocking::Client::new()
            .get(format!("{}/v1/status", self.base()))
            .bearer_auth(&self.token)
            .send()
            .expect("status responds")
            .json()
            .expect("status JSON")
    }
}

fn output(task: &Value) -> String {
    task["output"].as_str().unwrap_or_default().to_string()
}

fn create(root: &Path, id: &str, release: &Path, config: Option<PathBuf>) -> InstanceRecord {
    let mut request = CreateRequest::new(root, id, release);
    request.grants = [
        "task:submit",
        "task:read",
        "session:write",
        "profile:read",
        "profile:write",
        "task:cancel",
    ]
    .iter()
    .map(|grant| (*grant).into())
    .collect();
    request.limits = InstanceLimits {
        max_active: 1,
        max_queue: 4,
        per_principal: 4,
    };
    request.secret_refs = vec![format!("secret://{id}/model-key")];
    request.config = config;
    instance::create(request).expect("instance created").record
}

fn request(root: &Path, id: &str, release: &Path) -> UpgradeRequest {
    UpgradeRequest::new(root, id, release)
}

fn profile(root: &Path, id: &str, name: &str, instruction: &str) {
    let result = instance::profile(
        root,
        id,
        &[
            "create".into(),
            name.into(),
            "--instruction".into(),
            instruction.into(),
            "--member".into(),
            "general".into(),
            "--capability".into(),
            "general".into(),
            "--grant".into(),
            "task:submit".into(),
            "--activate".into(),
        ],
    )
    .expect("profile proxy");
    assert_eq!(result.status, 0, "{}", result.stderr);
}

fn journal(root: &Path, id: &str) -> UpgradeJournal {
    serde_json::from_slice(
        &std::fs::read(root.join(id).join(instance::upgrade::JOURNAL_FILE)).expect("journal"),
    )
    .expect("journal JSON")
}

fn state_json(root: &Path, id: &str) -> Value {
    serde_json::from_slice(&std::fs::read(root.join(id).join("state.json")).expect("state"))
        .expect("state JSON")
}

fn addr(root: &Path, id: &str) -> String {
    state_json(root, id)["addr"]
        .as_str()
        .expect("instance address")
        .to_string()
}

#[test]
fn dry_run_reports_differences_and_permissions_gate_activation() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let first = release(cache.path(), "PROMPT-UPGRADE-A", "git:upgrade-a", &[]);
    let second = release(
        cache.path(),
        "PROMPT-UPGRADE-B",
        "git:upgrade-b",
        &["exec:command"],
    );
    let record = create(root.path(), "worker-a", &first, None);
    profile(root.path(), "worker-a", "research", "PROFILE-UPGRADE-V1");
    let running = start(root.path(), "worker-a");
    let old_task = running.run("prompt:", "before-upgrade");
    assert_eq!(old_task["state"], "succeeded");
    assert!(output(&old_task).contains("PROMPT-UPGRADE-A"));

    let config_before = std::fs::read(root.path().join("worker-a/worker.toml")).unwrap();
    let record_before = std::fs::read(root.path().join("worker-a/instance.json")).unwrap();
    let state_before = state_json(root.path(), "worker-a");

    let mut plan_request = request(root.path(), "worker-a", &second);
    plan_request.authorized_permissions = vec!["exec:command".into()];
    let report = instance::plan_upgrade(&plan_request).expect("dry run");
    assert_eq!(report.instance, "worker-a");
    assert_eq!(report.current.artifact_digest, record.artifact_digest);
    assert_eq!(
        report.candidate.artifact_digest,
        format!("sha256:{}", second.file_name().unwrap().to_string_lossy())
    );
    assert!(report.signature.verified);
    assert!(!report.platform.changed);
    assert!(!report.runtime.changed);
    assert_eq!(report.schema_change.status, "compatible");
    assert_eq!(report.permissions.added, vec!["exec:command".to_string()]);
    assert!(report.authorization.satisfied);
    assert!(
        report.config.preflight_ok,
        "{:?}",
        report.config.preflight_error
    );
    assert_eq!(report.config.path, record.config_path);
    assert!(report.launcher.verified.contains(&"dev".to_string()));
    assert!(report.launcher.verified.contains(&"serve".to_string()));
    assert!(
        report
            .profiles
            .iter()
            .any(|profile| profile.name == "research" && profile.active),
        "{:?}",
        report.profiles
    );
    assert!(report.blocked.is_empty(), "{:?}", report.blocked);

    // Dry-run and an unauthorized apply change nothing.
    assert_eq!(
        std::fs::read(root.path().join("worker-a/worker.toml")).unwrap(),
        config_before
    );
    assert_eq!(
        std::fs::read(root.path().join("worker-a/instance.json")).unwrap(),
        record_before
    );
    assert_eq!(state_json(root.path(), "worker-a"), state_before);
    let mut unauthorized = request(root.path(), "worker-a", &second);
    let denied = instance::apply_upgrade(&unauthorized).expect_err("permissions must be explicit");
    assert_eq!(denied.code, "upgrade.incompatible");
    assert!(
        denied.message.contains("exec:command"),
        "{}",
        denied.message
    );
    assert_eq!(
        instance::inspect(root.path(), "worker-a")
            .unwrap()
            .artifact_digest,
        record.artifact_digest
    );
    assert_eq!(
        instance::inspect(root.path(), "worker-a")
            .unwrap()
            .process_health,
        "running"
    );

    // Explicit authorization activates the candidate and preserves history,
    // identity, profiles, configuration location, and workspace authority.
    unauthorized.authorized_permissions = vec!["exec:command".into()];
    let outcome = instance::apply_upgrade(&unauthorized).expect("authorized upgrade");
    let inspection = instance::inspect(root.path(), "worker-a").unwrap();
    assert_eq!(inspection.artifact_digest, report.candidate.artifact_digest);
    assert_eq!(inspection.binary_digest, report.candidate.binary_digest);
    assert_eq!(inspection.config_path, record.config_path);
    assert_eq!(inspection.data_dir, record.data_dir);
    assert_eq!(inspection.workspace_root, record.workspace_root);
    assert_eq!(inspection.grants, record.grants);
    assert_eq!(inspection.limits, record.limits);
    assert_eq!(inspection.secret_refs, record.secret_refs);
    assert_eq!(inspection.active_profile.as_deref(), Some("research"));
    assert_eq!(inspection.process_health, "running");
    assert_eq!(inspection.runtime_phase.as_deref(), Some("ready"));
    assert!(outcome.verification.identity_preserved);
    assert!(outcome.verification.profiles_preserved);
    assert!(outcome.verification.history_preserved);
    assert!(outcome.verification.workspace_authorization_preserved);
    assert!(outcome.verification.credential_refs_preserved);
    assert_eq!(outcome.verification.capability_operation, "task");
    assert_eq!(
        outcome.verification.task_state.as_deref(),
        Some("succeeded")
    );
    assert_eq!(
        std::fs::read(root.path().join("worker-a/worker.toml")).unwrap(),
        config_before
    );
    let after = attach(root.path(), "worker-a", addr(root.path(), "worker-a"));
    let (status, old) = after.task(old_task["id"].as_str().unwrap());
    assert_eq!(status, 200, "{old}");
    let fresh = after.run("prompt:", "after-upgrade");
    assert!(output(&fresh).contains("PROMPT-UPGRADE-A"), "{fresh}");
    let profiles = instance::profile(root.path(), "worker-a", &["list".into(), "--json".into()])
        .expect("profile list");
    assert_eq!(profiles.status, 0);
    assert!(
        profiles.stdout.contains("\"research\""),
        "{}",
        profiles.stdout
    );
    let complete = journal(root.path(), "worker-a");
    assert_eq!(complete.status, "complete");
    assert!(complete.verification.is_some());
}

#[test]
fn incompatible_profiles_and_state_block_before_executable_change() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let first = release(cache.path(), "PROMPT-STATE-A", "git:state-a", &[]);
    let second = release(cache.path(), "PROMPT-STATE-B", "git:state-b", &[]);
    let record = create(root.path(), "state-worker", &first, None);
    drop(start(root.path(), "state-worker"));

    let state_path = record.data_dir.join("state.json");
    let mut state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    state["profiles"] = serde_json::json!({
        "legacy": {
            "name": "legacy",
            "active_revision": 1,
            "revisions": [{
                "revision": 1,
                "instruction": "legacy profile",
                "model": "deterministic",
                "capabilities": ["retired-agent"],
                "members": ["retired-agent"],
                "skills": [],
                "created_at": 1
            }]
        }
    });
    std::fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let report = instance::plan_upgrade(&request(root.path(), "state-worker", &second)).unwrap();
    assert!(
        report
            .blocked
            .iter()
            .any(|finding| finding.code == "upgrade.profile_capability_missing"),
        "{:?}",
        report.blocked
    );
    assert_eq!(
        instance::inspect(root.path(), "state-worker")
            .unwrap()
            .artifact_digest,
        record.artifact_digest
    );

    state["schema"] = serde_json::json!(99);
    state["profiles"] = serde_json::json!({});
    std::fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    let report = instance::plan_upgrade(&request(root.path(), "state-worker", &second)).unwrap();
    assert_eq!(report.schema_change.status, "incompatible");
    assert!(
        report
            .blocked
            .iter()
            .any(|finding| finding.code == "upgrade.schema_incompatible"),
        "{:?}",
        report.blocked
    );
    let error = instance::apply_upgrade(&request(root.path(), "state-worker", &second))
        .expect_err("incompatible state must stop");
    assert_eq!(error.code, "upgrade.incompatible");
    assert_eq!(
        instance::inspect(root.path(), "state-worker")
            .unwrap()
            .artifact_digest,
        record.artifact_digest
    );
}

#[test]
fn pinned_work_blocks_and_drain_preserves_queued_history() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let first = release(cache.path(), "PROMPT-QUEUE-A", "git:queue-a", &[]);
    let second = release(cache.path(), "PROMPT-QUEUE-B", "git:queue-b", &[]);
    create(root.path(), "queue-worker", &first, None);
    let running = start(root.path(), "queue-worker");

    let (status, active) = running.submit("delay:30000 first", "queue-active");
    assert!(status == 200 || status == 201, "{active}");
    let (status, queued) = running.submit("delay:50 second", "queue-waiting");
    assert!(status == 200 || status == 201, "{queued}");
    let active_id = active["task_id"].as_str().unwrap().to_string();
    let queued_id = queued["task_id"].as_str().unwrap().to_string();
    for _ in 0..100 {
        let status = running.status();
        if status["capacity"]["active"].as_u64() == Some(1)
            && status["capacity"]["queued"].as_u64() == Some(1)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut pinned = request(root.path(), "queue-worker", &second);
    pinned.work_policy = WorkPolicy::Pin;
    let report = instance::plan_upgrade(&pinned).unwrap();
    assert_eq!(report.work.action, "pin");
    assert!(
        report
            .blocked
            .iter()
            .any(|finding| finding.code == "upgrade.work_pinned"),
        "{:?}",
        report.blocked
    );
    assert!(instance::apply_upgrade(&pinned).is_err());
    assert_eq!(
        instance::inspect(root.path(), "queue-worker")
            .unwrap()
            .artifact_digest,
        format!("sha256:{}", first.file_name().unwrap().to_string_lossy())
    );

    // Finish the pinned work explicitly, then run a short queued pair so the
    // drain path is exercised without waiting on the long cancellation hold.
    running.cancel(&active_id);
    running.cancel(&queued_id);
    let (status, active) = running.submit("delay:1200 first", "drain-active");
    assert!(status == 200 || status == 201, "{active}");
    let (status, queued) = running.submit("delay:50 second", "drain-waiting");
    assert!(status == 200 || status == 201, "{queued}");
    let active_id = active["task_id"].as_str().unwrap().to_string();
    let queued_id = queued["task_id"].as_str().unwrap().to_string();
    for _ in 0..100 {
        let status = running.status();
        if status["capacity"]["active"].as_u64() == Some(1)
            && status["capacity"]["queued"].as_u64() == Some(1)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut drained = request(root.path(), "queue-worker", &second);
    drained.work_policy = WorkPolicy::Drain;
    let outcome = instance::apply_upgrade(&drained).expect("drain and upgrade");
    assert!(outcome.verification.history_preserved);
    let restarted = attach(
        root.path(),
        "queue-worker",
        addr(root.path(), "queue-worker"),
    );
    for task_id in [&active_id, &queued_id] {
        let (status, task) = restarted.task(task_id);
        assert_eq!(status, 200, "{task}");
        assert_eq!(task["state"], "succeeded", "{task}");
    }
}

#[test]
fn failed_capability_verification_restores_binary_and_data() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let first = release(cache.path(), "PROMPT-ROLLBACK-A", "git:rollback-a", &[]);
    let second = release(cache.path(), "PROMPT-ROLLBACK-B", "git:rollback-b", &[]);
    let record = create(root.path(), "rollback-worker", &first, None);
    let running = start(root.path(), "rollback-worker");
    profile(
        root.path(),
        "rollback-worker",
        "research",
        "PROFILE-ROLLBACK",
    );
    let old_task = running.run("prompt:", "rollback-history");
    let before: Value =
        serde_json::from_slice(&std::fs::read(record.data_dir.join("state.json")).unwrap())
            .unwrap();
    let tasks_before: Vec<String> = before["tasks"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();

    let mut upgrade = request(root.path(), "rollback-worker", &second);
    upgrade.verify_input = Some("provider-fail".into());
    let error = instance::apply_upgrade(&upgrade).expect_err("verification must fail");
    assert!(
        error.code == "upgrade.failed_rolled_back",
        "{}: {}",
        error.code,
        error.message
    );
    assert!(
        error.message.contains("binary restored"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("persisted data restored"),
        "{}",
        error.message
    );

    let inspection = instance::inspect(root.path(), "rollback-worker").unwrap();
    assert_eq!(inspection.artifact_digest, record.artifact_digest);
    assert_eq!(inspection.process_health, "running");
    assert_eq!(inspection.runtime_phase.as_deref(), Some("ready"));
    assert_eq!(inspection.active_profile.as_deref(), Some("research"));
    let restored: Value =
        serde_json::from_slice(&std::fs::read(record.data_dir.join("state.json")).unwrap())
            .unwrap();
    for task_id in &tasks_before {
        assert!(
            restored["tasks"].get(task_id).is_some(),
            "task {task_id} was lost"
        );
    }
    assert_eq!(restored["profiles"], before["profiles"]);
    let restarted = attach(
        root.path(),
        "rollback-worker",
        addr(root.path(), "rollback-worker"),
    );
    let (status, task) = restarted.task(old_task["id"].as_str().unwrap());
    assert_eq!(status, 200, "{task}");
    assert_eq!(task["state"], "succeeded");
    let recorded = journal(root.path(), "rollback-worker");
    assert_eq!(recorded.status, "rolled_back");
    assert!(recorded.binary_restored);
    assert!(recorded.data_restored);
    assert!(recorded
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("upgrade.verify_failed"));
}

#[test]
fn failed_readiness_restores_previous_binary() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let first = release(cache.path(), "PROMPT-READY-A", "git:ready-a", &[]);
    let second = release(cache.path(), "PROMPT-READY-B", "git:ready-b", &[]);
    let record = create(root.path(), "ready-worker", &first, None);
    let running = start(root.path(), "ready-worker");
    let old_task = running.run("prompt:", "readiness-history");

    // Occupy the candidate listener so readiness can never arrive.
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied listener");
    let occupied_addr = occupied.local_addr().unwrap().to_string();
    let mut upgrade = request(root.path(), "ready-worker", &second);
    upgrade.addr = Some(occupied_addr);
    let error = instance::apply_upgrade(&upgrade).expect_err("readiness must fail");
    assert!(
        error.code == "upgrade.failed_rolled_back",
        "{}: {}",
        error.code,
        error.message
    );
    drop(occupied);
    let inspection = instance::inspect(root.path(), "ready-worker").unwrap();
    assert_eq!(inspection.artifact_digest, record.artifact_digest);
    assert_eq!(inspection.process_health, "running");
    let restarted = attach(
        root.path(),
        "ready-worker",
        addr(root.path(), "ready-worker"),
    );
    let (status, task) = restarted.task(old_task["id"].as_str().unwrap());
    assert_eq!(status, 200, "{task}");
    let recorded = journal(root.path(), "ready-worker");
    assert!(recorded.binary_restored);
    assert!(recorded.data_restored);
}

#[test]
fn interrupted_upgrade_recovers_previous_binary_and_snapshot() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let first = release(cache.path(), "PROMPT-RECOVER-A", "git:recover-a", &[]);
    let second = release(cache.path(), "PROMPT-RECOVER-B", "git:recover-b", &[]);
    create(root.path(), "recover-worker", &first, None);
    let running = start(root.path(), "recover-worker");
    profile(root.path(), "recover-worker", "research", "PROFILE-RECOVER");
    let old_task = running.run("prompt:", "recover-history");
    instance::stop(root.path(), "recover-worker").unwrap();

    let previous_record: InstanceRecord = serde_json::from_slice(
        &std::fs::read(root.path().join("recover-worker/instance.json")).unwrap(),
    )
    .unwrap();
    let previous_state: InstanceState = serde_json::from_slice(
        &std::fs::read(root.path().join("recover-worker/state.json")).unwrap(),
    )
    .unwrap();
    let backup = root.path().join("recover-worker/backups/test");
    std::fs::create_dir_all(&backup).unwrap();
    copy_tree(&previous_record.data_dir, &backup.join("data"));
    let backup_marker = backup.join("data/marker");
    std::fs::write(&backup_marker, "before").unwrap();

    let mut candidate = previous_record.clone();
    candidate.release_dir = second.clone();
    candidate.binary = second.join("binary");
    candidate.artifact_digest = format!("sha256:{}", second.file_name().unwrap().to_string_lossy());
    candidate.binary_digest = bytes_digest(&std::fs::read(second.join("binary")).unwrap());
    std::fs::write(
        root.path().join("recover-worker/instance.json"),
        serde_json::to_vec_pretty(&candidate).unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.path().join("recover-worker/state.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": previous_state.schema,
            "lifecycle": "upgrading",
            "pid": null,
            "incarnation": null,
            "active_profile": previous_state.active_profile,
            "addr": null,
            "started_at": previous_state.started_at,
            "stopped_at": null,
            "last_error": null
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        previous_record.data_dir.join("interrupted-marker"),
        "candidate data",
    )
    .unwrap();
    let pending = UpgradeJournal {
        schema: 1,
        instance: "recover-worker".into(),
        status: "activating".into(),
        started_at: 1,
        updated_at: 1,
        policy: WorkPolicy::Drain,
        authorized_permissions: Vec::new(),
        previous_record: previous_record.clone(),
        candidate_record: candidate,
        previous_state: previous_state.clone(),
        backup: Some(backup.join("data")),
        backup_digest: None,
        error: None,
        binary_restored: false,
        data_restored: false,
        quarantine: None,
        verification: None,
    };
    std::fs::write(
        root.path().join("recover-worker/upgrade.json"),
        serde_json::to_vec_pretty(&pending).unwrap(),
    )
    .unwrap();

    let recovered = instance::recover_upgrade(root.path(), "recover-worker").expect("recovery");
    assert!(recovered.binary_restored);
    assert!(recovered.data_restored);
    let inspection = instance::inspect(root.path(), "recover-worker").unwrap();
    assert_eq!(inspection.artifact_digest, previous_record.artifact_digest);
    assert!(!previous_record.data_dir.join("interrupted-marker").exists());
    assert!(previous_record.data_dir.join("marker").exists());
    let restarted = start(root.path(), "recover-worker");
    let (status, task) = restarted.task(old_task["id"].as_str().unwrap());
    assert_eq!(status, 200, "{task}");
    assert_eq!(task["state"], "succeeded");
    let recorded = journal(root.path(), "recover-worker");
    assert_eq!(recorded.status, "recovered");
}

#[test]
fn upgrade_leaves_other_package_instance_and_external_config_untouched() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let configs = tempfile::tempdir().expect("operator config");
    let first = release(cache.path(), "PROMPT-EXTERNAL-A", "git:external-a", &[]);
    let second = release(cache.path(), "PROMPT-EXTERNAL-B", "git:external-b", &[]);

    let external = configs.path().join("site");
    std::fs::create_dir_all(external.join("agents")).unwrap();
    std::fs::create_dir_all(external.join("prompts")).unwrap();
    std::fs::write(external.join("prompts/general.md"), "EXTERNAL-USER-PROMPT").unwrap();
    std::fs::write(
        external.join("agents/general.toml"),
        "name = \"general\"\ndescription = \"external general\"\nmodel = \"deterministic\"\nprompt = \"general\"\n",
    )
    .unwrap();
    std::fs::write(
        external.join("worker.toml"),
        "instance = \"external-upgrade\"\nnamespace = \"local\"\ndefault_agent = \"general\"\ntoken = \"external-token\"\ntoken_scopes = [\"task:submit\", \"task:read\", \"session:write\", \"profile:read\", \"profile:write\"]\ngrants = [\"task:submit\", \"task:read\"]\n\n[paths]\nagents = \"agents\"\nprompts = \"prompts\"\n\n[storage]\ndirectory = \"state\"\n",
    )
    .unwrap();
    let config_before = std::fs::read(external.join("worker.toml")).unwrap();
    let prompt_before = std::fs::read(external.join("prompts/general.md")).unwrap();

    let external_record = create(
        root.path(),
        "external-worker",
        &first,
        Some(external.join("worker.toml")),
    );
    let other_record = create(root.path(), "other-worker", &first, None);

    let mut upgrade = request(root.path(), "external-worker", &second);
    upgrade.verify_input = Some("prompt:".into());
    let outcome = instance::apply_upgrade(&upgrade).expect("external config upgrade");
    assert!(outcome.report.config.preflight_ok);
    assert!(
        outcome
            .report
            .config
            .references
            .iter()
            .any(|reference| reference.kind == "prompts"
                && reference.path == external.join("prompts"))
    );
    assert_eq!(
        std::fs::read(external.join("worker.toml")).unwrap(),
        config_before
    );
    assert_eq!(
        std::fs::read(external.join("prompts/general.md")).unwrap(),
        prompt_before
    );
    let inspection = instance::inspect(root.path(), "external-worker").unwrap();
    assert_eq!(inspection.config_path, external_record.config_path);
    assert_eq!(
        inspection.artifact_digest,
        format!("sha256:{}", second.file_name().unwrap().to_string_lossy())
    );
    let other = instance::inspect(root.path(), "other-worker").unwrap();
    assert_eq!(other.artifact_digest, other_record.artifact_digest);
    drop(attach(
        root.path(),
        "external-worker",
        addr(root.path(), "external-worker"),
    ));
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
