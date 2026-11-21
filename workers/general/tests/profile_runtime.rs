//! In-process profile activation against real native Runner execution.

use common::{Limits, Principal, ProfileRevision, SubmitTaskRequest, Task, TaskState};
use std::path::Path;
use std::time::{Duration, Instant};

mod fixture;

fn revision(
    instruction: &str,
    members: &[&str],
    capabilities: &[&str],
    grants: &[&str],
    skills: &[&str],
) -> ProfileRevision {
    ProfileRevision {
        revision: 0,
        instruction: instruction.into(),
        model: "deterministic".into(),
        capabilities: capabilities.iter().map(|value| (*value).into()).collect(),
        grants: grants.iter().map(|value| (*value).into()).collect(),
        restrictions: Vec::new(),
        composition: "single".into(),
        members: members.iter().map(|value| (*value).into()).collect(),
        skills: skills.iter().map(|value| (*value).into()).collect(),
        mcp: Vec::new(),
        workflow: None,
        limits: Limits::default(),
        compaction: None,
        secret_refs: Vec::new(),
        reasoning: None,
        created_at: 0,
    }
}

fn request(profile: &str, input: &str, key: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: "general".into(),
        session_id: format!("session-{key}"),
        input: input.into(),
        idempotency_key: key.into(),
        profile: Some(profile.into()),
        team: None,
        worker: None,
        limits: Limits::default(),
    }
}

async fn wait(runtime: &runtime::Runtime, task_id: &str) -> Task {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let task = runtime.task_unscoped(task_id).await.expect("task exists");
        if task.state.is_terminal() {
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn start(config: &Path) -> runtime::worker::WorkerInstance {
    general::worker()
        .expect("package definition")
        .start(config)
        .await
        .expect("worker starts")
}

#[tokio::test(flavor = "multi_thread")]
async fn activation_changes_new_native_execution_and_pins_in_flight_tasks() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "profile-runtime".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance
        .operator_principal()
        .await
        .expect("operator principal");

    runtime
        .create_profile(
            &admin,
            "local",
            "research",
            revision(
                "PROFILE-V1",
                &["general"],
                &["general"],
                &["task:submit"],
                &[],
            ),
        )
        .await
        .expect("profile created");
    runtime
        .activate_profile(&admin, "local", "research", 1)
        .await
        .expect("profile activated");

    let slow = runtime
        .submit_task(
            &admin,
            request("research", "delay:700 prompt:", "profile-slow"),
        )
        .await
        .expect("slow task admitted");
    let slow_id = slow.task_id.clone();
    assert_eq!(slow.state, TaskState::Accepted);

    let mut second_revision = revision(
        "PROFILE-V2",
        &["general"],
        &["general"],
        &["task:submit"],
        &[],
    );
    second_revision.limits.max_model_calls = 1;
    runtime
        .revise_profile(&admin, "local", "research", 1, second_revision)
        .await
        .expect("revision appended");
    runtime
        .activate_profile(&admin, "local", "research", 2)
        .await
        .expect("revision activated");

    let mut wider = request("research", "prompt:", "profile-new");
    wider.limits.max_model_calls = 5;
    let new = runtime
        .submit_task(&admin, wider)
        .await
        .expect("new task admitted");
    let new_task = wait(&runtime, &new.task_id).await;
    assert_eq!(new_task.state, TaskState::Succeeded);
    assert!(new_task
        .output
        .as_deref()
        .unwrap_or_default()
        .contains("PROFILE-V2"));
    assert_eq!(new_task.profile_revision, 2);
    assert_eq!(
        new_task.limits.max_model_calls, 1,
        "profile limits must narrow wider task limits"
    );

    let old_task = wait(&runtime, &slow_id).await;
    assert_eq!(old_task.state, TaskState::Succeeded);
    assert!(
        old_task
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("PROFILE-V1"),
        "in-flight task must retain its pinned revision: {old_task:?}"
    );
    assert_eq!(old_task.profile_revision, 1);

    let profile = runtime
        .profile(&admin, "local", "research")
        .await
        .expect("profile visible");
    assert_eq!(profile.revisions.len(), 2);
    assert_eq!(profile.active_revision, 2);
    assert_eq!(profile.revisions[0].instruction, "PROFILE-V1");
    assert_eq!(profile.revisions[1].instruction, "PROFILE-V2");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn effective_authority_capability_and_revocation_intersect() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "profile-authority".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance
        .operator_principal()
        .await
        .expect("operator principal");
    let restricted = Principal::new("restricted", "local", &["task:submit", "task:read"]);

    runtime
        .create_profile(
            &admin,
            "local",
            "research",
            revision(
                "PROFILE",
                &["general"],
                &["general"],
                &["task:submit", "admin"],
                &[],
            ),
        )
        .await
        .expect("profile created");
    runtime
        .activate_profile(&admin, "local", "research", 1)
        .await
        .expect("profile activated");

    let denied = runtime
        .submit_task(
            &restricted,
            request("research", "prompt:", "authority-denied"),
        )
        .await
        .expect_err("principal lacks profile grant");
    assert_eq!(denied.code, "profile.authority_denied");

    runtime
        .revise_profile(
            &admin,
            "local",
            "research",
            1,
            revision("PROFILE", &["general"], &["general"], &["task:submit"], &[]),
        )
        .await
        .expect("revision appended");
    runtime
        .activate_profile(&admin, "local", "research", 2)
        .await
        .expect("revision activated");

    let allowed = runtime
        .submit_task(
            &restricted,
            request("research", "prompt:", "authority-allowed"),
        )
        .await
        .expect("restricted principal now admitted");
    let allowed_task = wait(&runtime, &allowed.task_id).await;
    assert_eq!(allowed_task.state, TaskState::Succeeded);

    let denied_capability = runtime
        .submit_task(&restricted, {
            let mut body = request("research", "prompt:", "capability-denied");
            body.agent = "echo".into();
            body
        })
        .await
        .expect_err("agent is outside the profile capability list");
    assert_eq!(denied_capability.code, "profile.capability_denied");

    // Revoke task submission and prove new calls are blocked.
    runtime
        .revise_profile(
            &admin,
            "local",
            "research",
            2,
            revision("PROFILE", &["general"], &["general"], &[], &[]),
        )
        .await
        .expect("revision appended");
    runtime
        .activate_profile(&admin, "local", "research", 3)
        .await
        .expect("revision activated");
    let revoked = runtime
        .submit_task(&admin, request("research", "prompt:", "grant-revoked"))
        .await
        .expect_err("profile no longer grants submission");
    assert_eq!(revoked.code, "profile.grant_revoked");

    // Invalid references fail validation and leave the active revision intact.
    let invalid_member = runtime
        .revise_profile(
            &admin,
            "local",
            "research",
            3,
            revision("INVALID", &["absent"], &["general"], &["task:submit"], &[]),
        )
        .await
        .expect_err("unregistered member is rejected");
    assert_eq!(invalid_member.code, "profile.capability_unavailable");
    let invalid_skill = runtime
        .revise_profile(
            &admin,
            "local",
            "research",
            3,
            revision(
                "INVALID",
                &["general"],
                &["general"],
                &["task:submit"],
                &["absent"],
            ),
        )
        .await
        .expect_err("unknown skill is rejected");
    assert_eq!(invalid_skill.code, "profile.skill_unavailable");
    let mut conflicting = revision("INVALID", &["general"], &["general"], &["task:submit"], &[]);
    conflicting.restrictions = vec!["task:submit".into()];
    let restriction = runtime
        .revise_profile(&admin, "local", "research", 3, conflicting)
        .await
        .expect_err("granting and restricting the same scope is rejected");
    assert_eq!(restriction.code, "profile.restriction_conflict");
    let profile = runtime
        .profile(&admin, "local", "research")
        .await
        .expect("profile visible");
    assert_eq!(profile.active_revision, 3);
    assert_eq!(profile.revisions.len(), 3);
    instance.shutdown().await;
}
