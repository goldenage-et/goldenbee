//! Gateway-owned package install and local instance lifecycle.

use base64::Engine;
use common::{bytes_digest, ReleaseMetadata};
use ed25519_dalek::{Signer, SigningKey};
use install::{encode_metadata, seed_assets, write_layout_with_seeds, SeedFile, TrustPolicy};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const DELEGATION_SECRET: &str = "t44-local-delegation-secret";
const ADMIN_TOKEN: &str = "t44-local-admin-token";
const LIMITED_TOKEN: &str = "t44-local-limited-token";
const WORKER_TOKEN: &str = "t44-local-worker-token";

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[13_u8; 32])
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

fn artifact(releases: &Path, prompt: &str, revision: &str) -> (String, PathBuf) {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_general")).expect("general binary");
    let source = serde_json::to_vec(&serde_json::json!({
        "revision": revision,
        "tree": bytes_digest(format!("general source tree {revision}").as_bytes()),
    }))
    .unwrap();
    let seeds = seed_assets_for(prompt);
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
    let directory = releases.join(format!("general-{revision}"));
    std::fs::create_dir_all(&directory).expect("artifact directory");
    let digests =
        write_layout_with_seeds(&directory, &metadata, &binary, &source, &seeds, &signature)
            .expect("layout");
    (digests.artifact_digest, directory)
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
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("gateway starts");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("gateway announces its listener");
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
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "{what} did not become ready");
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn gateway_config(directory: &Path, releases: &Path, trust: &Path) -> PathBuf {
    let path = directory.join("gateway.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "listen": "127.0.0.1:0",
            "data_dir": directory.join("state"),
            "registry": directory.join("registry.json"),
            "delegation_secret": DELEGATION_SECRET,
            "host_id": "local",
            "instances_root": directory.join("instances"),
            "packages_root": directory.join("packages"),
            "trust_policy": trust,
            "allowed_package_sources": [releases],
            "operations": [
                "provision", "inspect", "upgrade", "rollback", "start", "stop", "restart"
            ],
            "allowed_grants": [
                "task:submit", "task:read", "task:cancel", "session:write",
                "profile:read", "profile:write"
            ],
            "allowed_profile_grants": ["task:submit", "task:read", "profile:read"],
            "allowed_config_roots": [],
            "allowed_workspace_roots": [],
            "allowed_secret_prefixes": ["secret://"],
            "allow_remote_listener": false,
            "callers": [
                {
                    "token": ADMIN_TOKEN,
                    "id": "local-admin",
                    "namespace": "local",
                    "scopes": ["*"]
                },
                {
                    "token": LIMITED_TOKEN,
                    "id": "limited",
                    "namespace": "local",
                    "scopes": ["task:read"]
                }
            ]
        }))
        .unwrap(),
    )
    .expect("gateway config");
    path
}

#[test]
fn gateway_installs_packages_and_manages_local_instances() {
    let root = tempfile::tempdir().expect("root");
    let releases = root.path().join("releases");
    let configs = root.path().join("config");
    std::fs::create_dir_all(&releases).expect("releases");
    std::fs::create_dir_all(&configs).expect("configs");
    let (digest, artifact) = artifact(&releases, "PROMPT-LOCAL-HOST", "git:local-host");
    let manifest = general::manifest();
    let trust = configs.join("trust.json");
    std::fs::write(
        &trust,
        serde_json::to_vec_pretty(&policy(&manifest)).expect("policy json"),
    )
    .expect("trust policy");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&trust, std::fs::Permissions::from_mode(0o600))
            .expect("trust mode");
    }
    let config = gateway_config(root.path(), &releases, &trust);
    let (_gateway, base) = spawn_gateway(&config);
    wait_for(
        || api("GET", &format!("{base}/v1/health"), ADMIN_TOKEN, None).0 == 200,
        "gateway",
    );

    let (status, installed) = api(
        "POST",
        &format!("{base}/v1/packages/install"),
        ADMIN_TOKEN,
        Some(json!({
            "source": {"kind": "folder", "path": artifact},
            "package": "general",
            "expected_digest": digest
        })),
    );
    assert_eq!(status, 200, "{installed}");
    assert_eq!(installed["package"]["artifact_digest"], digest);
    assert_eq!(installed["package"]["package"], "general");
    assert_eq!(installed["installed_now"], true);

    let (status, packages) = api("GET", &format!("{base}/v1/packages"), ADMIN_TOKEN, None);
    assert_eq!(status, 200, "{packages}");
    assert_eq!(packages["packages"][0]["artifact_digest"], digest);

    let (status, mismatch) = api(
        "POST",
        &format!("{base}/v1/packages/install"),
        ADMIN_TOKEN,
        Some(json!({
            "source": {"kind": "folder", "path": artifact},
            "package": "general",
            "expected_digest": format!("sha256:{}", "0".repeat(64))
        })),
    );
    assert_eq!(status, 400, "{mismatch}");
    assert_eq!(mismatch["code"], "package.digest_mismatch");

    let archive_dir = root.path().join("archive");
    std::fs::create_dir_all(&archive_dir).expect("archive dir");
    let archive = archive_dir.join("release.tar.gz");
    let packed = Command::new("tar")
        .args([
            "-czf",
            archive.to_str().expect("archive path"),
            "-C",
            artifact.to_str().expect("artifact path"),
            ".",
        ])
        .status()
        .expect("tar runs");
    assert!(packed.success(), "artifact archive");
    let port = free_port();
    let python = Command::new("python3")
        .args([
            "-m",
            "http.server",
            &port.to_string(),
            "--bind",
            "127.0.0.1",
            "--directory",
            archive_dir.to_str().expect("archive directory"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("archive server");
    let _server = Process { child: python };
    wait_for(
        || TcpStream::connect(("127.0.0.1", port)).is_ok(),
        "archive server",
    );
    let (status, url_installed) = api(
        "POST",
        &format!("{base}/v1/packages/install"),
        ADMIN_TOKEN,
        Some(json!({
            "source": {
                "kind": "url",
                "url": format!("http://127.0.0.1:{port}/release.tar.gz")
            },
            "package": "general",
            "expected_digest": digest
        })),
    );
    assert_eq!(status, 200, "{url_installed}");
    assert_eq!(url_installed["package"]["artifact_digest"], digest);
    assert_eq!(url_installed["installed_now"], false);

    let worker_addr = format!("127.0.0.1:{}", free_port());
    let (status, provisioned) = api(
        "POST",
        &format!("{base}/v1/instances"),
        ADMIN_TOKEN,
        Some(json!({
            "operation_id": "op-local-provision",
            "instance": "general-a",
            "worker": "worker-local-a",
            "release_digest": digest,
            "addr": worker_addr,
            "control_token": WORKER_TOKEN,
            "grants": [
                "task:submit", "task:read", "task:cancel", "session:write",
                "profile:read", "profile:write"
            ]
        })),
    );
    assert_eq!(status, 200, "{provisioned}");
    assert_eq!(provisioned["status"], "succeeded");
    assert_eq!(provisioned["view"]["lifecycle"], "running");
    assert_eq!(provisioned["view"]["process_health"], "running");
    assert_eq!(provisioned["worker"]["id"], "worker-local-a");
    assert_eq!(provisioned["worker"]["reachable"], true);

    let (status, instances) = api("GET", &format!("{base}/v1/instances"), ADMIN_TOKEN, None);
    assert_eq!(status, 200, "{instances}");
    assert_eq!(instances[0]["instance"], "general-a");
    assert_eq!(instances[0]["artifact_digest"], digest);

    let (status, denied) = api("GET", &format!("{base}/v1/instances"), LIMITED_TOKEN, None);
    assert_eq!(status, 403, "{denied}");
    assert_eq!(denied["code"], "scope.denied");

    let (status, stopped) = api(
        "POST",
        &format!("{base}/v1/instances/general-a/stop"),
        ADMIN_TOKEN,
        None,
    );
    assert_eq!(status, 200, "{stopped}");
    assert_eq!(stopped["view"]["lifecycle"], "stopped");
    assert_eq!(stopped["view"]["process_health"], "exited");

    let (status, started) = api(
        "POST",
        &format!("{base}/v1/instances/general-a/start"),
        ADMIN_TOKEN,
        None,
    );
    assert_eq!(status, 200, "{started}");
    assert_eq!(started["view"]["process_health"], "running");
    assert_eq!(started["worker"]["reachable"], true);

    let (status, restarted) = api(
        "POST",
        &format!("{base}/v1/instances/general-a/restart"),
        ADMIN_TOKEN,
        None,
    );
    assert_eq!(status, 200, "{restarted}");
    assert_eq!(restarted["view"]["process_health"], "running");
    assert_eq!(restarted["worker"]["reachable"], true);

    let (status, logs) = api(
        "GET",
        &format!("{base}/v1/instances/general-a/logs?tail=20"),
        ADMIN_TOKEN,
        None,
    );
    assert_eq!(status, 200, "{logs}");
    assert_eq!(logs["instance"], "general-a");
    assert!(logs["lines"].is_array());

    let (status, inspected) = api(
        "GET",
        &format!("{base}/v1/instances/general-a"),
        ADMIN_TOKEN,
        None,
    );
    assert_eq!(status, 200, "{inspected}");
    assert_eq!(inspected["view"]["artifact_digest"], digest);
    assert_eq!(inspected["worker"]["reachable"], true);
}
