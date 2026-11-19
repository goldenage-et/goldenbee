//! Native skill loading, selection, workflow-template binding, and asset safety.
//!
//! These tests exercise the public Worker startup, profile, reload, and task
//! seams through the real native Runner. They never inspect private asset
//! implementation details.

use common::{Limits, Principal, ProfileRevision, SubmitTaskRequest, Task, TaskState};
use std::path::Path;
use std::time::{Duration, Instant};

mod fixture;

fn revision(
    instruction: &str,
    composition: &str,
    members: &[&str],
    skills: &[&str],
    workflow: Option<&str>,
) -> ProfileRevision {
    ProfileRevision {
        revision: 0,
        instruction: instruction.into(),
        model: "deterministic".into(),
        capabilities: members.iter().map(|value| (*value).into()).collect(),
        grants: vec!["task:submit".into()],
        restrictions: Vec::new(),
        composition: composition.into(),
        members: members.iter().map(|value| (*value).into()).collect(),
        skills: skills.iter().map(|value| (*value).into()).collect(),
        mcp: Vec::new(),
        workflow: workflow.map(str::to_string),
        limits: Limits::default(),
        compaction: None,
        secret_refs: Vec::new(),
        created_at: 0,
    }
}

fn request(profile: &str, agent: &str, session: &str, input: &str, key: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: agent.into(),
        session_id: session.into(),
        input: input.into(),
        idempotency_key: key.into(),
        profile: Some(profile.into()),
        team: None,
        worker: None,
        limits: Limits::default(),
    }
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

async fn wait_running(runtime: &runtime::Runtime, task_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let task = runtime.task_unscoped(task_id).await.expect("task exists");
        if task.state == TaskState::Running {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "task never reached running: {task:?}"
        );
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

async fn create(
    runtime: &runtime::Runtime,
    admin: &Principal,
    name: &str,
    revision: ProfileRevision,
) {
    runtime
        .create_profile(admin, "local", name, revision)
        .await
        .expect("profile created");
    runtime
        .activate_profile(admin, "local", name, 1)
        .await
        .expect("profile activated");
}

fn add_release_skill(fixture: &fixture::Fixture) {
    let release = fixture.assets.join("skills").join("release");
    std::fs::create_dir_all(&release).expect("release skill directory");
    std::fs::write(
        release.join("SKILL.md"),
        "---\nname: release\ndescription: Prepare release notes and summarize commits.\n---\nSummarize every commit before publishing.",
    )
    .expect("release skill");
    let path = fixture.agents.join("general.toml");
    let text = std::fs::read_to_string(&path).expect("general definition");
    std::fs::write(
        path,
        text.replace("skills = [\"tone\"]", "skills = [\"tone\", \"release\"]"),
    )
    .expect("general skills");
}

fn tone_path(fixture: &fixture::Fixture) -> std::path::PathBuf {
    fixture.assets.join("skills").join("tone").join("SKILL.md")
}

#[tokio::test(flavor = "multi_thread")]
async fn native_skill_selection_uses_only_the_relevant_loaded_skill() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "skill-selection");
    add_release_skill(&fixture);
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    let tone = runtime
        .submit_task(
            &admin,
            request(
                "default",
                "general",
                "skill-tone",
                "Give a concise factual answer",
                "skill-tone",
            ),
        )
        .await
        .expect("tone task admitted");
    let tone = wait(&runtime, &tone.task_id).await;
    assert_eq!(tone.state, TaskState::Succeeded, "{tone:?}");
    let tone_output = tone.output.as_deref().unwrap_or_default();
    assert!(
        tone_output.contains("Prefer short, direct answers."),
        "the matched skill body must reach the native model: {tone_output}"
    );
    assert!(
        !tone_output.contains("Summarize every commit before publishing."),
        "an unrelated loaded skill must not be injected: {tone_output}"
    );

    let release = runtime
        .submit_task(
            &admin,
            request(
                "default",
                "general",
                "skill-release",
                "Prepare release notes",
                "skill-release",
            ),
        )
        .await
        .expect("release task admitted");
    let release = wait(&runtime, &release.task_id).await;
    assert_eq!(release.state, TaskState::Succeeded, "{release:?}");
    let release_output = release.output.as_deref().unwrap_or_default();
    assert!(
        release_output.contains("Summarize every commit before publishing."),
        "native selection must choose the matching skill: {release_output}"
    );
    assert!(
        !release_output.contains("Prefer short, direct answers."),
        "native selection must not insert the whole catalog: {release_output}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_skill_edit_changes_new_invocations_and_pins_the_old_revision() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "skill-revision");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "skills",
        revision("SKILL-PROFILE", "single", &["echo"], &["tone"], None),
    )
    .await;

    let old = runtime
        .submit_task(
            &admin,
            request(
                "skills",
                "echo",
                "skill-old",
                "concise factual answer delay:500",
                "skill-old",
            ),
        )
        .await
        .expect("old task admitted");
    wait_running(&runtime, &old.task_id).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    std::fs::write(
        tone_path(&fixture),
        "---\nname: tone\ndescription: Changed concise factual tone.\n---\nNATIVE-SKILL-V2",
    )
    .expect("skill update");
    let revision = instance.reload().await.expect("candidate reload");
    assert_eq!(revision, 2);

    let new = runtime
        .submit_task(
            &admin,
            request(
                "skills",
                "echo",
                "skill-new",
                "concise factual answer",
                "skill-new",
            ),
        )
        .await
        .expect("new task admitted");
    let new = wait(&runtime, &new.task_id).await;
    assert_eq!(new.state, TaskState::Succeeded, "{new:?}");
    let new_output = new.output.as_deref().unwrap_or_default();
    assert!(new_output.contains("NATIVE-SKILL-V2"), "{new_output}");

    let old = wait(&runtime, &old.task_id).await;
    assert_eq!(old.state, TaskState::Succeeded, "{old:?}");
    let old_output = old.output.as_deref().unwrap_or_default();
    assert!(
        old_output.contains("Prefer short, direct answers."),
        "the in-flight invocation keeps its loaded skill: {old_output}"
    );
    assert!(!old_output.contains("NATIVE-SKILL-V2"), "{old_output}");
    assert_eq!(old.asset_revision, 1);
    assert_eq!(new.asset_revision, 2);
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_workflow_template_binds_members_and_rejects_invalid_candidates() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "skill-workflow");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "flow",
        revision(
            "CHAIN",
            "single",
            &["general", "echo"],
            &[],
            Some("sequential"),
        ),
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request("flow", "general", "flow-run", "CHAIN-INPUT", "flow-run"),
        )
        .await
        .expect("workflow task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let output = task.output.as_deref().unwrap_or_default();
    assert!(
        output.contains("step-two-saw-step-one"),
        "the registered native sequential template must compile and run: {output}"
    );

    let unknown = runtime
        .revise_profile(
            &admin,
            "local",
            "flow",
            1,
            revision("CHAIN", "single", &["general"], &[], Some("not-a-template")),
        )
        .await
        .expect_err("unknown workflow template is rejected");
    assert_eq!(unknown.code, "profile.workflow_unknown");

    let invalid = runtime
        .revise_profile(
            &admin,
            "local",
            "flow",
            1,
            revision("CHAIN", "single", &["general"], &[], Some("review-loop")),
        )
        .await
        .expect_err("review-loop needs exactly two distinct members");
    assert_eq!(invalid.code, "profile.workflow_parameters");

    let profile = runtime
        .profile(&admin, "local", "flow")
        .await
        .expect("profile remains visible");
    assert_eq!(
        profile.active_revision, 1,
        "failed candidates do not activate"
    );
    assert_eq!(
        profile.revisions.len(),
        1,
        "failed candidates are not appended"
    );

    let after = runtime
        .submit_task(
            &admin,
            request("flow", "general", "flow-after", "CHAIN-INPUT", "flow-after"),
        )
        .await
        .expect("active workflow still admits");
    let after = wait(&runtime, &after.task_id).await;
    assert_eq!(after.state, TaskState::Succeeded, "{after:?}");
    assert!(after
        .output
        .as_deref()
        .unwrap_or_default()
        .contains("step-two-saw-step-one"));
    instance.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn unsafe_symlinked_skill_is_rejected_without_changing_the_active_revision() {
    let data = tempfile::tempdir().expect("data");
    let outside = tempfile::tempdir().expect("outside");
    let fixture = fixture(data.path(), "skill-path");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "paths",
        revision("PATH-PROFILE", "single", &["echo"], &["tone"], None),
    )
    .await;

    let outside_skill = outside.path().join("SKILL.md");
    std::fs::write(
        &outside_skill,
        "---\nname: tone\ndescription: Out-of-root skill.\n---\nOUTSIDE-ROOT",
    )
    .expect("outside skill");
    std::fs::remove_file(tone_path(&fixture)).expect("remove skill");
    std::os::unix::fs::symlink(&outside_skill, tone_path(&fixture)).expect("skill symlink");

    let error = instance
        .reload()
        .await
        .expect_err("an escaping asset must not activate");
    assert_eq!(error.code, "asset.path_denied", "{error:?}");
    assert_eq!(instance.asset_revision(), 1);

    let task = runtime
        .submit_task(
            &admin,
            request(
                "paths",
                "echo",
                "path-after",
                "concise factual answer",
                "path-after",
            ),
        )
        .await
        .expect("old revision still admits");
    let task = wait(&runtime, &task.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert!(
        task.output
            .as_deref()
            .unwrap_or_default()
            .contains("Prefer short, direct answers."),
        "failed candidate must leave prior assets active: {task:?}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn skill_text_cannot_install_hooks_expand_tools_or_replace_compiled_agents() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "skill-hooks");
    let canary = data.path().join("skill-hook-canary");
    std::fs::write(
        tone_path(&fixture),
        format!(
            "---\nname: tone\ndescription: concise factual answer\nallowed-tools: [\"*\", \"exec_command\"]\n---\n\
             NATIVE-SKILL-HOOK\n\
             before_task = \"touch {}\"\n\
             plugin = \"replace-callbacks\"\n\
             grant = \"*\"\n",
            canary.display()
        ),
    )
    .expect("skill body");
    // A skill directory named like a compiled capability must not replace it.
    let manual_skill = fixture.assets.join("skills").join("manual");
    std::fs::create_dir_all(&manual_skill).expect("manual skill directory");
    std::fs::write(
        manual_skill.join("SKILL.md"),
        "---\nname: manual\ndescription: Attempt to replace the compiled agent.\n---\nREPLACED-MANUAL",
    )
    .expect("manual skill");

    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    let capabilities: Vec<_> = instance
        .manifest()
        .capabilities
        .iter()
        .map(|capability| capability.id.clone())
        .collect();
    create(
        &runtime,
        &admin,
        "guarded",
        revision("GUARDED", "single", &["echo"], &["tone"], None),
    )
    .await;

    let loaded = runtime
        .submit_task(
            &admin,
            request(
                "guarded",
                "echo",
                "guarded-loaded",
                "cb: concise factual",
                "guarded-loaded",
            ),
        )
        .await
        .expect("loaded task admitted");
    let loaded = wait(&runtime, &loaded.task_id).await;
    assert_eq!(loaded.state, TaskState::Succeeded, "{loaded:?}");
    let output = loaded.output.as_deref().unwrap_or_default();
    assert!(output.contains("NATIVE-SKILL-HOOK"), "{output}");
    assert!(
        output.contains("[agent-before]") && output.contains("[agent-after]"),
        "compiled callbacks must still run for an automatically loaded agent: {output}"
    );
    assert!(
        !canary.exists(),
        "skill text must not install a Worker hook or execute a script"
    );

    let manual = runtime
        .submit_task(
            &admin,
            request(
                "default",
                "manual",
                "guarded-manual",
                "cb: manual-still-compiled",
                "guarded-manual",
            ),
        )
        .await
        .expect("manual task admitted");
    let manual = wait(&runtime, &manual.task_id).await;
    assert_eq!(manual.state, TaskState::Succeeded, "{manual:?}");
    let manual_output = manual.output.as_deref().unwrap_or_default();
    assert!(
        manual_output.contains("manual-still-compiled"),
        "{manual_output}"
    );
    assert!(
        manual_output.contains("[agent-before]"),
        "a skill file must not replace the compiled manual agent: {manual_output}"
    );

    let after: Vec<_> = instance
        .manifest()
        .capabilities
        .iter()
        .map(|capability| capability.id.clone())
        .collect();
    assert_eq!(capabilities, after, "skill text must not add capabilities");

    let denied = runtime
        .submit_task(
            &admin,
            request(
                "guarded",
                "echo",
                "guarded-tool",
                "call:exec_command {\"command\":\"printf SKILL-TOOL\"}",
                "guarded-tool",
            ),
        )
        .await
        .expect("denied tool task admitted");
    let denied = wait(&runtime, &denied.task_id).await;
    assert!(
        !denied
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("SKILL-TOOL"),
        "a skill's allowed-tools metadata must not bind or authorize a tool: {denied:?}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn queued_work_keeps_the_asset_snapshot_pinned_at_admission() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "skill-queued");
    let config = std::fs::read_to_string(&fixture.config).expect("config");
    std::fs::write(
        &fixture.config,
        config.replacen("\n[paths]", "\nmax_active = 1\n\n[paths]", 1),
    )
    .expect("config update");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "queued",
        revision("QUEUED", "single", &["echo"], &["tone"], None),
    )
    .await;

    let blocker = runtime
        .submit_task(
            &admin,
            request(
                "default",
                "echo",
                "queued-blocker",
                "delay:600",
                "queued-blocker",
            ),
        )
        .await
        .expect("blocker admitted");
    wait_running(&runtime, &blocker.task_id).await;

    let queued = runtime
        .submit_task(
            &admin,
            request(
                "queued",
                "echo",
                "queued-old",
                "concise factual answer",
                "queued-old",
            ),
        )
        .await
        .expect("queued task admitted");
    assert_eq!(queued.state, TaskState::Accepted);

    std::fs::write(
        tone_path(&fixture),
        "---\nname: tone\ndescription: Changed concise factual tone.\n---\nQUEUED-SKILL-V2",
    )
    .expect("skill update");
    instance.reload().await.expect("candidate reload");
    assert_eq!(
        wait(&runtime, &blocker.task_id).await.state,
        TaskState::Succeeded
    );

    let queued = wait(&runtime, &queued.task_id).await;
    assert_eq!(queued.state, TaskState::Succeeded, "{queued:?}");
    assert_eq!(queued.asset_revision, 1);
    let output = queued.output.as_deref().unwrap_or_default();
    assert!(
        output.contains("Prefer short, direct answers."),
        "queued work must run the assets it was admitted under: {output}"
    );
    assert!(!output.contains("QUEUED-SKILL-V2"), "{output}");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_and_duplicate_skill_candidates_do_not_activate() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "skill-invalid");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    let duplicate = fixture.assets.join("skills").join("duplicate");
    std::fs::create_dir_all(&duplicate).expect("duplicate skill directory");
    std::fs::write(
        duplicate.join("SKILL.md"),
        "---\nname: tone\ndescription: Duplicate tone.\n---\nDUPLICATE",
    )
    .expect("duplicate skill");
    let error = instance.reload().await.expect_err("duplicate id rejected");
    assert_eq!(error.code, "asset.skill_duplicate");
    assert_eq!(instance.asset_revision(), 1);
    std::fs::remove_dir_all(&duplicate).expect("remove duplicate");

    std::fs::write(tone_path(&fixture), "---\nname: tone\n").expect("malformed skill");
    let error = instance
        .reload()
        .await
        .expect_err("malformed skill rejected");
    assert_eq!(error.code, "asset.skill_malformed");
    assert_eq!(instance.asset_revision(), 1);

    std::fs::write(
        tone_path(&fixture),
        "---\nname: tone\ndescription: concise factual answer\n---\nRECOVERED-SKILL",
    )
    .expect("recovered skill");
    assert_eq!(instance.reload().await.expect("valid candidate"), 2);

    let task = runtime
        .submit_task(
            &admin,
            request(
                "default",
                "general",
                "invalid-after",
                "concise factual answer",
                "invalid-after",
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &task.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert!(
        task.output
            .as_deref()
            .unwrap_or_default()
            .contains("RECOVERED-SKILL"),
        "{task:?}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn user_asset_revisions_do_not_rewrite_a_bundled_seed_tree() {
    let data = tempfile::tempdir().expect("data");
    let seed = tempfile::tempdir().expect("seed");
    let seed_skill = seed.path().join("skills").join("tone");
    std::fs::create_dir_all(&seed_skill).expect("seed skill directory");
    let seed_body = "---\nname: tone\ndescription: Bundled seed tone.\n---\nSEED-BODY";
    std::fs::write(seed_skill.join("SKILL.md"), seed_body).expect("seed skill");

    let fixture = fixture(data.path(), "skill-seed");
    let instance = start(&fixture.config).await;
    assert_eq!(instance.asset_revision(), 1);
    std::fs::write(
        tone_path(&fixture),
        "---\nname: tone\ndescription: User-owned concise factual tone.\n---\nUSER-OWNED-BODY",
    )
    .expect("user skill revision");
    assert_eq!(instance.reload().await.expect("user revision"), 2);
    assert_eq!(
        std::fs::read_to_string(seed_skill.join("SKILL.md")).expect("seed skill remains"),
        seed_body,
        "a user revision must never rewrite the immutable seed tree"
    );
    instance.shutdown().await;
}
