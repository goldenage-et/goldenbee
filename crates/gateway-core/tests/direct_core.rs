//! Direct gateway-core control-plane acceptance with a real separate Worker process.
//!
//! The test constructs the `gateway-core` library directly. It supplies an
//! authenticated `Caller` with no caller token or business authentication
//! layer, starts the `general` Worker as its own OS process, and drives
//! submission, projections, retained events, the live event stream, and
//! cancellation through the core API. No Axum adapter and no business identity
//! or payment system is involved.

use common::{GatewaySubmitRequest, Limits, Principal};
use gateway_core::{Caller, CoreConfig, Gateway, WorkerConfig};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const TEST_DELEGATION_SECRET: &str = "direct-core-delegation-secret";
const TEST_INPUT: &str = "direct control plane task";

struct Worker {
    child: Child,
    _data: tempfile::TempDir,
    base: String,
    instance: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Resolve a workspace sibling binary. Cargo only defines
/// `CARGO_BIN_EXE_*` for the package under test, so walk the target directory.
fn binary(name: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var(format!("CARGO_BIN_EXE_{name}")) {
        return Some(PathBuf::from(path));
    }
    let current = std::env::current_exe().ok()?;
    let mut directory = current.parent();
    while let Some(candidate_directory) = directory {
        let candidate = candidate_directory.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        directory = candidate_directory.parent();
    }
    None
}

fn worker_config(data: &Path) -> PathBuf {
    let agents = data.join("assets/agents");
    let prompts = data.join("assets/prompts");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::write(
        prompts.join("general.md"),
        "You are the Goldenbee General Worker. Answer usefully and concisely.",
    )
    .expect("general prompt");
    std::fs::write(
        agents.join("general.toml"),
        "name = \"general\"\ndescription = \"General Worker\"\nmodel = \"deterministic\"\nprompt = \"general\"\ntools = [\"exec_command\"]\n",
    )
    .expect("general agent");
    let path = data.join("worker.toml");
    std::fs::write(
        &path,
        format!(
            "instance = \"direct-core\"\nnamespace = \"local\"\ndefault_agent = \"general\"\ntoken = \"direct-core-token\"\ntoken_scopes = [\"*\"]\n\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n",
            data.display()
        ),
    )
    .expect("worker config");
    path
}

fn start_worker() -> Option<Worker> {
    let executable = binary("general")?;
    let data = tempfile::tempdir().expect("worker data directory");
    let config = worker_config(data.path());
    let mut child = Command::new(executable)
        .args([
            "serve",
            "--config",
            config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .env("GOLDENBEE_DELEGATION_SECRET", TEST_DELEGATION_SECRET)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("general worker starts");
    let stdout = child.stdout.take().expect("worker stdout is piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("worker announces its listener");
    let base = line
        .split("listening on ")
        .nth(1)
        .unwrap_or_else(|| panic!("worker address announcement: {line}"))
        .trim()
        .to_string();
    let instance = reqwest::blocking::Client::new()
        .get(format!("{base}/v1/health"))
        .send()
        .expect("worker health responds")
        .json::<Value>()
        .expect("worker health is JSON")["instance"]
        .as_str()
        .expect("worker instance identity")
        .to_string();
    Some(Worker {
        child,
        _data: data,
        base,
        instance,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_core_drives_a_separate_worker_without_a_caller_token() {
    let Some(mut worker) = tokio::task::spawn_blocking(start_worker)
        .await
        .expect("fixture task")
    else {
        eprintln!("skipping direct-core acceptance: the general binary is not built");
        return;
    };
    assert!(
        worker.child.id() > 0,
        "the Worker must be a separate OS process"
    );

    // Core configuration has no caller tokens and no listener address.
    std::env::set_var("GOLDENBEE_DELEGATION_SECRET", TEST_DELEGATION_SECRET);
    let state = tempfile::tempdir().expect("core state directory");
    let config = CoreConfig {
        data_dir: state.path().join("dispatch").display().to_string(),
        workers: vec![WorkerConfig {
            id: "direct-core".into(),
            endpoint: worker.base.clone(),
            instance: Some(worker.instance.clone()),
            package: Some("general".into()),
            capabilities: None,
            control_token: None,
            preflight: None,
        }],
        hosts: Vec::new(),
        registry: Some(state.path().join("registry.json").display().to_string()),
        lease_ttl_ms: Some(60_000),
        max_streams: 256,
        claim_ttl_ms: None,
        replica_id: None,
        store: None,
        vault: None,
        delegation_secret: None,
        base_dir: None,
    };
    config.validate().expect("core configuration is valid");

    // Startup preflight verifies the approved Worker identity.
    let core = Gateway::prepare(config).await.expect("core prepares");
    assert_eq!(core.worker_summary(), (1, 1));

    // The business backend authenticates its own user and supplies a
    // principal directly. No caller token or business system is consulted.
    let caller = Caller::new(Principal::new(
        "business-user",
        "local",
        &["task:submit", "task:read", "task:cancel"],
    ))
    .with_worker_allowlist(["direct-core"]);

    let accepted = core
        .submit(
            &caller,
            GatewaySubmitRequest {
                worker: Some("direct-core".into()),
                agent: "general".into(),
                session_id: "business-session".into(),
                input: TEST_INPUT.into(),
                idempotency_key: "business-key".into(),
                profile: None,
                team: None,
                team_version: None,
                limits: Limits::default(),
            },
        )
        .await
        .expect("direct-core submission is accepted");
    assert_eq!(accepted.worker, "direct-core");
    assert_eq!(accepted.worker_instance, worker.instance);
    assert!(accepted.task_id.starts_with("exec-"));

    let mut last = None;
    for _ in 0..300 {
        let projection = core
            .task(&caller, &accepted.task_id)
            .await
            .expect("direct-core task projection responds");
        let terminal = projection.task.state.is_terminal();
        last = Some(projection);
        if terminal {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let projection = last.expect("task projection was observed");
    assert_eq!(projection.task.state, common::TaskState::Succeeded);
    assert!(projection
        .task
        .output
        .as_deref()
        .unwrap_or_default()
        .contains(TEST_INPUT));

    let page = core
        .events(&caller, &accepted.task_id, 0, 100)
        .await
        .expect("retained events respond");
    assert!(!page.events.is_empty(), "retained Worker events exist");

    // The live stream is a transport-neutral byte stream. Reading one chunk
    // proves the replay-then-live Worker SSE proxy through the core.
    let mut stream = core
        .stream(&caller, &accepted.task_id, 0)
        .await
        .expect("direct-core stream opens");
    let chunk = tokio::time::timeout(
        Duration::from_secs(10),
        futures::StreamExt::next(&mut stream),
    )
    .await
    .expect("the direct-core stream produced a chunk before the timeout")
    .expect("the direct-core stream is not empty")
    .expect("the direct-core stream chunk is valid");
    assert!(
        String::from_utf8_lossy(&chunk).contains("event:"),
        "the core stream carries the Worker Server-Sent Event frames"
    );

    // The Worker remains a separate process for the whole session.
    assert!(
        worker
            .child
            .try_wait()
            .expect("worker status is observable")
            .is_none(),
        "the Worker process must remain separate from the business backend"
    );

    let cancelled = core
        .cancel(&caller, &accepted.task_id)
        .await
        .expect("direct-core cancellation responds");
    assert!(matches!(
        cancelled.task.state,
        common::TaskState::Succeeded | common::TaskState::Cancelled
    ));
}

#[test]
fn postgres_driver_is_feature_gated() {
    let manifest = include_str!("../Cargo.toml");
    assert!(
        manifest.contains("optional = true"),
        "the Postgres driver must be an optional dependency"
    );
    assert!(
        manifest.contains("postgres = [\"dep:sqlx\"]"),
        "the postgres feature must enable the optional driver"
    );
    assert!(
        !manifest.contains("axum"),
        "gateway-core must not gain a transport adapter"
    );
}

#[test]
fn core_manifest_has_no_adapter_transport_or_business_edges() {
    let manifest = include_str!("../Cargo.toml");
    for forbidden in [
        "axum",
        "futures-util-extra",
        "goldenbee-gateway",
        "runtime",
        "general",
        "cli",
        "sdk",
        "stripe",
        "payment",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "gateway-core must not depend on '{forbidden}'"
        );
    }
}
