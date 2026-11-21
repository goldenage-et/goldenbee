//! Progressive skill disclosure through the pinned adk-skill toolset.
//!
//! These tests drive the real native Runner with the deterministic model. They
//! assert that an unloaded skill body is not injected, that instructions and
//! bundled references load on demand, that scripts remain inert resources, and
//! that a profile's skill selection bounds the discoverable catalog.

use common::{Limits, Principal, ProfileRevision, SubmitTaskRequest, Task, TaskState};
use std::path::Path;
use std::time::{Duration, Instant};

mod fixture;

fn fixture(data: &Path, instance: &str) -> fixture::Fixture {
    fixture::write(
        data,
        fixture::Options {
            instance: instance.into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    )
}

fn set_disclosure(fixture: &fixture::Fixture, value: &str) {
    let text = std::fs::read_to_string(&fixture.config).expect("worker config");
    std::fs::write(
        &fixture.config,
        format!("{text}\n[skills]\ndisclosure = \"{value}\"\n"),
    )
    .expect("skills table");
}

async fn start(config: &Path) -> runtime::worker::WorkerInstance {
    general::worker()
        .expect("package definition")
        .start(config)
        .await
        .expect("worker starts")
}

async fn wait(runtime: &runtime::Runtime, task_id: &str) -> Task {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let task = runtime.task_unscoped(task_id).await.expect("task exists");
        if task.state.is_terminal() {
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn request(agent: &str, session: &str, input: &str, key: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: agent.into(),
        session_id: session.into(),
        input: input.into(),
        idempotency_key: key.into(),
        profile: None,
        team: None,
        worker: None,
        limits: Limits::default(),
    }
}

async fn run(
    runtime: &runtime::Runtime,
    principal: &Principal,
    agent: &str,
    session: &str,
    input: &str,
    key: &str,
) -> Task {
    let submitted = runtime
        .submit_task(principal, request(agent, session, input, key))
        .await
        .expect("task admitted");
    wait(runtime, &submitted.task_id).await
}

fn add_resources(fixture: &fixture::Fixture, canary: &Path) {
    let tone = fixture.assets.join("skills").join("tone");
    std::fs::create_dir_all(tone.join("references")).expect("references");
    std::fs::create_dir_all(tone.join("scripts")).expect("scripts");
    std::fs::write(
        tone.join("references/guide.md"),
        "REFERENCE-GUIDE: prefer the short checklist.",
    )
    .expect("reference");
    std::fs::write(
        tone.join("scripts/run.sh"),
        format!("#!/bin/sh\ntouch {}\n", canary.display()),
    )
    .expect("script");
}

fn add_other_skill(fixture: &fixture::Fixture) {
    let other = fixture.assets.join("skills").join("other");
    std::fs::create_dir_all(&other).expect("other skill");
    std::fs::write(
        other.join("SKILL.md"),
        "---\nname: other\ndescription: OTHER-SKILL-DESCRIPTION\n---\nOTHER-SKILL-BODY",
    )
    .expect("other skill");
}

#[tokio::test(flavor = "multi_thread")]
async fn progressive_disclosure_lists_then_loads_instructions_and_resources() {
    let data = tempfile::tempdir().expect("data");
    let canary = data.path().join("script-canary");
    let fixture = fixture(data.path(), "skills-progressive");
    set_disclosure(&fixture, "progressive");
    add_resources(&fixture, &canary);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    let listed = run(
        &runtime,
        &admin,
        "general",
        "progressive",
        "call:list_skills",
        "progressive-list",
    )
    .await;
    assert_eq!(listed.state, TaskState::Succeeded, "{listed:?}");
    let output = listed.output.as_deref().unwrap_or_default();
    assert!(output.contains("tone"), "{output}");
    assert!(output.contains("Keep replies concise"), "{output}");
    assert!(
        !output.contains("Prefer short, direct answers."),
        "the skill body must stay behind load_skill: {output}"
    );

    let traversal = run(
        &runtime,
        &admin,
        "general",
        "progressive-traversal",
        "call:load_skill_resource {\"skill_name\":\"tone\",\"file_path\":\"references/../SKILL.md\"}",
        "progressive-traversal",
    )
    .await;
    assert!(
        traversal
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("resource path"),
        "a skill resource must stay inside references/, assets/, or scripts/: {traversal:?}"
    );

    let loaded = run(
        &runtime,
        &admin,
        "general",
        "progressive",
        "call:load_skill {\"skill_name\":\"tone\"}",
        "progressive-load",
    )
    .await;
    assert_eq!(loaded.state, TaskState::Succeeded, "{loaded:?}");
    assert!(
        loaded
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("Prefer short, direct answers."),
        "load_skill must return the selected body: {loaded:?}"
    );

    let reference = run(
        &runtime,
        &admin,
        "general",
        "progressive",
        "call:load_skill_resource {\"skill_name\":\"tone\",\"file_path\":\"references/guide.md\"}",
        "progressive-reference",
    )
    .await;
    assert_eq!(reference.state, TaskState::Succeeded, "{reference:?}");
    assert!(
        reference
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("REFERENCE-GUIDE"),
        "an activated skill may read its references: {reference:?}"
    );

    let script = run(
        &runtime,
        &admin,
        "general",
        "progressive",
        "call:load_skill_resource {\"skill_name\":\"tone\",\"file_path\":\"scripts/run.sh\"}",
        "progressive-script",
    )
    .await;
    assert_eq!(script.state, TaskState::Succeeded, "{script:?}");
    assert!(
        script
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("touch"),
        "a bundled script is visible as a resource: {script:?}"
    );
    assert!(
        !canary.exists(),
        "reading a skill resource must never execute its script"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_skill_selection_bounds_the_discoverable_catalog() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "skills-progressive-profile");
    set_disclosure(&fixture, "progressive");
    add_other_skill(&fixture);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    runtime
        .create_profile(
            &admin,
            "local",
            "other-only",
            ProfileRevision {
                revision: 0,
                instruction: "PROFILE-OTHER".into(),
                model: "deterministic".into(),
                capabilities: vec!["general".into()],
                grants: vec!["task:submit".into()],
                restrictions: Vec::new(),
                composition: "single".into(),
                members: vec!["general".into()],
                skills: vec!["other".into()],
                mcp: Vec::new(),
                workflow: None,
                limits: Limits::default(),
                compaction: None,
                secret_refs: Vec::new(),
                created_at: 0,
            },
        )
        .await
        .expect("profile created");
    runtime
        .activate_profile(&admin, "local", "other-only", 1)
        .await
        .expect("profile activated");

    let submitted = runtime
        .submit_task(
            &admin,
            SubmitTaskRequest {
                profile: Some("other-only".into()),
                ..request(
                    "general",
                    "progressive-profile",
                    "call:list_skills",
                    "profile",
                )
            },
        )
        .await
        .expect("profile task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let output = task.output.as_deref().unwrap_or_default();
    assert!(output.contains("OTHER-SKILL-DESCRIPTION"), "{output}");
    assert!(
        !output.contains("Keep replies concise"),
        "the profile skill selection must exclude other discovered skills: {output}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn progressive_disclosure_requires_a_declared_package_capability() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "skills-progressive-declared");
    set_disclosure(&fixture, "unsupported");
    let error = general::worker()
        .expect("package definition")
        .start(&fixture.config)
        .await
        .err()
        .expect("unsupported disclosure is rejected");
    assert_eq!(error.code, "config.skill_disclosure");
}
