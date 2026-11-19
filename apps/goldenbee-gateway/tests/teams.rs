//! Cross-process team roster admission, routing, version freeze, and child
//! authority evidence. One Master and two independently built Worker Packages
//! are real processes reached through Gateway and native A2A.

mod support;

use serde_json::{json, Value};
use support::{Options, Stack, ADMIN_TOKEN, WORKER_A_TOKEN};

fn post(stack: &Stack, token: &str, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
    let response = Stack::client()
        .post(format!("{}{path}", stack.gateway_base))
        .bearer_auth(token)
        .json(&body)
        .send()
        .expect("POST responds");
    let status = response.status();
    let body = response.text().unwrap_or_default();
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body})),
    )
}

fn put(stack: &Stack, token: &str, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
    let response = Stack::client()
        .put(format!("{}{path}", stack.gateway_base))
        .bearer_auth(token)
        .json(&body)
        .send()
        .expect("PUT responds");
    let status = response.status();
    let body = response.text().unwrap_or_default();
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body})),
    )
}

fn team_body(master: &str, members: Value, max_depth: u32, max_fanout: u32) -> Value {
    json!({
        "master": master,
        "members": members,
        "limits": {"max_depth": max_depth, "max_fanout": max_fanout}
    })
}

fn member(worker: &str, capability: &str, max_depth: u32) -> Value {
    json!({
        "worker": worker,
        "role": if worker == "worker-a" { "master" } else { "specialist" },
        "capabilities": [capability],
        "max_depth": max_depth
    })
}

fn create_team(stack: &Stack, name: &str, body: Value) -> Value {
    let (status, team) = post(
        stack,
        ADMIN_TOKEN,
        "/v1/teams",
        json!({"name": name, "master": body["master"], "members": body["members"], "limits": body["limits"]}),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{team}");
    team
}

fn activate_team(stack: &Stack, name: &str, version: u64) -> Value {
    let (status, team) = post(
        stack,
        ADMIN_TOKEN,
        &format!("/v1/teams/{name}/activate"),
        json!({"version": version}),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{team}");
    team
}

#[test]
fn team_routes_to_master_with_immutable_version_and_native_execution() {
    let stack = Stack::start_with_options(Options {
        with_echo_worker: true,
        ..Options::default()
    });
    let members = json!([
        member("worker-a", "general", 2),
        member("worker-b", "general", 2),
        member("worker-c", "echo", 2)
    ]);
    let created = create_team(
        &stack,
        "alpha",
        team_body("worker-a", members.clone(), 2, 3),
    );
    assert_eq!(created["version"], 1);
    assert_eq!(created["active"], false);
    let activated = activate_team(&stack, "alpha", 1);
    assert_eq!(activated["active"], true);

    let (status, first) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/tasks",
        json!({
            "team": "alpha",
            "team_version": 1,
            "agent": "general",
            "session_id": "team-session-1",
            "input": "coordinate the team",
            "idempotency_key": "team-1"
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{first}");
    assert_eq!(first["worker"], "worker-a");
    assert_eq!(first["team_version"], 1);
    let execution = first["task_id"].as_str().expect("execution id");
    let projection = stack.wait_task(ADMIN_TOKEN, execution);
    assert_eq!(projection["worker"], "worker-a");
    assert_eq!(projection["task"]["team"], "alpha");
    assert_eq!(projection["task"]["team_version"], 1);
    assert_eq!(projection["task"]["team_roster"]["version"], 1);
    assert_eq!(
        projection["task"]["delegation_path"],
        json!([stack.worker_a.instance])
    );
    assert_eq!(projection["task"]["agent"], "general");

    // A later roster update affects future tasks, not the already routed child.
    let updated = put(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams/alpha",
        json!({
            "expected_version": 1,
            "master": "worker-a",
            "members": members,
            "limits": {"max_depth": 3, "max_fanout": 3}
        }),
    );
    assert_eq!(updated.0, reqwest::StatusCode::OK, "{}", updated.1);
    assert_eq!(updated.1["version"], 2);
    activate_team(&stack, "alpha", 2);

    let frozen = stack.task(ADMIN_TOKEN, execution);
    assert_eq!(frozen["worker"], "worker-a");
    assert_eq!(frozen["task"]["team_version"], 1);
    assert_eq!(frozen["task"]["team_roster"]["version"], 1);

    // A stale explicit version is rejected instead of silently selecting the
    // newest roster.
    let (status, stale) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/tasks",
        json!({
            "team": "alpha",
            "team_version": 1,
            "agent": "general",
            "session_id": "team-session-stale",
            "input": "stale roster",
            "idempotency_key": "team-stale"
        }),
    );
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["code"], "team.version_conflict");

    // Deactivation stops new admission but leaves the frozen execution intact.
    let (status, _) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams/alpha/deactivate",
        json!({"version": 2}),
    );
    assert_eq!(status, reqwest::StatusCode::OK);
    let (status, inactive) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/tasks",
        json!({
            "team": "alpha",
            "agent": "general",
            "session_id": "team-session-inactive",
            "input": "inactive roster",
            "idempotency_key": "team-inactive"
        }),
    );
    assert!(!status.is_success(), "{inactive}");
    assert_eq!(inactive["code"], "team.inactive");
    assert_eq!(
        stack.task(ADMIN_TOKEN, execution)["task"]["team_version"],
        1
    );

    // A retry of the accepted idempotency key replays the frozen execution
    // instead of selecting the changed or inactive roster.
    let (status, replay) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/tasks",
        json!({
            "team": "alpha",
            "team_version": 1,
            "agent": "general",
            "session_id": "team-session-1",
            "input": "coordinate the team",
            "idempotency_key": "team-1"
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{replay}");
    assert_eq!(replay["task_id"], execution);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["team_version"], 1);
}

#[test]
fn team_admission_rejects_hidden_member_bad_locality_master_and_fanout() {
    let stack = Stack::start_with_options(Options {
        with_echo_worker: true,
        ..Options::default()
    });
    // The ordinary caller cannot see worker-b, so it cannot smuggle it into a
    // team roster.
    let (status, hidden) = post(
        &stack,
        support::GATEWAY_TOKEN,
        "/v1/teams",
        json!({
            "name": "hidden",
            "master": "worker-a",
            "members": [
                member("worker-a", "general", 2),
                member("worker-b", "general", 2)
            ],
            "limits": {"max_depth": 2, "max_fanout": 2}
        }),
    );
    assert!(!status.is_success(), "{hidden}");
    assert_eq!(hidden["code"], "team.member_hidden");

    let members = json!([
        member("worker-a", "general", 2),
        member("worker-b", "general", 2)
    ]);
    let (status, locality) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams",
        json!({
            "name": "locality",
            "master": "worker-a",
            "members": members,
            "limits": {"max_depth": 2, "max_fanout": 2},
            "locality": "remote"
        }),
    );
    assert!(!status.is_success(), "{locality}");
    assert_eq!(locality["code"], "team.locality_incompatible");

    let (status, master) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams",
        json!({
            "name": "bad-master",
            "master": "worker-ghost",
            "members": [
                member("worker-a", "general", 2),
                member("worker-b", "general", 2)
            ],
            "limits": {"max_depth": 2, "max_fanout": 2}
        }),
    );
    assert!(!status.is_success(), "{master}");
    assert_eq!(master["code"], "team.master_invalid");

    let (status, fanout) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams",
        json!({
            "name": "fanout",
            "master": "worker-a",
            "members": [
                member("worker-a", "general", 2),
                member("worker-b", "general", 2),
                member("worker-c", "echo", 2)
            ],
            "limits": {"max_depth": 2, "max_fanout": 1}
        }),
    );
    assert!(!status.is_success(), "{fanout}");
    assert_eq!(fanout["code"], "team.fanout_exceeded");

    let (status, capability) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams",
        json!({
            "name": "capability",
            "master": "worker-a",
            "members": [
                member("worker-a", "general", 2),
                member("worker-c", "research", 2)
            ],
            "limits": {"max_depth": 2, "max_fanout": 2}
        }),
    );
    assert!(!status.is_success(), "{capability}");
    assert_eq!(capability["code"], "team.capability_unavailable");

    // The proposal form lists only subworkers and names the Master separately.
    let (status, implicit_master) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams",
        json!({
            "name": "subworkers-only",
            "master": "worker-a",
            "members": [member("worker-b", "general", 2)],
            "limits": {"max_depth": 2, "max_fanout": 1}
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{implicit_master}");
    assert_eq!(implicit_master["master"], "worker-a");
    assert_eq!(implicit_master["members"].as_array().unwrap().len(), 1);
}

#[test]
fn team_activation_rejects_membership_race() {
    let stack = Stack::start_with_options(Options {
        with_echo_worker: true,
        ..Options::default()
    });
    create_team(
        &stack,
        "race",
        team_body(
            "worker-a",
            json!([
                member("worker-a", "general", 2),
                member("worker-b", "general", 2),
                member("worker-c", "echo", 2)
            ]),
            2,
            3,
        ),
    );
    let disabled = Stack::client()
        .post(format!(
            "{}/v1/admin/workers/worker-c/disable",
            stack.gateway_base
        ))
        .bearer_auth(ADMIN_TOKEN)
        .send()
        .expect("disable responds");
    assert!(disabled.status().is_success());
    let (status, error) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams/race/activate",
        json!({"version": 1}),
    );
    assert!(!status.is_success(), "{error}");
    assert_eq!(error["code"], "team.member_unavailable");
    let fetched = Stack::client()
        .get(format!("{}/v1/teams/race", stack.gateway_base))
        .bearer_auth(ADMIN_TOKEN)
        .send()
        .expect("GET team")
        .json::<Value>()
        .expect("team JSON");
    assert_eq!(fetched["active"], false);
}

#[test]
fn team_child_delegation_is_narrowed_cycle_safe_and_uses_frozen_roster() {
    let stack = Stack::start_with_options(Options {
        with_echo_worker: true,
        ..Options::default()
    });
    let members = json!([
        member("worker-a", "general", 2),
        member("worker-b", "general", 2),
        member("worker-c", "echo", 2)
    ]);
    create_team(
        &stack,
        "bravo",
        team_body("worker-a", members.clone(), 2, 3),
    );
    activate_team(&stack, "bravo", 1);
    let (status, accepted) = post(
        &stack,
        ADMIN_TOKEN,
        "/v1/tasks",
        json!({
            "team": "bravo",
            "team_version": 1,
            "agent": "general",
            "session_id": "team-child-session",
            "input": "coordinate children",
            "idempotency_key": "team-child"
        }),
    );
    assert_eq!(status, reqwest::StatusCode::OK, "{accepted}");
    let execution = accepted["task_id"].as_str().expect("execution id");
    let projection = stack.wait_task(ADMIN_TOKEN, execution);
    let master_task = projection["task"]["id"].as_str().expect("Master task id");

    let delegate = |member: &str, capability: &str, key: &str| {
        Stack::client()
            .post(format!(
                "{}/v1/teams/bravo/members/{member}/delegate",
                stack.worker_a.base
            ))
            .bearer_auth(WORKER_A_TOKEN)
            .json(&json!({
                "capability": capability,
                "objective": format!("child objective for {member}"),
                "parent_task_id": master_task,
                "idempotency_key": key,
                "limits": {"max_depth": 9, "max_fanout": 9, "max_tokens": 999999}
            }))
            .send()
            .expect("team delegation responds")
    };

    // The independently built echo package executes as a real remote child.
    let response = delegate("worker-c", "echo", "child-c");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let child = response.json::<Value>().expect("child binding");
    assert_eq!(child["team"], "bravo");
    assert_eq!(child["team_version"], 1);
    assert_eq!(child["team_member"]["worker"], "worker-c");
    assert_eq!(child["depth"], 1);
    assert!(child["max_depth"].as_u64().unwrap_or(0) <= 3);
    assert!(child["limits"]["max_depth"].as_u64().unwrap_or(0) <= 2);
    assert!(child["limits"]["max_fanout"].as_u64().unwrap_or(0) <= 3);
    assert!(
        child["limits"]["max_tokens"].as_u64().unwrap_or(0) <= 999999,
        "{child}"
    );
    assert_eq!(child["state"], "completed", "{child}");

    // Self-delegation and a path cycle are rejected before any child exists.
    for (member, key) in [("worker-a", "cycle-master"), ("worker-a", "cycle-self")] {
        let response = delegate(member, "general", key);
        assert!(!response.status().is_success());
        let error = response.json::<Value>().expect("cycle error");
        assert_eq!(error["code"], "delegation.cycle", "{error}");
    }

    // A member cannot receive a capability outside its roster binding.
    let response = delegate("worker-b", "echo", "escalation");
    assert!(!response.status().is_success());
    let error = response.json::<Value>().expect("escalation error");
    assert_eq!(error["code"], "peer.capability_denied", "{error}");

    // A hidden identity is not selectable even when it is a live Worker.
    let response = delegate("worker-down", "general", "hidden");
    assert!(!response.status().is_success());
    let error = response.json::<Value>().expect("hidden error");
    assert_eq!(error["code"], "team.member_hidden", "{error}");

    // A later roster revision does not mutate the already running Master's
    // frozen membership.
    let updated = put(
        &stack,
        ADMIN_TOKEN,
        "/v1/teams/bravo",
        json!({
            "expected_version": 1,
            "master": "worker-a",
            "members": [
                member("worker-a", "general", 2),
                member("worker-c", "echo", 2)
            ],
            "limits": {"max_depth": 2, "max_fanout": 2}
        }),
    );
    assert_eq!(updated.0, reqwest::StatusCode::OK, "{}", updated.1);
    assert_eq!(updated.1["version"], 2);
    activate_team(&stack, "bravo", 2);
    let response = delegate("worker-b", "general", "frozen-b");
    // worker-b was removed from the current roster but remains authorized by
    // the immutable snapshot attached to the parent task.
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let frozen = response.json::<Value>().expect("frozen child");
    assert_eq!(frozen["team_version"], 1);
    assert_eq!(frozen["team_member"]["worker"], "worker-b");
}
