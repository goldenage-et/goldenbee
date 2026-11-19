//! Native ADK composition evidence through the real Runner.
//!
//! These tests exercise sequential, parallel, loop, and coordinator/specialist
//! teams built from pinned ADK primitives. They assert public task results and
//! durable events, not runtime internals.

use adk_rust::agent::{RelationshipKind, TeamMemberSpec, TeamPolicy, TeamRelationship, TeamSpec};
use adk_rust::prelude::{LlmAgentBuilder, SequentialAgent};
use adk_rust::Agent;
use common::{Limits, Principal, ProfileRevision, StreamEvent, SubmitTaskRequest, Task, TaskState};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod fixture;

fn revision(
    instruction: &str,
    composition: &str,
    members: &[&str],
    skills: &[&str],
    limits: Limits,
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
        workflow: None,
        limits,
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

fn set_top_level(config: &Path, line: &str) {
    let text = std::fs::read_to_string(config).expect("config");
    let marker = "\n[paths]";
    let updated = text.replacen(marker, &format!("\n{line}\n{marker}"), 1);
    std::fs::write(config, updated).expect("config update");
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

async fn events(runtime: &runtime::Runtime, task_id: &str) -> Vec<StreamEvent> {
    runtime
        .events_since(task_id, 0, 10_000)
        .await
        .expect("events")
        .events
}

/// Drain live broadcast events for one task. Model deltas are intentionally
/// ephemeral, so attribution is observed on the public live stream.
fn live(rx: &mut tokio::sync::broadcast::Receiver<StreamEvent>, task_id: &str) -> Vec<StreamEvent> {
    let stream = format!("task:{task_id}");
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if event.stream_id == stream {
            events.push(event);
        }
    }
    events
}

fn authors(events: &[StreamEvent], kind: &str) -> Vec<String> {
    events
        .iter()
        .filter(|event| event.kind == kind)
        .filter_map(|event| event.payload.get("author"))
        .filter_map(|author| author.as_str())
        .map(str::to_string)
        .collect()
}

fn complete_authors(events: &[StreamEvent]) -> Vec<String> {
    authors(events, "model.delta.complete")
}

fn count(text: &str, needle: &str) -> usize {
    text.match_indices(needle).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn sequential_composition_passes_earlier_result_to_the_next_native_agent() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-sequential");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "chain",
        revision(
            "CHAIN",
            "sequential",
            &["general", "echo"],
            &[],
            Limits::default(),
        ),
    )
    .await;

    let mut live_events = runtime.subscribe();
    let submitted = runtime
        .submit_task(
            &admin,
            request("chain", "general", "chain-session", "CHAIN-INPUT", "chain"),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert_eq!(task.profile_revision, 1);
    let output = task.output.as_deref().unwrap_or_default();
    assert!(output.contains("step-one"), "{output}");
    assert!(
        output.contains("step-two-saw-step-one"),
        "second sequential agent must see the first result: {output}"
    );
    let authors = complete_authors(&live(&mut live_events, &task.id));
    let first = authors
        .iter()
        .position(|author| author == "general")
        .expect("first author");
    let second = authors
        .iter()
        .position(|author| author == "echo")
        .expect("second author");
    assert!(first < second, "native order is not preserved: {authors:?}");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_composition_keeps_each_independent_branch_output() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-parallel");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "branches",
        revision(
            "PARALLEL",
            "parallel",
            &["general", "echo"],
            &[],
            Limits::default(),
        ),
    )
    .await;

    let mut live_events = runtime.subscribe();
    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "branches",
                "general",
                "branch-session",
                "BRANCH-INPUT",
                "branches",
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let output = task.output.as_deref().unwrap_or_default();
    assert!(output.contains("branch-general"), "{output}");
    assert!(output.contains("branch-echo"), "{output}");
    let events = live(&mut live_events, &task.id);
    let authors = complete_authors(&events);
    assert_eq!(
        authors.iter().filter(|author| *author == "general").count(),
        1,
        "each parallel branch runs once: {authors:?}"
    );
    assert_eq!(
        authors.iter().filter(|author| *author == "echo").count(),
        1,
        "each parallel branch runs once: {authors:?}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn loop_composition_is_bounded_by_the_profile_model_call_limit() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-loop");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    let limits = Limits {
        max_model_calls: 2,
        ..Limits::default()
    };
    create(
        &runtime,
        &admin,
        "loop",
        revision("LOOP", "loop", &["echo"], &[], limits),
    )
    .await;

    let mut live_events = runtime.subscribe();
    let submitted = runtime
        .submit_task(
            &admin,
            request("loop", "echo", "loop-session", "LOOP-INPUT", "loop"),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert_eq!(task.usage.model_calls, 2, "{task:?}");
    let output = task.output.as_deref().unwrap_or_default();
    assert_eq!(
        count(output, "echo: LOOP-INPUT"),
        2,
        "loop must run exactly the bounded number of passes: {output}"
    );
    let complete = complete_authors(&live(&mut live_events, &task.id));
    assert_eq!(complete.len(), 2, "{complete:?}");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_team_preserves_delegation_receipts_and_branch_outputs() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-team");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "team",
        revision(
            "TEAM-PROFILE",
            "team",
            &["general", "echo"],
            &[],
            Limits::default(),
        ),
    )
    .await;

    let mut live_events = runtime.subscribe();
    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "team",
                "general",
                "team-session",
                "delegate:general,echo",
                "team",
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let output = task.output.as_deref().unwrap_or_default();
    assert!(output.contains("team-final"), "{output}");

    let authors = complete_authors(&live(&mut live_events, &task.id));
    let events = events(&runtime, &task.id).await;
    assert!(
        authors.iter().any(|author| author == "general"),
        "{authors:?}"
    );
    assert!(authors.iter().any(|author| author == "echo"), "{authors:?}");
    assert_eq!(
        authors.iter().filter(|author| *author == "general").count(),
        1,
        "delegated child must run once: {authors:?}"
    );
    assert_eq!(
        authors.iter().filter(|author| *author == "echo").count(),
        1,
        "delegated child must run once: {authors:?}"
    );

    let receipts: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "team.receipt")
        .collect();
    assert!(!receipts.is_empty(), "native team receipt is missing");
    let both_targets = receipts.iter().any(|event| {
        let edges = event
            .payload
            .get("edges")
            .and_then(|edges| edges.as_array())
            .cloned()
            .unwrap_or_default();
        let targets: Vec<_> = edges
            .iter()
            .filter_map(|edge| edge.get("to").and_then(|to| to.as_str()))
            .collect();
        targets.contains(&"general")
            && targets.contains(&"echo")
            && edges.iter().any(|edge| {
                edge.get("kind").and_then(|kind| kind.as_str()) == Some("delegate")
                    && edge.get("status").and_then(|status| status.as_str()) == Some("completed")
            })
    });
    assert!(
        both_targets,
        "receipt lacks native delegate edges: {receipts:?}"
    );
    assert!(
        !events.iter().any(|event| event.kind == "task.transfer"),
        "delegate composition must not fabricate a transfer"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn team_budget_stops_excess_delegation_with_truthful_failure() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-team-budget");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    let limits = Limits {
        max_tool_calls: 1,
        ..Limits::default()
    };
    create(
        &runtime,
        &admin,
        "team",
        revision("TEAM-PROFILE", "team", &["general", "echo"], &[], limits),
    )
    .await;

    let mut live_events = runtime.subscribe();
    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "team",
                "general",
                "team-budget-session",
                "delegate:general,echo",
                "team-budget",
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(
        task.error.as_ref().map(|error| error.code.as_str()),
        Some("agent.team.budget_exceeded"),
        "{task:?}"
    );
    let live_events = live(&mut live_events, &task.id);
    assert!(
        !task
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("team-final"),
        "a budget-stopped team must not fabricate a final result: {task:?}"
    );
    let events = events(&runtime, &task.id).await;
    let receipts: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "team.receipt")
        .collect();
    for receipt in &receipts {
        let delegations = receipt
            .payload
            .pointer("/usage/delegations")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        assert!(
            delegations <= 1,
            "budget must stop the second delegation: {receipt:?}"
        );
    }
    assert!(
        !complete_authors(&live_events)
            .iter()
            .any(|author| author == "echo"),
        "no work may run after the budget is exceeded"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deadline_stops_only_the_composed_task_and_an_unrelated_session_continues() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-deadline");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "slow",
        revision(
            "SLOW",
            "sequential",
            &["general", "echo"],
            &[],
            Limits::default(),
        ),
    )
    .await;

    let mut slow = request("slow", "general", "slow-session", "delay:600", "slow");
    slow.limits.max_duration_ms = 100;
    let slow = runtime
        .submit_task(&admin, slow)
        .await
        .expect("slow task admitted");

    // The unrelated session uses the default profile and must finish while the
    // bounded composition is still running.
    let fast = runtime
        .submit_task(
            &admin,
            request(
                "default",
                "echo",
                "unrelated-session",
                "UNRELATED",
                "unrelated",
            ),
        )
        .await
        .expect("unrelated task admitted");
    let fast_task = wait(&runtime, &fast.task_id).await;
    assert_eq!(fast_task.state, TaskState::Succeeded, "{fast_task:?}");
    assert!(fast_task
        .output
        .as_deref()
        .unwrap_or_default()
        .contains("UNRELATED"));

    let task = wait(&runtime, &slow.task_id).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(
        task.error.as_ref().map(|error| error.code.as_str()),
        Some("budget.deadline_exceeded"),
        "{task:?}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_composition_references_fail_before_admission() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-invalid");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "base",
        revision("BASE", "single", &["general"], &[], Limits::default()),
    )
    .await;
    let before = runtime.all_tasks().await.len();

    let missing = runtime
        .revise_profile(
            &admin,
            "local",
            "base",
            1,
            revision(
                "X",
                "sequential",
                &["general", "absent"],
                &[],
                Limits::default(),
            ),
        )
        .await
        .expect_err("missing member is rejected");
    assert_eq!(missing.code, "profile.capability_unavailable");

    let duplicate = runtime
        .revise_profile(
            &admin,
            "local",
            "base",
            1,
            revision(
                "X",
                "parallel",
                &["general", "general"],
                &[],
                Limits::default(),
            ),
        )
        .await
        .expect_err("duplicate member is rejected");
    assert_eq!(duplicate.code, "profile.member_duplicate");

    let coordinator = runtime
        .revise_profile(
            &admin,
            "local",
            "base",
            1,
            revision("X", "team", &["coordinator"], &[], Limits::default()),
        )
        .await
        .expect_err("reserved coordinator name is rejected");
    assert_eq!(coordinator.code, "profile.member_reserved");

    let empty = runtime
        .revise_profile(
            &admin,
            "local",
            "base",
            1,
            revision("X", "sequential", &[], &[], Limits::default()),
        )
        .await
        .expect_err("empty non-single composition is rejected");
    assert_eq!(empty.code, "profile.composition_empty");

    let unsupported = runtime
        .revise_profile(
            &admin,
            "local",
            "base",
            1,
            revision("X", "graph", &["general"], &[], Limits::default()),
        )
        .await
        .expect_err("unsupported composition is rejected");
    assert_eq!(unsupported.code, "profile.composition_invalid");

    let fanout_limits = Limits {
        max_fanout: 1,
        ..Limits::default()
    };
    let fanout = runtime
        .revise_profile(
            &admin,
            "local",
            "base",
            1,
            revision("X", "parallel", &["general", "echo"], &[], fanout_limits),
        )
        .await
        .expect_err("parallel fanout over the limit is rejected");
    assert_eq!(fanout.code, "profile.fanout_exceeded");

    assert_eq!(
        runtime.all_tasks().await.len(),
        before,
        "invalid revisions must not admit work"
    );
    assert_eq!(
        runtime
            .profile(&admin, "local", "base")
            .await
            .expect("profile")
            .active_revision,
        1
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revision_during_execution_cannot_mutate_the_active_roster() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-revision");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    let package_revision = instance.asset_revision();
    create(
        &runtime,
        &admin,
        "roster",
        revision("R-V1", "team", &["general", "echo"], &[], Limits::default()),
    )
    .await;

    let mut old_live = runtime.subscribe();
    let slow = runtime
        .submit_task(
            &admin,
            request(
                "roster",
                "general",
                "roster-old",
                "delegate:general,echo delay:500",
                "roster-old",
            ),
        )
        .await
        .expect("slow composition admitted");

    runtime
        .revise_profile(
            &admin,
            "local",
            "roster",
            1,
            revision("R-V2", "team", &["general"], &[], Limits::default()),
        )
        .await
        .expect("revision appended");
    runtime
        .activate_profile(&admin, "local", "roster", 2)
        .await
        .expect("revision activated");

    let mut new_live = runtime.subscribe();
    let new = runtime
        .submit_task(
            &admin,
            request(
                "roster",
                "general",
                "roster-new",
                "delegate:general",
                "roster-new",
            ),
        )
        .await
        .expect("new task admitted");
    let new_task = wait(&runtime, &new.task_id).await;
    assert_eq!(new_task.state, TaskState::Succeeded, "{new_task:?}");
    assert_eq!(new_task.profile_revision, 2);
    let new_authors = complete_authors(&live(&mut new_live, &new_task.id));
    assert!(
        !new_authors.iter().any(|author| author == "echo"),
        "new revision must use the new roster only: {new_authors:?}"
    );

    let old_task = wait(&runtime, &slow.task_id).await;
    assert_eq!(old_task.state, TaskState::Succeeded, "{old_task:?}");
    assert_eq!(old_task.profile_revision, 1);
    let old_authors = complete_authors(&live(&mut old_live, &old_task.id));
    assert!(
        old_authors.iter().any(|author| author == "echo"),
        "in-flight composition keeps its pinned roster: {old_authors:?}"
    );
    assert_eq!(
        instance.asset_revision(),
        package_revision,
        "a profile revision must not change the installed package revision"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_asset_revision_does_not_mutate_an_active_composition() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-assets");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "assets",
        revision(
            "ASSET-PROFILE",
            "sequential",
            &["general", "echo"],
            &["tone"],
            Limits::default(),
        ),
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "assets",
                "general",
                "assets-old",
                "delay:500 prompt: concise factual",
                "assets-old",
            ),
        )
        .await
        .expect("task admitted");
    wait_running(&runtime, &submitted.task_id).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let skill = fixture.assets.join("skills").join("tone").join("SKILL.md");
    std::fs::write(
        &skill,
        "---\nname: tone\ndescription: Changed concise factual tone.\n---\nCHANGED-SKILL-BODY",
    )
    .expect("skill update");
    instance.reload().await.expect("candidate reload");

    let old = wait(&runtime, &submitted.task_id).await;
    assert_eq!(old.state, TaskState::Succeeded, "{old:?}");
    let old_output = old.output.as_deref().unwrap_or_default();
    assert!(
        old_output.contains("Prefer short, direct answers."),
        "in-flight composition keeps its resolved skill: {old_output}"
    );
    assert!(!old_output.contains("CHANGED-SKILL-BODY"), "{old_output}");

    let new = runtime
        .submit_task(
            &admin,
            request(
                "assets",
                "general",
                "assets-new",
                "prompt: concise factual",
                "assets-new",
            ),
        )
        .await
        .expect("new task admitted");
    let new_task = wait(&runtime, &new.task_id).await;
    assert_eq!(new_task.state, TaskState::Succeeded, "{new_task:?}");
    assert!(
        new_task
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("CHANGED-SKILL-BODY"),
        "new execution resolves the new asset revision: {new_task:?}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pinned_revision_that_disappears_does_not_silently_run_the_base_agent() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-pinned");
    set_top_level(&fixture.config, "max_active = 1");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "vanish",
        revision("VANISH", "single", &["general"], &[], Limits::default()),
    )
    .await;

    let blocker = runtime
        .submit_task(
            &admin,
            request("default", "echo", "blocker-session", "delay:500", "blocker"),
        )
        .await
        .expect("blocker admitted");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let pinned = runtime
        .submit_task(
            &admin,
            request("vanish", "general", "pinned-session", "prompt:", "pinned"),
        )
        .await
        .expect("pinned task admitted while revision is active");
    runtime
        .deactivate_profile(&admin, "local", "vanish")
        .await
        .expect("profile deactivated");
    runtime
        .delete_profile(&admin, "local", "vanish")
        .await
        .expect("inactive profile removed");
    assert_eq!(
        wait(&runtime, &blocker.task_id).await.state,
        TaskState::Succeeded
    );

    let task = wait(&runtime, &pinned.task_id).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(
        task.error.as_ref().map(|error| error.code.as_str()),
        Some("profile.revision_unavailable"),
        "{task:?}"
    );
    assert!(
        !task
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("VANISH"),
        "the base agent must not answer for a missing pinned revision"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_composition_resolves_loaded_prompts_and_selected_skills() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-context");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "skills",
        revision(
            "SKILL-PROFILE",
            "single",
            &["echo"],
            &["tone"],
            Limits::default(),
        ),
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "skills",
                "echo",
                "skill-session",
                "prompt: concise factual",
                "skills",
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let output = task.output.as_deref().unwrap_or_default();
    assert!(output.contains("SKILL-PROFILE"), "{output}");
    assert!(
        output.contains("Prefer short, direct answers."),
        "selected skill body must resolve from the runtime-loaded catalog: {output}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manually_registered_native_composition_is_selectable_without_a_workflow_enum() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-manual");
    fixture::set_agent_tools(&fixture, "general", &[]);
    let model = runtime::native::model_for("deterministic").expect("model");
    let first = Arc::new(
        LlmAgentBuilder::new("manual-first")
            .instruction("MANUAL-FIRST")
            .model(model.clone())
            .build()
            .expect("first leaf"),
    ) as Arc<dyn Agent>;
    let second = Arc::new(
        LlmAgentBuilder::new("manual-second")
            .instruction("MANUAL-SECOND")
            .model(model)
            .build()
            .expect("second leaf"),
    ) as Arc<dyn Agent>;
    let composed =
        Arc::new(SequentialAgent::new("composed", vec![first, second])) as Arc<dyn Agent>;
    let instance = runtime::worker::Worker::new("compose-manual", "0.1.0")
        .agent("composed", composed)
        .start(&fixture.config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "manual",
        revision(
            "MANUAL-PROFILE",
            "single",
            &["composed"],
            &[],
            Limits::default(),
        ),
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "manual",
                "composed",
                "manual-session",
                "MANUAL-INPUT",
                "manual",
            ),
        )
        .await
        .expect("manual composition admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let output = task.output.as_deref().unwrap_or_default();
    assert!(output.contains("MANUAL-FIRST"), "{output}");
    assert!(output.contains("MANUAL-SECOND"), "{output}");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_handoff_transfer_is_preserved_and_target_receives_control() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-handoff");
    fixture::set_agent_tools(&fixture, "general", &[]);
    let model = runtime::native::model_for("deterministic").expect("model");
    let coordinator = Arc::new(
        LlmAgentBuilder::new("coordinator")
            .instruction("HANDOFF-COORDINATOR")
            .model(model.clone())
            .build()
            .expect("coordinator"),
    ) as Arc<dyn Agent>;
    let target = Arc::new(
        LlmAgentBuilder::new("echo")
            .instruction("HANDOFF-TARGET")
            .model(model)
            .build()
            .expect("target"),
    ) as Arc<dyn Agent>;
    let spec = TeamSpec {
        name: "handoff-team".into(),
        description: "native handoff proof".into(),
        coordinator: "coordinator".into(),
        members: vec![
            TeamMemberSpec::new("coordinator"),
            TeamMemberSpec::new("echo"),
        ],
        relationships: vec![TeamRelationship::new(
            "coordinator",
            "echo",
            RelationshipKind::Handoff,
        )],
        policy: TeamPolicy::default(),
    };
    let team = spec
        .compile(vec![coordinator, target])
        .expect("handoff team compiles natively");
    let instance = runtime::worker::Worker::new("compose-handoff", "0.1.0")
        .agent("handoff-team", Arc::new(team))
        .start(&fixture.config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "handoff",
        revision(
            "HANDOFF-PROFILE",
            "single",
            &["handoff-team"],
            &[],
            Limits::default(),
        ),
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "handoff",
                "handoff-team",
                "handoff-session",
                "handoff:echo",
                "handoff",
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let output = task.output.as_deref().unwrap_or_default();
    assert!(output.contains("handoff:echo"), "{output}");
    let events = events(&runtime, &task.id).await;
    let transfer = events
        .iter()
        .find(|event| event.kind == "task.transfer")
        .expect("durable native transfer record");
    assert_eq!(
        transfer
            .payload
            .get("target")
            .and_then(|value| value.as_str()),
        Some("echo")
    );
    assert_eq!(
        transfer
            .payload
            .get("author")
            .and_then(|value| value.as_str()),
        Some("coordinator")
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn callbacks_and_plugins_run_once_per_child_without_widening_authority() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-callbacks");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    create(
        &runtime,
        &admin,
        "callbacks",
        revision(
            "CALLBACK-PROFILE",
            "sequential",
            &["general", "echo"],
            &[],
            Limits::default(),
        ),
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "callbacks",
                "general",
                "callbacks-session",
                "plugin: cb: CALLBACK",
                "callbacks",
            ),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let output = task.output.as_deref().unwrap_or_default();
    assert_eq!(count(output, "[agent-before]"), 2, "{output}");
    assert_eq!(count(output, "[agent-after]"), 2, "{output}");
    assert_eq!(
        count(output, "[plugin-user]"),
        2,
        "the run-scope plugin observes each child model turn once: {output}"
    );
    assert_eq!(
        count(output, "[plugin-event]"),
        2,
        "one plugin event marker per final child response: {output}"
    );
    assert_eq!(count(output, "agent-before] [agent-before]"), 0, "{output}");

    // A before-agent callback may short-circuit a child, but it cannot widen the
    // profile capability list.
    let mut denied = request(
        "callbacks",
        "manual",
        "callbacks-denied",
        "cb-short",
        "denied",
    );
    denied.agent = "manual".into();
    let error = runtime
        .submit_task(&admin, denied)
        .await
        .expect_err("manual agent is outside the profile capabilities");
    assert_eq!(error.code, "profile.capability_denied");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn composed_agents_are_local_children_not_remote_worker_instances() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "composition-local");
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    assert!(
        instance.gateway_config().await.is_none(),
        "this proof must not require Gateway registration"
    );
    create(
        &runtime,
        &admin,
        "local",
        revision(
            "LOCAL",
            "sequential",
            &["general", "echo"],
            &[],
            Limits::default(),
        ),
    )
    .await;

    let submitted = runtime
        .submit_task(
            &admin,
            request("local", "general", "local-session", "LOCAL-INPUT", "local"),
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    assert_eq!(task.team, None);
    assert_eq!(task.native_a2a_task_id, None);
    assert_eq!(instance.manifest().name, "general");
    let capabilities: Vec<_> = instance
        .manifest()
        .capabilities
        .iter()
        .map(|capability| capability.id.clone())
        .collect();
    assert!(
        !capabilities.iter().any(|id| id == "coordinator"),
        "internal team coordinator must not become a public agent: {capabilities:?}"
    );
    assert!(
        !capabilities.iter().any(|id| id == "team"),
        "composition root must not become a public agent: {capabilities:?}"
    );
    let output = task.output.as_deref().unwrap_or_default();
    assert!(output.contains("LOCAL-INPUT"), "{output}");
    instance.shutdown().await;
}
