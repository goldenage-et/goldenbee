//! Independent Worker instances from one verified General package digest.
//!
//! These are public operator-behavior tests: create, start, inspect, profile,
//! restart, stop, and isolation. They drive the real installed General binary
//! through the shared Worker runtime; no registry or Gateway participates.

use base64::Engine;
use common::{bytes_digest, ReleaseMetadata};
use ed25519_dalek::{Signer, SigningKey};
use install::{
    encode_metadata, seed_assets, write_layout_with_seeds, InstallOutcome, Installer, SeedFile,
    TrustPolicy,
};
use instance::{CreateRequest, InstanceLimits};
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
fn release(cache: &Path, prompt: &str, revision: &str) -> PathBuf {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_general")).expect("general binary");
    let source = serde_json::to_vec(&serde_json::json!({
        "revision": revision,
        "tree": bytes_digest(format!("general source tree {revision}").as_bytes()),
    }))
    .unwrap();
    let seeds = seed_assets_for(prompt);
    let metadata = ReleaseMetadata {
        schema: common::RELEASE_METADATA_SCHEMA,
        package: general::manifest(),
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
            // The constrained smoke probe and both launcher modes must have
            // run under the unchanged 512 MiB address-space ceiling before any
            // instance is created from this release.
            let record: Value = serde_json::from_slice(
                &std::fs::read(release.directory.join("record.json")).expect("install record"),
            )
            .expect("install record JSON");
            assert_eq!(
                record["registered"],
                serde_json::json!([
                    "browser",
                    "close_subagent",
                    "exec_command",
                    "interrupt_subagent",
                    "list_subagents",
                    "manual",
                    "observe_knowledge",
                    "recall_knowledge",
                    "save_artifact",
                    "send_input",
                    "spawn_subagent",
                    "wait_subagent"
                ])
            );
            assert_eq!(
                record["launcher_modes"],
                serde_json::json!(["dev", "serve"])
            );
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

fn start(root: &Path, id: &str) -> Running {
    let started = instance::start(root, id, None).expect("instance starts");
    let token = std::fs::read_to_string(root.join(id).join("token"))
        .expect("token")
        .trim()
        .to_string();
    Running {
        root: root.to_path_buf(),
        id: id.to_string(),
        addr: started.addr,
        token,
    }
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
        for _ in 0..500 {
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
}

fn output(task: &Value) -> String {
    task["output"].as_str().unwrap_or_default().to_string()
}

fn create(
    root: &Path,
    id: &str,
    release: &Path,
    grants: &[&str],
    read_only: bool,
) -> instance::InstanceRecord {
    let mut request = CreateRequest::new(root, id, release);
    request.grants = grants.iter().map(|grant| (*grant).into()).collect();
    request.read_only = read_only;
    request.limits = InstanceLimits {
        max_active: 2,
        max_queue: 4,
        per_principal: 2,
    };
    request.secret_refs = vec![format!("secret://{id}/model-key")];
    instance::create(request).expect("instance created").record
}

#[test]
fn two_instances_share_one_digest_and_run_independently() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let release = release(cache.path(), "PROMPT-SEED", "git:instance-two");

    let first = create(
        root.path(),
        "worker-a",
        &release,
        &[
            "task:submit",
            "task:read",
            "session:write",
            "profile:read",
            "profile:write",
        ],
        false,
    );
    let second = create(
        root.path(),
        "worker-b",
        &release,
        &[
            "task:submit",
            "task:read",
            "session:write",
            "profile:read",
            "profile:write",
        ],
        false,
    );
    let mut repeat_request = CreateRequest::new(root.path(), "worker-a", &release);
    repeat_request.grants = vec!["task:submit".into(), "task:read".into()];
    let repeat = instance::create(repeat_request).expect("idempotent create");

    assert_eq!(first.artifact_digest, second.artifact_digest);
    assert_eq!(first.binary_digest, second.binary_digest);
    assert_ne!(first.id, second.id);
    assert_ne!(first.data_dir, second.data_dir);
    assert_ne!(first.workspace_root, second.workspace_root);
    assert_ne!(first.secret_refs, Vec::<String>::new());
    assert!(
        !repeat.created,
        "identical create converges without rewriting"
    );

    let token_a = std::fs::read_to_string(root.path().join("worker-a/token")).unwrap();
    let token_b = std::fs::read_to_string(root.path().join("worker-b/token")).unwrap();
    assert_ne!(token_a, token_b);

    // User-owned assets are instance-scoped, not copy-on-start package state.
    std::fs::write(
        root.path().join("worker-a/assets/prompts/general.md"),
        "PROMPT-INSTANCE-A",
    )
    .unwrap();
    std::fs::write(
        root.path().join("worker-b/assets/prompts/general.md"),
        "PROMPT-INSTANCE-B",
    )
    .unwrap();

    let worker_a = start(root.path(), "worker-a");
    let worker_b = start(root.path(), "worker-b");

    let inspection_a = instance::inspect(root.path(), "worker-a").unwrap();
    let inspection_b = instance::inspect(root.path(), "worker-b").unwrap();
    assert_eq!(inspection_a.process_health, "running");
    assert_eq!(inspection_a.lifecycle, "running");
    assert_eq!(inspection_a.task_activity, "idle");
    assert_eq!(inspection_b.process_health, "running");
    assert_ne!(inspection_a.incarnation, inspection_b.incarnation);
    assert_ne!(inspection_a.pid, inspection_b.pid);

    let profile = instance::profile(
        root.path(),
        "worker-a",
        &[
            "create".into(),
            "alpha".into(),
            "--instruction".into(),
            "Instance A profile".into(),
            "--member".into(),
            "general".into(),
            "--capability".into(),
            "general".into(),
            "--grant".into(),
            "task:submit".into(),
            "--activate".into(),
        ],
    )
    .unwrap();
    assert_eq!(profile.status, 0, "{}", profile.stderr);
    assert_eq!(
        instance::inspect(root.path(), "worker-a")
            .unwrap()
            .active_profile
            .as_deref(),
        Some("alpha")
    );
    assert_eq!(
        instance::inspect(root.path(), "worker-b")
            .unwrap()
            .active_profile,
        None
    );

    let task_a = worker_a.run("prompt:", "parallel-a");
    let task_b = worker_b.run("prompt:", "parallel-b");
    assert!(output(&task_a).contains("PROMPT-INSTANCE-A"), "{task_a}");
    assert!(output(&task_b).contains("PROMPT-INSTANCE-B"), "{task_b}");
    assert!(!output(&task_a).contains("INSTANCE-B"));
    assert!(!output(&task_b).contains("INSTANCE-A"));
    assert_eq!(task_a["state"], "succeeded");
    assert_eq!(task_b["state"], "succeeded");

    // Restart keeps stable identity and durable task history, but a new
    // process incarnation. The other instance is untouched.
    let old_incarnation = inspection_a.incarnation.clone();
    instance::stop(root.path(), "worker-a").unwrap();
    let inspection_stopped = instance::inspect(root.path(), "worker-a").unwrap();
    assert_eq!(inspection_stopped.lifecycle, "stopped");
    assert_eq!(inspection_stopped.process_health, "exited");
    let restarted = start(root.path(), "worker-a");
    let inspection_restarted = instance::inspect(root.path(), "worker-a").unwrap();
    assert_eq!(inspection_restarted.instance, "worker-a");
    assert_ne!(inspection_restarted.incarnation, old_incarnation);
    assert_eq!(restarted.task(task_a["id"].as_str().unwrap()).0, 200);
    assert_eq!(
        instance::inspect(root.path(), "worker-b")
            .unwrap()
            .process_health,
        "running"
    );
}

#[test]
fn read_only_instance_denies_escalation_and_cross_instance_access() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let release = release(cache.path(), "PROMPT-READONLY", "git:instance-readonly");

    let writer = create(
        root.path(),
        "writer",
        &release,
        &[
            "task:submit",
            "task:read",
            "session:write",
            "profile:read",
            "profile:write",
        ],
        false,
    );
    let reader = create(
        root.path(),
        "reader",
        &release,
        &["task:read", "profile:read"],
        true,
    );
    assert!(!reader.workspace_write);
    assert!(writer.workspace_write);

    let writer_worker = start(root.path(), "writer");
    let reader_worker = start(root.path(), "reader");
    let writer_task = writer_worker.run("prompt:", "writer-task");

    // The reader has no task:submit authority even though the same verified
    // executable and the same package are used.
    let (status, denied) = reader_worker.submit("prompt:", "reader-denied");
    assert_eq!(status, 403, "{denied}");
    assert_eq!(denied["code"], "scope.denied");

    // Copying the writer's instruction cannot grant write authority, and the
    // reader cannot create a profile outside its instance grants.
    std::fs::write(
        root.path().join("reader/assets/prompts/general.md"),
        "PROMPT-INSTANCE-A-WRITER",
    )
    .unwrap();
    let created = instance::profile(
        root.path(),
        "reader",
        &[
            "create".into(),
            "escalate".into(),
            "--instruction".into(),
            "COPied writer".into(),
            "--member".into(),
            "general".into(),
            "--capability".into(),
            "general".into(),
            "--grant".into(),
            "task:submit".into(),
        ],
    )
    .unwrap();
    assert_ne!(created.status, 0);
    assert!(
        created.stderr.contains("scope.denied")
            || created.stderr.contains("profile.grant_escalation"),
        "{}",
        created.stderr
    );

    // A task id from one instance does not exist in the other, and the two
    // tokens are not interchangeable.
    assert_eq!(
        reader_worker.task(writer_task["id"].as_str().unwrap()).0,
        404
    );
    let foreign = reqwest::blocking::Client::new()
        .get(format!("{}/v1/status", writer_worker.base()))
        .bearer_auth(&reader_worker.token)
        .send()
        .unwrap();
    assert_eq!(foreign.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[test]
fn seed_defaults_revisions_and_profile_removal_leave_package_and_other_instance() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let first_release = release(cache.path(), "SEED-ONE", "git:instance-seed-one");

    let first = create(
        root.path(),
        "seed-a",
        &first_release,
        &["task:submit", "task:read", "profile:read", "profile:write"],
        false,
    );
    let seed_one =
        std::fs::read_to_string(root.path().join("seed-a/assets/prompts/general.md")).unwrap();
    assert!(seed_one.contains("SEED-ONE"));
    std::fs::write(
        root.path().join("seed-a/assets/prompts/general.md"),
        "USER-EDITED-PROMPT",
    )
    .unwrap();

    let profile = instance::profile(
        root.path(),
        "seed-a",
        &[
            "create".into(),
            "research".into(),
            "--instruction".into(),
            "USER-PROFILE-V1".into(),
            "--member".into(),
            "general".into(),
            "--capability".into(),
            "general".into(),
            "--grant".into(),
            "task:submit".into(),
            "--activate".into(),
        ],
    )
    .unwrap();
    assert_eq!(profile.status, 0, "{}", profile.stderr);
    let active = instance::inspect(root.path(), "seed-a").unwrap();
    assert_eq!(active.active_profile.as_deref(), Some("research"));

    // A later release with different seed defaults creates a new instance from
    // the new digest without overwriting the first instance's user revision.
    let second_release = release(cache.path(), "SEED-TWO", "git:instance-seed-two");
    assert_ne!(first_release, second_release);
    let second = create(
        root.path(),
        "seed-b",
        &second_release,
        &["task:submit", "task:read"],
        false,
    );
    assert_eq!(second.package, first.package);
    assert_ne!(second.artifact_digest, first.artifact_digest);
    assert!(
        std::fs::read_to_string(root.path().join("seed-b/assets/prompts/general.md"))
            .unwrap()
            .contains("SEED-TWO")
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("seed-a/assets/prompts/general.md")).unwrap(),
        "USER-EDITED-PROMPT"
    );

    // Re-running create for the existing id converges and does not copy seeds.
    let mut repeat = CreateRequest::new(root.path(), "seed-a", &first_release);
    repeat.grants = vec!["task:submit".into(), "task:read".into()];
    assert!(!instance::create(repeat).unwrap().created);
    assert_eq!(
        std::fs::read_to_string(root.path().join("seed-a/assets/prompts/general.md")).unwrap(),
        "USER-EDITED-PROMPT"
    );

    let revised = instance::profile(
        root.path(),
        "seed-a",
        &[
            "revise".into(),
            "research".into(),
            "--expected".into(),
            "1".into(),
            "--instruction".into(),
            "USER-PROFILE-V2".into(),
        ],
    )
    .unwrap();
    assert_eq!(revised.status, 0, "{}", revised.stderr);

    // Removing configuration leaves the installed package and the other
    // instance intact.
    let deactivated = instance::profile(
        root.path(),
        "seed-a",
        &["deactivate".into(), "research".into()],
    )
    .unwrap();
    assert_eq!(deactivated.status, 0, "{}", deactivated.stderr);
    let deleted =
        instance::profile(root.path(), "seed-a", &["delete".into(), "research".into()]).unwrap();
    assert_eq!(deleted.status, 0, "{}", deleted.stderr);
    assert!(first_release.join("binary").is_file());
    let profiles =
        instance::profile(root.path(), "seed-a", &["list".into(), "--json".into()]).unwrap();
    assert_eq!(profiles.status, 0);
    assert!(!profiles.stdout.contains("\"research\""));
    let second_profile = instance::inspect(root.path(), "seed-b").unwrap();
    assert_eq!(second_profile.instance, "seed-b");
    assert_eq!(second_profile.artifact_digest, second.artifact_digest);
}

#[test]
fn failed_startup_is_faulted_and_stop_stays_instance_scoped() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let release = release(cache.path(), "PROMPT-STARTUP", "git:instance-startup");
    create(
        root.path(),
        "broken",
        &release,
        &["task:submit", "task:read"],
        false,
    );
    create(
        root.path(),
        "healthy",
        &release,
        &["task:submit", "task:read"],
        false,
    );

    // A missing configured asset fails startup truthfully.
    std::fs::remove_file(root.path().join("broken/assets/prompts/general.md")).unwrap();
    let error = instance::start(root.path(), "broken", None).expect_err("startup must fail");
    assert_eq!(error.code, "instance.startup_failed");
    let broken = instance::inspect(root.path(), "broken").unwrap();
    assert_eq!(broken.lifecycle, "faulted");
    assert_ne!(broken.process_health, "running");
    assert_eq!(broken.task_activity, "unknown");
    assert!(broken.last_error.is_some());

    let healthy = start(root.path(), "healthy");
    assert_eq!(
        instance::inspect(root.path(), "healthy")
            .unwrap()
            .process_health,
        "running"
    );
    // Stopping one instance leaves the other's assets and process untouched.
    let broken_prompt = root.path().join("broken/assets/prompts/general.md");
    assert!(!broken_prompt.exists());
    instance::stop(root.path(), "healthy").unwrap();
    let healthy_stopped = instance::inspect(root.path(), "healthy").unwrap();
    assert_eq!(healthy_stopped.lifecycle, "stopped");
    assert!(!broken_prompt.exists());
    assert_eq!(
        instance::inspect(root.path(), "broken").unwrap().lifecycle,
        "faulted"
    );
    drop(healthy);
}

#[test]
fn interrupted_creation_recovers_and_start_needs_no_registry() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let release = release(cache.path(), "PROMPT-OFFLINE", "git:instance-offline");

    // A crash before the atomic rename leaves only a private staging entry.
    let stale = root.path().join(".staging-offline-dead");
    std::fs::create_dir_all(stale.join("state")).unwrap();
    std::fs::write(stale.join("instance.json"), "{ partial").unwrap();
    let invalid = instance::create(CreateRequest::new(root.path(), "../escape", &release));
    assert_eq!(invalid.expect_err("invalid id").code, "instance.id_invalid");

    create(
        root.path(),
        "offline",
        &release,
        &["task:submit", "task:read"],
        false,
    );
    assert!(!stale.exists(), "stale staging is cleaned");
    let visible: Vec<String> = std::fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(visible, vec!["offline".to_string()]);

    // Execution uses only the verified release and the instance directory.
    // A bogus registry variable in the parent environment is not inherited.
    std::env::set_var("GOLDENBEE_REGISTRY_URL", "http://127.0.0.1:1/stopped");
    let worker = start(root.path(), "offline");
    std::env::remove_var("GOLDENBEE_REGISTRY_URL");
    let task = worker.run("prompt:", "offline-task");
    assert_eq!(task["state"], "succeeded");
}

#[test]
fn two_external_configs_and_third_cwd_use_the_same_digest() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let release = release(cache.path(), "PROMPT-SEED", "git:instance-external");

    let configs = tempfile::tempdir().expect("operator configs");
    let first_dir = configs.path().join("first");
    let second_dir = configs.path().join("second");
    for (dir, prompt, port_token) in [
        (&first_dir, "EXTERNAL-CONFIG-A", "token-a"),
        (&second_dir, "EXTERNAL-CONFIG-B", "token-b"),
    ] {
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::create_dir_all(dir.join("prompts")).unwrap();
        std::fs::write(dir.join("prompts/general.md"), prompt).unwrap();
        std::fs::write(
            dir.join("agents/general.toml"),
            "name = \"general\"\ndescription = \"external general\"\nmodel = \"deterministic\"\nprompt = \"general\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("worker.toml"),
            format!(
                "instance = \"external\"\nnamespace = \"local\"\ndefault_agent = \"general\"\ntoken = \"{port_token}\"\ntoken_scopes = [\"task:submit\", \"task:read\", \"session:write\"]\ngrants = [\"task:submit\", \"task:read\"]\n\n[paths]\nagents = \"agents\"\nprompts = \"prompts\"\n\n[storage]\ndirectory = \"state\"\n"
            ),
        )
        .unwrap();
    }

    let mut first = CreateRequest::new(root.path(), "external-a", &release);
    first.config = Some(first_dir.join("worker.toml"));
    let mut second = CreateRequest::new(root.path(), "external-b", &release);
    second.config = Some(second_dir.join("worker.toml"));
    let first = instance::create(first).expect("external a").record;
    let second = instance::create(second).expect("external b").record;
    assert_eq!(first.artifact_digest, second.artifact_digest);
    assert_eq!(first.binary_digest, second.binary_digest);
    assert_ne!(first.config_path, second.config_path);
    assert!(!first.generated_config);

    let third_cwd = tempfile::tempdir().expect("third cwd");
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(third_cwd.path()).unwrap();
    let worker_a = instance::start(root.path(), "external-a", None).expect("start a");
    let worker_b = instance::start(root.path(), "external-b", None).expect("start b");
    std::env::set_current_dir(previous).unwrap();
    let token_a = "token-a".to_string();
    let token_b = "token-b".to_string();
    let a = Running {
        root: root.path().to_path_buf(),
        id: "external-a".into(),
        addr: worker_a.addr,
        token: token_a,
    };
    let b = Running {
        root: root.path().to_path_buf(),
        id: "external-b".into(),
        addr: worker_b.addr,
        token: token_b,
    };
    let task_a = a.run("prompt:", "external-a");
    let task_b = b.run("prompt:", "external-b");
    assert!(output(&task_a).contains("EXTERNAL-CONFIG-A"), "{task_a}");
    assert!(output(&task_b).contains("EXTERNAL-CONFIG-B"), "{task_b}");
}

#[test]
fn one_general_package_creates_several_role_instances() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("instances");
    let release = release(cache.path(), "PROMPT-ROLES", "git:instance-roles");

    let developer = create(
        root.path(),
        "developer",
        &release,
        &["task:submit", "task:read", "profile:read", "profile:write"],
        false,
    );
    let reviewer = create(
        root.path(),
        "reviewer",
        &release,
        &["task:read", "profile:read"],
        true,
    );
    let master = create(
        root.path(),
        "master",
        &release,
        &["task:submit", "task:read", "profile:read", "profile:write"],
        false,
    );
    for record in [&developer, &reviewer, &master] {
        assert_eq!(record.package, "general");
        assert_eq!(record.artifact_digest, developer.artifact_digest);
    }
    assert!(!reviewer.workspace_write);
    assert_ne!(developer.id, reviewer.id);
    assert_ne!(reviewer.id, master.id);

    let created = instance::profile(
        root.path(),
        "developer",
        &[
            "create".into(),
            "developer-role".into(),
            "--instruction".into(),
            "Implement the change.".into(),
            "--member".into(),
            "general".into(),
            "--capability".into(),
            "general".into(),
            "--grant".into(),
            "task:submit".into(),
            "--activate".into(),
        ],
    )
    .unwrap();
    assert_eq!(created.status, 0, "{}", created.stderr);
    // The review role denies submission without adding another executable.
    let (status, denied) = {
        let worker = start(root.path(), "reviewer");
        worker.submit("prompt:", "reviewer-task")
    };
    assert_eq!(status, 403);
    assert_eq!(denied["code"], "scope.denied");
}
