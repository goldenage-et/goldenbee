//! Configured memory-file assembly, context manifests, manual compaction, and
//! pre-admission budget checks through the packaged General Worker.

use adk_rust::session::GetRequest;
use common::{Limits, ProfileRevision, StreamEvent, SubmitTaskRequest, Task, TaskState};
use runtime::worker::WorkerInstance;
use std::path::Path;
use std::time::{Duration, Instant};

mod fixture;

fn append_config(config: &Path, text: &str) {
    let mut existing = std::fs::read_to_string(config).expect("worker config");
    existing.push_str(text);
    std::fs::write(config, existing).expect("worker config");
}

fn memory_config() -> &'static str {
    "\n[context]\nmax_memory_bytes = 65536\nmax_context_bytes = 131072\n[[context.memory]]\nid = \"project\"\npaths = [\"MEMORY.md\"]\nprecedence = 10\nimports = true\n"
}

fn request(session: &str, input: String, key: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: "general".into(),
        session_id: session.into(),
        input,
        idempotency_key: key.into(),
        profile: None,
        team: None,
        worker: None,
        limits: Limits::default(),
    }
}

fn revision(limits: Limits) -> ProfileRevision {
    ProfileRevision {
        revision: 0,
        instruction: "Answer from configured context.".into(),
        model: "deterministic".into(),
        capabilities: vec!["general".into()],
        grants: vec!["task:submit".into()],
        restrictions: Vec::new(),
        composition: "single".into(),
        members: vec!["general".into()],
        skills: Vec::new(),
        mcp: Vec::new(),
        workflow: None,
        limits,
        compaction: None,
        secret_refs: Vec::new(),
        reasoning: None,
        created_at: 0,
    }
}

async fn start(config: &Path) -> WorkerInstance {
    general::worker()
        .expect("package definition")
        .start(config)
        .await
        .expect("worker starts")
}

async fn wait(runtime: &runtime::Runtime, task_id: &str) -> Task {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let task = runtime.task_unscoped(task_id).await.expect("task exists");
        if task.state.is_terminal() {
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn manifests(runtime: &runtime::Runtime, task_id: &str) -> Vec<serde_json::Value> {
    runtime
        .events_since(task_id, 0, 10_000)
        .await
        .expect("events")
        .events
        .into_iter()
        .filter(|event| event.kind == "context.manifest")
        .map(|event: StreamEvent| event.payload)
        .collect()
}

async fn contents(instance: &WorkerInstance, principal: &str, session: &str) -> Vec<String> {
    let user =
        runtime::native::executor::native_user_id("local", principal).expect("native identity");
    let session = instance
        .session_service()
        .get(GetRequest {
            app_name: "general".into(),
            user_id: user,
            session_id: session.into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("canonical session");
    session
        .events()
        .all()
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| {
            content.parts.iter().filter_map(|part| match part {
                adk_rust::Part::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_memory_reaches_the_instruction_and_manifest_survives_restart() {
    let data = tempfile::tempdir().expect("data directory");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "context-memory".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    std::fs::write(
        data.path().join("MEMORY.md"),
        "Runtime memory marker.\n@NESTED.md\n",
    )
    .expect("memory file");
    std::fs::write(data.path().join("NESTED.md"), "Nested memory marker.").expect("nested memory");
    append_config(&fixture.config, memory_config());

    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    let first = runtime
        .submit_task(
            &admin,
            request("memory-session", "prompt:".into(), "memory-1"),
        )
        .await
        .expect("task admitted");
    let first = wait(&runtime, &first.task_id).await;
    assert_eq!(first.state, TaskState::Succeeded, "{first:?}");
    let output = first.output.clone().unwrap_or_default();
    assert!(
        output.contains("Runtime memory marker"),
        "configured memory must reach the agent instruction: {output}"
    );
    assert!(
        output.contains("Nested memory marker"),
        "memory imports must expand: {output}"
    );

    let manifest = manifests(&runtime, &first.id)
        .await
        .pop()
        .expect("context manifest is durable");
    assert_eq!(manifest["memory_files"][0]["source"], "project");
    assert_eq!(manifest["memory_files"][1]["imported"], true);
    assert!(manifest["memory_config_digest"]
        .as_str()
        .is_some_and(|value| value.starts_with("sha256:")));
    assert!(manifest["memory_digest"]
        .as_str()
        .is_some_and(|value| value.starts_with("sha256:")));
    assert!(manifest["instruction_digest"]
        .as_str()
        .is_some_and(|value| value.starts_with("sha256:")));
    assert_eq!(manifest["memory_diagnostics"], serde_json::json!([]));
    let digest = manifest["digest"].clone();

    instance.shutdown().await;
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    let second = runtime
        .submit_task(
            &admin,
            request("memory-session-2", "prompt:".into(), "memory-2"),
        )
        .await
        .expect("task admitted after restart");
    let second = wait(&runtime, &second.task_id).await;
    assert_eq!(second.state, TaskState::Succeeded, "{second:?}");
    let manifest = manifests(&runtime, &second.id)
        .await
        .pop()
        .expect("context manifest after restart");
    assert_eq!(
        manifest["digest"], digest,
        "same configuration and profile revision must produce the same context manifest"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compaction_is_scheduled_for_the_next_turn_and_keeps_canonical_history() {
    let data = tempfile::tempdir().expect("data directory");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "context-manual-compaction".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    let mut last = None;
    for turn in 1..=6 {
        let input = format!("manual-turn-{turn}:{}", "x".repeat(120));
        let submitted = runtime
            .submit_task(
                &admin,
                request("manual-compaction", input, &format!("manual-{turn}")),
            )
            .await
            .expect("task admitted");
        let task = wait(&runtime, &submitted.task_id).await;
        assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
        last = Some(task);
    }
    assert!(last.is_some());

    let scheduled = runtime
        .request_session_compaction(&admin, "manual-compaction")
        .await
        .expect("manual compaction request accepted");
    assert_eq!(scheduled.state, "scheduled");
    assert_eq!(scheduled.applies_on, "next_model_turn");
    assert!(!scheduled.manual_entry_point);

    let submitted = runtime
        .submit_task(
            &admin,
            request(
                "manual-compaction",
                "manual-turn-after".into(),
                "manual-after",
            ),
        )
        .await
        .expect("next turn admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");

    let events = runtime
        .events_since(&task.id, 0, 10_000)
        .await
        .expect("events")
        .events;
    let compaction = events
        .iter()
        .find(|event| event.kind == "context.compacted")
        .expect("manual compaction must use the native compaction event");
    assert_eq!(compaction.payload["forced"], true, "{compaction:?}");
    assert_eq!(
        compaction.payload["manual_compaction"], true,
        "{compaction:?}"
    );
    assert!(
        compaction.payload["events_after"].as_u64() < compaction.payload["events_before"].as_u64(),
        "manual compaction must reduce active context: {compaction:?}"
    );
    let manifest = manifests(&runtime, &task.id).await.pop().expect("manifest");
    assert_eq!(manifest["forced_compaction"], true);
    assert_eq!(manifest["manual_compaction"], "next_model_turn");

    let text = contents(&instance, "local", "manual-compaction").await;
    let text = text.join("\n");
    for turn in 1..=6 {
        assert!(
            text.contains(&format!("manual-turn-{turn}")),
            "canonical session history must retain turn {turn}"
        );
    }
    assert!(text.contains("manual-turn-after"));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cost_budget_without_operator_pricing_is_rejected_before_admission() {
    let data = tempfile::tempdir().expect("data directory");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "context-cost-budget".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let instance = start(&fixture.config).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    let error = runtime
        .create_profile(
            &admin,
            "local",
            "cost-budget",
            revision(Limits {
                max_cost_micros: 1,
                ..Limits::default()
            }),
        )
        .await
        .expect_err("unpriceable cost budget must be rejected");
    assert_eq!(error.code, "budget.cost_unverifiable");
    assert!(runtime.tasks(&admin, None).await.expect("tasks").is_empty());
    assert!(runtime
        .profiles(&admin, "local")
        .await
        .expect("profiles")
        .iter()
        .all(|profile| profile.name != "cost-budget"));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_required_memory_file_fails_startup_without_silent_omission() {
    let data = tempfile::tempdir().expect("data directory");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "context-missing-memory".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    append_config(&fixture.config, memory_config());
    let error = match general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
    {
        Ok(_) => panic!("missing required memory must fail startup"),
        Err(error) => error,
    };
    assert_eq!(error.code, "context.memory_missing");
}
