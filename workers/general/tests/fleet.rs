//! Ticket 44 public behavior: authorized fleet lifecycle through Gateway-owned
//! local hosts, with standalone local management and no caller filesystem access.
//!
//! Each child Gateway owns its local provision/upgrade/rollback. A root Gateway
//! orchestrates through scoped host credentials that are ordinary Gateway caller
//! tokens. Product access travels through the SDK and the independent CLI.

use base64::Engine;
use common::{bytes_digest, ReleaseMetadata};
use ed25519_dalek::{Signer, SigningKey};
use install::{
    encode_metadata, seed_assets, write_layout_with_seeds, InstallOutcome, Installer, SeedFile,
    TrustPolicy,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const DELEGATION_SECRET: &str = "t44-shared-delegation-secret";
const ADMIN_TOKEN: &str = "t44-admin-token";
const LIMITED_TOKEN: &str = "t44-limited-token";
const HOST_A_TOKEN: &str = "t44-host-a-token";
const HOST_B_TOKEN: &str = "t44-host-b-token";
const WORKER_A_TOKEN: &str = "t44-worker-a-control-token";

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
            "name = \"general\"\ndescription = \"seeded general agent\"\nmodel = \"deterministic\"\nprompt = \"general\"\n",
        ),
        SeedFile::new("assets/prompts/general.md", prompt.to_string()),
    ]
}

/// Install the real General binary as one signed release in the host cache.
fn release(
    cache: &Path,
    prompt: &str,
    revision: &str,
    extra_permissions: &[&str],
) -> (String, PathBuf) {
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
    let installed = match outcome {
        InstallOutcome::Installed(release) | InstallOutcome::Converged(release) => release,
    };
    (installed.artifact_digest, installed.directory)
}

fn binary(name: &str) -> PathBuf {
    if let Ok(path) = std::env::var(format!("CARGO_BIN_EXE_{name}")) {
        return PathBuf::from(path);
    }
    let current = std::env::current_exe().expect("test executable path");
    let mut directory = current.parent();
    while let Some(candidate_directory) = directory {
        let candidate = candidate_directory.join(name);
        if candidate.is_file() {
            return candidate;
        }
        directory = candidate_directory.parent();
    }
    panic!("{name} is missing; build workspace binaries first");
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("ephemeral port")
        .local_addr()
        .expect("address")
        .port()
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::new()
}

struct Process {
    child: Child,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_announced(mut command: Command) -> (Process, String) {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("process starts");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("process announces its listener");
    // Keep draining later startup lines. Dropping the read end after the first
    // announcement can deliver SIGPIPE to a composer that prints its supported
    // operations on the next line.
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
    });
    let base = line
        .trim()
        .rsplit_once(' ')
        .map(|(_, address)| address.to_string())
        .unwrap_or_else(|| panic!("listener announcement: {line}"));
    (Process { child }, base)
}

fn spawn_gateway(config: &Path) -> (Process, String) {
    let mut command = Command::new(binary("gb-gateway"));
    command.args([
        "serve",
        "--addr",
        "127.0.0.1:0",
        "--config",
        config.to_str().expect("gateway config"),
    ]);
    command.env("GOLDENBEE_DELEGATION_SECRET", DELEGATION_SECRET);
    spawn_announced(command)
}

fn cli(gateway: &str, token: &str, arguments: &[&str]) -> (i32, String, String) {
    let output = Command::new(binary("gb-client"))
        .args(["--gateway", gateway, "--token", token])
        .args(arguments)
        .output()
        .expect("cli runs");
    (
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn cli_json(gateway: &str, token: &str, arguments: &[&str]) -> Value {
    let (status, stdout, stderr) = cli(gateway, token, arguments);
    assert_eq!(status, 0, "cli failed: {stderr}\n{stdout}");
    serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("cli JSON: {stdout}"))
}

fn api(method: &str, url: &str, token: &str, body: Option<Value>) -> (u16, Value) {
    let mut request = match method {
        "GET" => client().get(url),
        "POST" => client().post(url),
        other => panic!("unsupported method {other}"),
    }
    .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().expect("request responds");
    let status = response.status().as_u16();
    let body = response.json().unwrap_or(Value::Null);
    (status, body)
}

fn wait_for<F: FnMut() -> bool>(mut ready: F, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "{what} did not become ready");
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn gateway_config(directory: &Path, registry: &Path, hosts: Value) -> PathBuf {
    let path = directory.join("gateway.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "listen": "127.0.0.1:0",
            "data_dir": directory.join("gateway-state"),
            "registry": registry,
            "delegation_secret": DELEGATION_SECRET,
            "callers": [
                {
                    "token": ADMIN_TOKEN,
                    "id": "fleet-admin",
                    "namespace": "fleet",
                    "scopes": ["*"]
                },
                {
                    "token": LIMITED_TOKEN,
                    "id": "limited",
                    "namespace": "fleet",
                    "scopes": ["task:read"]
                }
            ],
            "hosts": hosts
        }))
        .unwrap(),
    )
    .expect("gateway config");
    path
}

#[allow(clippy::too_many_arguments)]
fn host_gateway_config(
    directory: &Path,
    host: &str,
    token: &str,
    instances_root: &Path,
    packages_root: &Path,
    configs: &Path,
    workspaces: &Path,
    operations: &[&str],
    instances: &[&str],
) -> PathBuf {
    let path = directory.join(format!("{host}.json"));
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "listen": "127.0.0.1:0",
            "data_dir": directory.join(format!("{host}-state")),
            "registry": directory.join(format!("{host}-registry.json")),
            "delegation_secret": DELEGATION_SECRET,
            "host_id": host,
            "instances_root": instances_root,
            "packages_root": packages_root,
            "allowed_config_roots": [configs],
            "allowed_workspace_roots": [workspaces],
            "allowed_grants": [
                "task:submit", "task:read", "task:cancel", "session:write",
                "profile:read", "profile:write"
            ],
            "allowed_profile_grants": ["task:submit", "task:read", "profile:read"],
            "allowed_secret_prefixes": ["secret://"],
            "operations": operations,
            "allowed_instances": instances,
            "callers": [{
                "token": token,
                "id": host,
                "namespace": "fleet",
                "scopes": ["instance:admin"]
            }]
        }))
        .unwrap(),
    )
    .expect("host gateway config");
    path
}

fn external_config(
    configs: &Path,
    instance: &str,
    prompt: &str,
    token: &str,
) -> (PathBuf, PathBuf) {
    let directory = configs.join(instance);
    let agents = directory.join("assets/agents");
    let prompts = directory.join("assets/prompts");
    std::fs::create_dir_all(&agents).expect("agents");
    std::fs::create_dir_all(&prompts).expect("prompts");
    std::fs::write(
        agents.join("general.toml"),
        "name = \"general\"\ndescription = \"operator agent\"\nmodel = \"deterministic\"\nprompt = \"general\"\n",
    )
    .expect("agent");
    std::fs::write(prompts.join("general.md"), prompt).expect("prompt");
    let config = directory.join("worker.toml");
    std::fs::write(
        &config,
        format!(
            "instance = \"{instance}\"\nnamespace = \"fleet\"\ndefault_agent = \"general\"\ntoken = \"{token}\"\ntoken_scopes = [\"*\"]\ngrants = [\"task:submit\", \"task:read\", \"task:cancel\", \"session:write\", \"profile:read\", \"profile:write\"]\n\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"state\"\n"
        ),
    )
    .expect("config");
    (config, directory)
}

fn submit(base: &str, token: &str, profile: Option<&str>, key: &str) -> Value {
    let mut body = json!({
        "agent": "general",
        "session_id": format!("session-{key}"),
        "input": "prompt:",
        "idempotency_key": key,
    });
    if let Some(profile) = profile {
        body["profile"] = json!(profile);
    }
    let (status, submitted) = api("POST", &format!("{base}/v1/tasks"), token, Some(body));
    assert!(status == 200 || status == 201, "{status} {submitted}");
    let task_id = submitted["task_id"].as_str().expect("task id");
    for _ in 0..500 {
        let (_, task) = api("GET", &format!("{base}/v1/tasks/{task_id}"), token, None);
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

#[test]
fn authorized_fleet_lifecycle_uses_sdk_and_cli_without_gateway_filesystem_access() {
    let cache = tempfile::tempdir().expect("cache");
    let instances = tempfile::tempdir().expect("instances");
    let configs = tempfile::tempdir().expect("configs");
    let workspaces = tempfile::tempdir().expect("workspaces");
    let state = tempfile::tempdir().expect("gateway state");
    let (first_digest, _) = release(cache.path(), "PROMPT-FLEET-V1", "git:fleet-v1", &[]);
    let (second_digest, _) = release(
        cache.path(),
        "PROMPT-FLEET-V2",
        "git:fleet-v2",
        &["exec:command"],
    );

    let host_a_config = host_gateway_config(
        state.path(),
        "host-a",
        HOST_A_TOKEN,
        instances.path(),
        cache.path(),
        configs.path(),
        workspaces.path(),
        &[
            "provision",
            "inspect",
            "upgrade",
            "rollback",
            "start",
            "stop",
            "restart",
        ],
        &["fleet-a"],
    );
    let host_b_config = host_gateway_config(
        state.path(),
        "host-b",
        HOST_B_TOKEN,
        &state.path().join("instances-b"),
        &state.path().join("packages-b"),
        &state.path().join("configs-b"),
        &state.path().join("workspaces-b"),
        &["provision", "inspect", "upgrade"],
        &["fleet-b"],
    );
    let (host_a, host_a_base) = spawn_gateway(&host_a_config);
    let (_host_b, host_b_base) = spawn_gateway(&host_b_config);
    wait_for(
        || {
            api(
                "GET",
                &format!("{host_a_base}/v1/health"),
                ADMIN_TOKEN,
                None,
            )
            .0 == 200
        },
        "host-a gateway",
    );
    wait_for(
        || {
            api(
                "GET",
                &format!("{host_b_base}/v1/health"),
                ADMIN_TOKEN,
                None,
            )
            .0 == 200
        },
        "host-b gateway",
    );
    let gateway_file = gateway_config(
        state.path(),
        &state.path().join("registry.json"),
        json!([
            {
                "id": "host-a",
                "endpoint": host_a_base,
                "token": HOST_A_TOKEN,
                "instances": ["fleet-a"]
            },
            {
                "id": "host-b",
                "endpoint": host_b_base,
                "token": HOST_B_TOKEN,
                "instances": ["fleet-b"]
            }
        ]),
    );
    let (gateway, gateway_base) = spawn_gateway(&gateway_file);
    wait_for(
        || {
            api(
                "GET",
                &format!("{gateway_base}/v1/health"),
                ADMIN_TOKEN,
                None,
            )
            .0 == 200
        },
        "gateway",
    );

    let (config, config_dir) = external_config(
        configs.path(),
        "fleet-a",
        "PROMPT-EXTERNAL-FLEET",
        WORKER_A_TOKEN,
    );
    let config_before = std::fs::read(&config).expect("config bytes");
    let worker_addr = format!("127.0.0.1:{}", free_port());

    let provisioned = cli_json(
        &gateway_base,
        ADMIN_TOKEN,
        &[
            "fleet",
            "provision",
            "host-a",
            "--operation",
            "op-provision-a",
            "--instance",
            "fleet-a",
            "--worker",
            "worker-a",
            "--release",
            &first_digest,
            "--addr",
            &worker_addr,
            "--config",
            config.to_str().unwrap(),
            "--control-token",
            WORKER_A_TOKEN,
            "--grant",
            "task:submit",
            "--grant",
            "task:read",
            "--grant",
            "task:cancel",
            "--grant",
            "session:write",
            "--grant",
            "profile:read",
            "--grant",
            "profile:write",
            "--profile-name",
            "research",
            "--profile-instruction",
            "PROFILE-FLEET-RESEARCH",
            "--profile-member",
            "general",
            "--profile-capability",
            "general",
            "--profile-grant",
            "task:submit",
            "--profile-create",
            "--json",
        ],
    );
    assert_eq!(provisioned["status"], "succeeded", "{provisioned}");
    assert_eq!(provisioned["kind"], "provision");
    assert_eq!(provisioned["view"]["artifact_digest"], first_digest);
    assert_eq!(provisioned["view"]["instance"], "fleet-a");
    assert_eq!(provisioned["view"]["active_profile"], "research");
    assert_eq!(provisioned["view"]["lifecycle"], "running");
    assert_eq!(provisioned["view"]["process_health"], "running");
    assert_eq!(
        provisioned["view"]["compatibility"]["compatible"], true,
        "{provisioned}"
    );
    assert_eq!(provisioned["worker"]["id"], "worker-a");
    assert_eq!(provisioned["worker"]["status"], "registered");
    assert_eq!(provisioned["worker"]["reachable"], true);
    assert!(
        !provisioned
            .to_string()
            .contains(config.to_str().expect("config path")),
        "Gateway inspection does not expose host filesystem paths"
    );

    // The SDK performs the same inspection and receives the same identity.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("sdk runtime");
    let sdk = sdk::Client::new(gateway_base.clone(), ADMIN_TOKEN);
    let hosts = runtime.block_on(sdk.hosts()).expect("sdk hosts");
    let host = hosts
        .iter()
        .find(|host| host.host == "host-a")
        .expect("host-a advertised");
    assert!(host
        .supported
        .iter()
        .any(|operation| operation == "provision"));
    let sdk_view = runtime
        .block_on(sdk.inspect_instance("host-a", "fleet-a"))
        .expect("sdk inspection");
    assert_eq!(sdk_view.view.artifact_digest, first_digest);
    assert_eq!(sdk_view.view.active_profile.as_deref(), Some("research"));
    assert_eq!(
        sdk_view.worker.as_ref().map(|worker| worker.id.as_str()),
        Some("worker-a")
    );

    let worker_base = format!("http://{worker_addr}");
    wait_for(
        || {
            client()
                .get(format!("{worker_base}/v1/health"))
                .send()
                .map(|response| response.status().is_success())
                .unwrap_or(false)
        },
        "provisioned worker health",
    );
    let inspected = cli_json(
        &gateway_base,
        ADMIN_TOKEN,
        &["fleet", "inspect", "host-a", "fleet-a", "--json"],
    );
    assert_eq!(inspected["view"]["artifact_digest"], first_digest);
    assert_eq!(inspected["view"]["lifecycle"], "running");
    assert_eq!(inspected["view"]["compatibility"]["compatible"], true);
    assert_eq!(inspected["worker"]["reachable"], true);

    // The provisioned Worker executes on its host without Gateway help.
    let standalone = submit(&worker_base, WORKER_A_TOKEN, None, "standalone-1");
    assert_eq!(standalone["state"], "succeeded");
    assert!(
        standalone["output"]
            .as_str()
            .unwrap_or_default()
            .contains("PROMPT-EXTERNAL-FLEET"),
        "{standalone}"
    );

    // A repeated command replays the same lifecycle operation.
    let repeated = cli_json(
        &gateway_base,
        ADMIN_TOKEN,
        &[
            "fleet",
            "provision",
            "host-a",
            "--operation",
            "op-provision-a",
            "--instance",
            "fleet-a",
            "--worker",
            "worker-a",
            "--release",
            &first_digest,
            "--addr",
            &worker_addr,
            "--config",
            config.to_str().unwrap(),
            "--control-token",
            WORKER_A_TOKEN,
            "--grant",
            "task:submit",
            "--grant",
            "task:read",
            "--grant",
            "task:cancel",
            "--grant",
            "session:write",
            "--grant",
            "profile:read",
            "--grant",
            "profile:write",
            "--profile-name",
            "research",
            "--profile-instruction",
            "PROFILE-FLEET-RESEARCH",
            "--profile-member",
            "general",
            "--profile-capability",
            "general",
            "--profile-grant",
            "task:submit",
            "--profile-create",
            "--json",
        ],
    );
    assert_eq!(repeated["status"], "succeeded");
    assert_eq!(repeated["replayed"], true);

    let (conflict_status, conflict) = api(
        "POST",
        &format!("{gateway_base}/v1/hosts/host-a/instances"),
        ADMIN_TOKEN,
        Some(json!({
            "operation_id": "op-provision-a",
            "instance": "fleet-a",
            "worker": "worker-a",
            "release_digest": second_digest,
            "addr": worker_addr,
        })),
    );
    assert_eq!(conflict_status, 409, "{conflict}");
    assert_eq!(conflict["code"], "host.operation_conflict");

    // Another host cannot administer this instance.
    let (cross_status, cross) = api(
        "POST",
        &format!("{gateway_base}/v1/hosts/host-b/instances/fleet-a/rollback"),
        ADMIN_TOKEN,
        Some(json!({"operation_id": "op-cross"})),
    );
    assert_eq!(cross_status, 403, "{cross}");
    assert_eq!(cross["code"], "host.scope_denied");
    // Host credentials are ordinary Gateway caller tokens; the same token is
    // refused by another Gateway even if a caller learns it.
    let (foreign_status, foreign) =
        api("GET", &format!("{host_b_base}/v1/host"), HOST_A_TOKEN, None);
    assert_eq!(foreign_status, 401, "{foreign}");
    assert_eq!(foreign["code"], "auth.unauthenticated");

    // A caller without the fleet administration scope is refused.
    let (denied_status, denied) = api(
        "GET",
        &format!("{gateway_base}/v1/hosts/host-a/instances/fleet-a"),
        LIMITED_TOKEN,
        None,
    );
    assert_eq!(denied_status, 403, "{denied}");
    assert_eq!(denied["code"], "scope.denied");

    // Arbitrary software cannot be installed: only a verified cache digest is
    // accepted, and the request has no host path field.
    let (missing_status, missing) = api(
        "POST",
        &format!("{gateway_base}/v1/hosts/host-a/instances"),
        ADMIN_TOKEN,
        Some(json!({
            "operation_id": "op-missing-release",
            "instance": "fleet-a",
            "worker": "worker-a",
            "release_digest": format!("sha256:{}", "0".repeat(64)),
            "addr": worker_addr
        })),
    );
    assert_eq!(missing_status, 400, "{missing}");
    assert_eq!(missing["code"], "host.release_unavailable");

    // A configuration reference cannot escape the host's authorized root, and
    // a caller cannot smuggle a host path through the Gateway.
    let outside_config = state.path().join("outside-worker.toml");
    std::fs::write(
        &outside_config,
        "instance = \"fleet-a\"\ntoken = \"t44-worker-a-control-token\"\n",
    )
    .expect("outside config");
    let (escape_status, escape) = api(
        "POST",
        &format!("{gateway_base}/v1/hosts/host-a/instances"),
        ADMIN_TOKEN,
        Some(json!({
            "operation_id": "op-config-escape",
            "instance": "fleet-a",
            "worker": "worker-a",
            "release_digest": first_digest,
            "config": outside_config,
            "control_token": WORKER_A_TOKEN
        })),
    );
    assert_eq!(escape_status, 403, "{escape}");
    assert_eq!(escape["code"], "host.config_forbidden");

    // A composer that does not implement an operation reports it explicitly.
    let (unsupported_status, unsupported) = api(
        "POST",
        &format!("{gateway_base}/v1/hosts/host-b/instances/fleet-b/rollback"),
        ADMIN_TOKEN,
        Some(json!({"operation_id": "op-unsupported"})),
    );
    assert_eq!(unsupported_status, 400, "{unsupported}");
    assert_eq!(unsupported["code"], "host.unsupported");

    // Managed upgrade uses the local verification, permission, drain, backup,
    // and readiness semantics, then registers the restarted Worker again.
    let upgrade = cli_json(
        &gateway_base,
        ADMIN_TOKEN,
        &[
            "fleet",
            "upgrade",
            "host-a",
            "fleet-a",
            "--operation",
            "op-upgrade-a",
            "--release",
            &second_digest,
            "--authorize",
            "exec:command",
            "--work-policy",
            "drain",
            "--json",
        ],
    );
    assert_eq!(upgrade["status"], "succeeded", "{upgrade}");
    assert_eq!(upgrade["view"]["artifact_digest"], second_digest);
    assert_eq!(upgrade["view"]["active_profile"], "research");
    assert_eq!(upgrade["view"]["process_health"], "running");
    assert_eq!(upgrade["worker"]["reachable"], true);
    assert_eq!(upgrade["upgrade"]["current_digest"], first_digest);
    assert_eq!(upgrade["upgrade"]["candidate_digest"], second_digest);
    assert_eq!(
        upgrade["upgrade"]["permissions_added"],
        json!(["exec:command"])
    );
    assert_eq!(upgrade["upgrade"]["config_preserved"], true);
    assert_eq!(
        std::fs::read(&config).expect("config after upgrade"),
        config_before,
        "operator configuration is preserved byte-for-byte"
    );
    assert!(std::fs::read_dir(&config_dir)
        .expect("external assets")
        .next()
        .is_some());

    // Rollback requires a recoverable journal; a completed upgrade is not
    // relabeled as a successful rollback.
    let (no_recovery_status, no_recovery) = api(
        "POST",
        &format!("{gateway_base}/v1/hosts/host-a/instances/fleet-a/rollback"),
        ADMIN_TOKEN,
        Some(json!({"operation_id": "op-rollback-none"})),
    );
    assert_eq!(no_recovery_status, 400, "{no_recovery}");
    assert!(
        matches!(
            no_recovery["code"].as_str(),
            Some("upgrade.no_recovery" | "upgrade.nothing_to_recover")
        ),
        "{no_recovery}"
    );

    // Simulate an activation interrupted after the journal was written. The
    // local host restores the previous binary record and data snapshot.
    let instance_dir = instances.path().join("fleet-a");
    let record: Value =
        serde_json::from_slice(&std::fs::read(instance_dir.join("instance.json")).expect("record"))
            .expect("record JSON");
    let state_json: Value =
        serde_json::from_slice(&std::fs::read(instance_dir.join("state.json")).expect("state"))
            .expect("state JSON");
    let backup = instance_dir.join("test-backup/data");
    copy_tree(
        &PathBuf::from(record["data_dir"].as_str().unwrap()),
        &backup,
    );
    std::fs::write(
        instance_dir.join("upgrade.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": 1,
            "instance": "fleet-a",
            "status": "activating",
            "started_at": 1,
            "updated_at": 1,
            "policy": "drain",
            "authorized_permissions": ["exec:command"],
            "previous_record": record,
            "candidate_record": record,
            "previous_state": state_json,
            "backup": backup,
            "backup_digest": "sha256:test",
            "error": null,
            "binary_restored": false,
            "data_restored": false,
            "quarantine": null,
            "verification": null
        }))
        .unwrap(),
    )
    .expect("journal");
    let rolled_back = cli_json(
        &gateway_base,
        ADMIN_TOKEN,
        &[
            "fleet",
            "rollback",
            "host-a",
            "fleet-a",
            "--operation",
            "op-rollback-a",
            "--json",
        ],
    );
    assert_eq!(rolled_back["status"], "recovered", "{rolled_back}");
    assert_eq!(rolled_back["recovery"]["binary_restored"], true);
    assert_eq!(rolled_back["recovery"]["data_restored"], true);
    assert_eq!(rolled_back["recovery"]["quarantine"], false);
    assert_eq!(rolled_back["view"]["artifact_digest"], second_digest);
    assert_eq!(rolled_back["view"]["lifecycle"], "running");

    // Host loss is explicit, not a false success, and local management remains
    // independent while Gateways are stopped.
    drop(host_a);
    let (lost_status, lost) = api(
        "GET",
        &format!("{gateway_base}/v1/hosts/host-a/instances/fleet-a"),
        ADMIN_TOKEN,
        None,
    );
    assert_eq!(lost_status, 503, "{lost}");
    assert_eq!(lost["code"], "host.unreachable");
    drop(gateway);
    let local = Command::new(binary("gb-instance"))
        .args([
            "inspect",
            "--root",
            instances.path().to_str().unwrap(),
            "--instance",
            "fleet-a",
            "--json",
        ])
        .output()
        .expect("gb-instance runs");
    assert!(
        local.status.success(),
        "{}",
        String::from_utf8_lossy(&local.stderr)
    );
    let local: Value = serde_json::from_slice(&local.stdout).expect("local inspection");
    assert_eq!(local["artifact_digest"], second_digest);
    assert_eq!(local["lifecycle"], "running");
    let standalone = submit(&worker_base, WORKER_A_TOKEN, None, "standalone-2");
    assert_eq!(standalone["state"], "succeeded");

    // Restarting the same host Gateway and root Gateway reconciles the recorded
    // operation instead of creating a second instance or a second approval.
    let (host_a, host_a_base) = spawn_gateway(&host_a_config);
    let mut hosts: Value = json!([
        {
            "id": "host-a",
            "endpoint": host_a_base,
            "token": HOST_A_TOKEN,
            "instances": ["fleet-a"]
        },
        {
            "id": "host-b",
            "endpoint": host_b_base,
            "token": HOST_B_TOKEN,
            "instances": ["fleet-b"]
        }
    ]);
    let restarted = gateway_config(
        state.path(),
        &state.path().join("registry.json"),
        hosts.take(),
    );
    let (_gateway, gateway_base) = spawn_gateway(&restarted);
    wait_for(
        || {
            api(
                "GET",
                &format!("{gateway_base}/v1/health"),
                ADMIN_TOKEN,
                None,
            )
            .0 == 200
        },
        "restarted gateway",
    );
    let reconciled = cli_json(
        &gateway_base,
        ADMIN_TOKEN,
        &[
            "fleet",
            "provision",
            "host-a",
            "--operation",
            "op-provision-a",
            "--instance",
            "fleet-a",
            "--worker",
            "worker-a",
            "--release",
            &first_digest,
            "--addr",
            &worker_addr,
            "--config",
            config.to_str().unwrap(),
            "--control-token",
            WORKER_A_TOKEN,
            "--grant",
            "task:submit",
            "--grant",
            "task:read",
            "--grant",
            "task:cancel",
            "--grant",
            "session:write",
            "--grant",
            "profile:read",
            "--grant",
            "profile:write",
            "--profile-name",
            "research",
            "--profile-instruction",
            "PROFILE-FLEET-RESEARCH",
            "--profile-member",
            "general",
            "--profile-capability",
            "general",
            "--profile-grant",
            "task:submit",
            "--profile-create",
            "--json",
        ],
    );
    assert_eq!(reconciled["operation_id"], "op-provision-a");
    assert_eq!(reconciled["replayed"], true);
    let inventory = api(
        "GET",
        &format!("{gateway_base}/v1/admin/workers"),
        ADMIN_TOKEN,
        None,
    );
    assert_eq!(inventory.0, 200);
    assert_eq!(
        inventory
            .1
            .as_array()
            .expect("inventory")
            .iter()
            .filter(|worker| worker["id"] == "worker-a")
            .count(),
        1
    );
    drop(host_a);
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("backup directory");
    for entry in std::fs::read_dir(source).expect("data directory") {
        let entry = entry.expect("data entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("entry type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("backup file");
        }
    }
}
