//! Public management journey through the real CLI, SDK, Gateway, and Workers.
//!
//! The test observes only external commands and wire JSON. It proves the CLI
//! reuses the SDK for Worker discovery, team, profile, and approval semantics,
//! preserves revision conflicts, and produces the same resource identities as a
//! separately constructed SDK client.

#[path = "../../goldenbee-gateway/tests/support/mod.rs"]
mod support;

use serde_json::Value;
use std::thread::sleep;
use std::time::Duration;
use support::{is_terminal, Stack, ADMIN_TOKEN, GATEWAY_TOKEN, READONLY_TOKEN};

fn stdout_json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn success(stack: &Stack, token: &str, arguments: &[&str]) -> Value {
    let output = stack.cli_as(token, arguments);
    assert!(
        output.status.success(),
        "cli {arguments:?} failed: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains(GATEWAY_TOKEN),
        "successful output echoed the caller token"
    );
    stdout_json(&output)
}

fn failure(stack: &Stack, token: &str, code: i32, needle: &str, arguments: &[&str]) -> String {
    let output = stack.cli_as(token, arguments);
    assert_eq!(
        output.status.code(),
        Some(code),
        "cli {arguments:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "failure wrote to stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains(needle), "{stderr}");
    assert!(!stderr.contains(GATEWAY_TOKEN), "{stderr}");
    assert!(!stderr.contains(ADMIN_TOKEN), "{stderr}");
    stderr
}

#[test]
fn sdk_client_completes_team_and_profile_management_journey() {
    let stack = Stack::start();
    let client = sdk::Client::new(&stack.gateway_base, ADMIN_TOKEN);
    let readonly = sdk::Client::new(&stack.gateway_base, READONLY_TOKEN);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let members = vec![
        common::TeamMember {
            worker: "worker-a".into(),
            role: "master".into(),
            capabilities: vec!["general".into()],
            max_depth: 0,
            instance: None,
        },
        common::TeamMember {
            worker: "worker-b".into(),
            role: "specialist".into(),
            capabilities: vec!["general".into()],
            max_depth: 0,
            instance: None,
        },
    ];
    let spec = common::TeamSpec {
        master: "worker-a".into(),
        members: members.clone(),
        limits: common::Limits {
            max_depth: 2,
            max_fanout: 2,
            ..common::Limits::default()
        },
        locality: None,
    };

    let created = runtime
        .block_on(client.create_team(&common::CreateTeamRequest {
            name: "sdk-team".into(),
            team: spec.clone(),
        }))
        .expect("SDK team create");
    assert_eq!(created.version, 1);
    assert!(!created.active);

    let revised = runtime
        .block_on(client.update_team(
            "sdk-team",
            &common::UpdateTeamRequest {
                expected_version: 1,
                team: spec.clone(),
            },
        ))
        .expect("SDK team revise");
    assert_eq!(revised.version, 2);

    let stale = runtime
        .block_on(client.update_team(
            "sdk-team",
            &common::UpdateTeamRequest {
                expected_version: 1,
                team: spec.clone(),
            },
        ))
        .expect_err("stale SDK team revise");
    assert_eq!(stale.code, "team.version_conflict");

    let validated = runtime
        .block_on(
            client.validate_team("sdk-team", &common::TeamVersionRequest { version: Some(2) }),
        )
        .expect("SDK team validate");
    assert!(!validated.active);
    let activated = runtime
        .block_on(
            client.activate_team("sdk-team", &common::TeamVersionRequest { version: Some(2) }),
        )
        .expect("SDK team activate");
    assert!(activated.active);
    let teams = runtime.block_on(client.teams()).expect("SDK team list");
    assert!(teams
        .iter()
        .any(|team| team.name == "sdk-team" && team.version == 2 && team.active));

    let revision = common::ProfileRevision {
        revision: 0,
        instruction: "SDK-PROFILE-ONE".into(),
        model: "deterministic".into(),
        capabilities: vec!["general".into()],
        grants: vec!["task:submit".into()],
        restrictions: Vec::new(),
        composition: "single".into(),
        members: vec!["general".into()],
        skills: Vec::new(),
        mcp: Vec::new(),
        workflow: None,
        limits: common::Limits::default(),
        compaction: None,
        secret_refs: Vec::new(),
        created_at: 0,
    };
    let profile = runtime
        .block_on(client.create_profile(
            "worker-a",
            &common::CreateProfileRequest {
                name: "sdk-research".into(),
                revision: revision.clone(),
            },
        ))
        .expect("SDK profile create");
    assert_eq!(profile.active_revision, 0);
    assert_eq!(profile.revisions[0].revision, 1);

    let revised = runtime
        .block_on(client.revise_profile(
            "worker-a",
            "sdk-research",
            &common::ReviseProfileRequest {
                expected_revision: 1,
                revision: common::ProfileRevision {
                    instruction: "SDK-PROFILE-TWO".into(),
                    ..revision.clone()
                },
            },
        ))
        .expect("SDK profile revise");
    assert_eq!(revised.revisions.len(), 2);
    assert_eq!(revised.active_revision, 0);

    let stale = runtime
        .block_on(client.revise_profile(
            "worker-a",
            "sdk-research",
            &common::ReviseProfileRequest {
                expected_revision: 1,
                revision: revision.clone(),
            },
        ))
        .expect_err("stale SDK profile revise");
    assert_eq!(stale.code, "profile.revision_conflict");

    let validated = runtime
        .block_on(client.validate_profile(
            "worker-a",
            "sdk-research",
            &common::ProfileVersionRequest { revision: Some(2) },
        ))
        .expect("SDK profile validate");
    assert_eq!(validated.active_revision, 0);
    let activated = runtime
        .block_on(client.activate_profile(
            "worker-a",
            "sdk-research",
            &common::ProfileVersionRequest { revision: Some(2) },
        ))
        .expect("SDK profile activate");
    assert_eq!(activated.active_revision, 2);
    let observed = runtime
        .block_on(client.profile("worker-a", "sdk-research"))
        .expect("SDK profile read");
    assert_eq!(observed.active_revision, 2);

    let denied_team = runtime
        .block_on(readonly.create_team(&common::CreateTeamRequest {
            name: "sdk-denied".into(),
            team: spec,
        }))
        .expect_err("read-only SDK team create");
    assert_eq!(denied_team.code, "scope.denied");
    let denied_profile = runtime
        .block_on(readonly.revise_profile(
            "worker-a",
            "sdk-research",
            &common::ReviseProfileRequest {
                expected_revision: 2,
                revision,
            },
        ))
        .expect_err("read-only SDK profile revise");
    assert_eq!(denied_profile.code, "scope.denied");

    let submitted = runtime
        .block_on(client.submit(&common::GatewaySubmitRequest {
            worker: Some("worker-a".into()),
            agent: "general".into(),
            session_id: "sdk-management-session".into(),
            input: "prompt:".into(),
            idempotency_key: "sdk-management-task".into(),
            profile: Some("sdk-research".into()),
            team: None,
            team_version: None,
            limits: common::Limits::default(),
        }))
        .expect("SDK profile submit");
    let projection = stack.wait_task(ADMIN_TOKEN, &submitted.task_id);
    assert_eq!(projection["task"]["state"], "succeeded");
    assert_eq!(projection["task"]["profile_revision"], 2);
    assert!(projection["task"]["output"]
        .as_str()
        .unwrap_or_default()
        .contains("SDK-PROFILE-TWO"));
}

#[test]
fn cli_and_sdk_share_worker_team_profile_and_approval_management_semantics() {
    let stack = Stack::start();

    // Worker discovery and inspection stay on the SDK-backed Gateway routes.
    let workers = success(&stack, GATEWAY_TOKEN, &["workers", "--json"]);
    assert!(workers
        .as_array()
        .is_some_and(|workers| workers.iter().any(|worker| worker["id"] == "worker-a")));
    let inspected = success(
        &stack,
        ADMIN_TOKEN,
        &["worker", "show", "worker-a", "--json"],
    );
    assert_eq!(inspected["id"], "worker-a");
    assert_eq!(inspected["enabled"], true);

    // Create a versioned team. Creation is inactive until an explicit
    // validation and activation.
    let created = success(
        &stack,
        ADMIN_TOKEN,
        &[
            "team",
            "create",
            "alpha",
            "--master",
            "worker-a",
            "--member",
            "worker-a:general",
            "--member",
            "worker-b:general",
            "--max-depth",
            "2",
            "--max-fanout",
            "2",
            "--json",
        ],
    );
    assert_eq!(created["name"], "alpha");
    assert_eq!(created["version"], 1);
    assert_eq!(created["master"], "worker-a");
    assert_eq!(created["active"], false);
    assert_eq!(created["members"].as_array().unwrap().len(), 2);

    let human = stack.cli_as(ADMIN_TOKEN, &["team", "show", "alpha"]);
    assert!(human.status.success());
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(human.contains("team=alpha"), "{human}");
    assert!(human.contains("version=1"), "{human}");
    assert!(human.contains("master=worker-a"), "{human}");
    assert!(human.contains("active=false"), "{human}");

    let validated = success(
        &stack,
        ADMIN_TOKEN,
        &["team", "validate", "alpha", "--version", "1", "--json"],
    );
    assert_eq!(validated["active"], false);
    let activated = success(
        &stack,
        ADMIN_TOKEN,
        &["team", "activate", "alpha", "--version", "1", "--json"],
    );
    assert_eq!(activated["active"], true);

    // Revising observes version 1 and produces version 2.
    let revised = success(
        &stack,
        ADMIN_TOKEN,
        &[
            "team",
            "revise",
            "alpha",
            "--version",
            "1",
            "--master",
            "worker-a",
            "--member",
            "worker-a:general",
            "--member",
            "worker-b:general",
            "--max-depth",
            "3",
            "--max-fanout",
            "2",
            "--json",
        ],
    );
    assert_eq!(revised["version"], 2);
    assert_eq!(revised["active"], true);

    // A stale writer is rejected with a distinct validation exit code.
    failure(
        &stack,
        ADMIN_TOKEN,
        3,
        "team.version_conflict",
        &[
            "team",
            "revise",
            "alpha",
            "--version",
            "1",
            "--master",
            "worker-a",
            "--member",
            "worker-a:general",
            "--member",
            "worker-b:general",
            "--json",
        ],
    );

    // A caller without team:write cannot create another roster.
    failure(
        &stack,
        READONLY_TOKEN,
        4,
        "scope.denied",
        &[
            "team",
            "create",
            "denied",
            "--master",
            "worker-a",
            "--member",
            "worker-a:general",
            "--json",
        ],
    );

    // Create and revise a Runtime Profile on its owning Worker. The selected
    // agent member is loaded from the Worker's file assets, so validation uses
    // the same resolved revisions as native execution.
    let canary = "sk-cli-profile-canary";
    let profile_created = success(
        &stack,
        ADMIN_TOKEN,
        &[
            "profile",
            "create",
            "research",
            "--worker",
            "worker-a",
            "--instruction",
            &format!("PROFILE-ONE {canary}"),
            "--model",
            "deterministic",
            "--composition",
            "single",
            "--member",
            "general",
            "--capability",
            "general",
            "--grant",
            "task:submit",
            "--json",
        ],
    );
    assert_eq!(profile_created["name"], "research");
    assert_eq!(profile_created["active_revision"], 0);
    assert_eq!(profile_created["revisions"][0]["revision"], 1);
    assert!(
        !serde_json::to_string(&profile_created)
            .unwrap()
            .contains(canary),
        "profile JSON exposed a secret canary"
    );

    success(
        &stack,
        ADMIN_TOKEN,
        &[
            "profile",
            "revise",
            "research",
            "--worker",
            "worker-a",
            "--expected",
            "1",
            "--instruction",
            "PROFILE-TWO",
            "--model",
            "deterministic",
            "--composition",
            "single",
            "--member",
            "general",
            "--capability",
            "general",
            "--grant",
            "task:submit",
            "--json",
        ],
    );

    let validated = success(
        &stack,
        ADMIN_TOKEN,
        &[
            "profile",
            "validate",
            "research",
            "--worker",
            "worker-a",
            "--revision",
            "2",
            "--json",
        ],
    );
    assert_eq!(validated["active_revision"], 0);

    let activated = success(
        &stack,
        ADMIN_TOKEN,
        &[
            "profile",
            "activate",
            "research",
            "--worker",
            "worker-a",
            "--revision",
            "2",
            "--json",
        ],
    );
    assert_eq!(activated["active_revision"], 2);

    let shown = success(
        &stack,
        ADMIN_TOKEN,
        &[
            "profile", "show", "research", "--worker", "worker-a", "--json",
        ],
    );
    assert_eq!(shown["active_revision"], 2);
    assert!(
        !serde_json::to_string(&shown).unwrap().contains(canary),
        "profile inspection exposed a secret canary"
    );

    // Stale profile revisions are rejected without changing the active one.
    failure(
        &stack,
        ADMIN_TOKEN,
        3,
        "profile.revision_conflict",
        &[
            "profile",
            "revise",
            "research",
            "--worker",
            "worker-a",
            "--expected",
            "1",
            "--instruction",
            "STALE",
            "--json",
        ],
    );
    let shown = success(
        &stack,
        ADMIN_TOKEN,
        &[
            "profile", "show", "research", "--worker", "worker-a", "--json",
        ],
    );
    assert_eq!(shown["active_revision"], 2);

    // A caller without profile:write cannot modify the owning Worker.
    failure(
        &stack,
        READONLY_TOKEN,
        4,
        "scope.denied",
        &[
            "profile",
            "revise",
            "research",
            "--worker",
            "worker-a",
            "--expected",
            "2",
            "--instruction",
            "DENIED",
            "--json",
        ],
    );

    // The activated revision is the one native execution pins. Running the
    // file-defined general agent observes revision 2 and the revised
    // instruction.
    let submitted = success(
        &stack,
        ADMIN_TOKEN,
        &[
            "submit",
            "--worker",
            "worker-a",
            "--agent",
            "general",
            "--profile",
            "research",
            "--input",
            "prompt:",
            "--session",
            "management-profile-session",
            "--key",
            "management-profile-task",
            "--json",
        ],
    );
    let task_id = submitted["task_id"].as_str().expect("task id");
    let mut projection = Value::Null;
    for _ in 0..300 {
        projection = success(&stack, ADMIN_TOKEN, &["task", task_id, "--json"]);
        if is_terminal(&projection["task"]["state"]) {
            break;
        }
        sleep(Duration::from_millis(20));
    }
    assert_eq!(projection["task"]["state"], "succeeded");
    assert_eq!(projection["task"]["profile_revision"], 2);
    assert!(projection["task"]["output"]
        .as_str()
        .unwrap_or_default()
        .contains("PROFILE-TWO"));

    // A separately constructed SDK client observes the same identities,
    // revisions, and active state as the CLI presentation.
    let client = sdk::Client::new(&stack.gateway_base, ADMIN_TOKEN);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (teams, profile, profiles) = runtime.block_on(async {
        (
            client.teams().await.expect("SDK team list"),
            client
                .profile("worker-a", "research")
                .await
                .expect("SDK profile"),
            client.profiles("worker-a").await.expect("SDK profile list"),
        )
    });
    drop(runtime);
    assert_eq!(teams[0].name, "alpha");
    assert_eq!(teams[0].version, 2);
    assert!(teams[0].active);
    assert_eq!(profile.name, "research");
    assert_eq!(profile.active_revision, 2);
    assert!(profiles.iter().any(|profile| profile.name == "research"));

    // Both clients expose the same team identity through a stable SDK read.
    let listed = success(&stack, ADMIN_TOKEN, &["team", "list", "--json"]);
    assert_eq!(listed[0]["version"], teams[0].version);
    assert_eq!(listed[0]["active"], teams[0].active);
}
