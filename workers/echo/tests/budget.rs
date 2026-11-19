//! The independently built Echo package uses the same shared runtime budget
//! enforcement as General. Only the executable capability differs.

use common::{Limits, StreamEvent, SubmitTaskRequest, Task, TaskState};
use std::path::Path;
use std::time::{Duration, Instant};

fn write_fixture(data: &Path) -> std::path::PathBuf {
    let assets = data.join("assets");
    let agents = assets.join("agents");
    let prompts = assets.join("prompts");
    let skills = assets.join("skills");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::create_dir_all(&skills).expect("skill directory");
    std::fs::write(
        agents.join("echo.toml"),
        "name = \"echo\"\ndescription = \"Echo budget agent\"\nmodel = \"deterministic\"\nprompt = \"echo\"\ntools = [\"echo_text\"]\n",
    )
    .expect("agent definition");
    std::fs::write(prompts.join("echo.md"), "Return the requested text once.").expect("prompt");
    let config = data.join("worker.toml");
    std::fs::write(
        &config,
        format!(
            "instance = \"echo-budget\"\nnamespace = \"local\"\ndefault_agent = \"echo\"\ntoken = \"dev-token\"\ntoken_scopes = [\"*\"]\n\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\nskills = \"assets/skills\"\n\n[storage]\ndirectory = \"{}\"\n",
            data.display()
        ),
    )
    .expect("worker config");
    config
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

async fn events(runtime: &runtime::Runtime, task_id: &str) -> Vec<StreamEvent> {
    runtime
        .events_since(task_id, 0, 10_000)
        .await
        .expect("events")
        .events
}

#[tokio::test(flavor = "multi_thread")]
async fn independently_built_echo_package_uses_shared_budget_enforcement() {
    let data = tempfile::tempdir().expect("data");
    let config = write_fixture(data.path());
    let instance = echo::worker()
        .expect("echo package definition")
        .start(&config)
        .await
        .expect("echo worker starts");
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    let submitted = runtime
        .submit_task(
            &admin,
            SubmitTaskRequest {
                namespace: Some("local".into()),
                agent: "echo".into(),
                session_id: "echo-budget".into(),
                input: r#"tool-loop:2:echo_text {"text":"hello"}"#.into(),
                idempotency_key: "echo-budget".into(),
                profile: None,
                team: None,
                worker: None,
                limits: Limits {
                    max_tool_calls: 1,
                    ..Limits::default()
                },
            },
        )
        .await
        .expect("task admitted");
    let task = wait(&runtime, &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(
        task.error.as_ref().map(|error| error.code.as_str()),
        Some("budget.tool_calls_exceeded"),
        "{task:?}"
    );
    assert_eq!(task.usage.tool_calls, 1, "{task:?}");

    let events = events(&runtime, &task.id).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "tool.result")
            .count(),
        1,
        "only the allowed Echo tool call may execute: {events:?}"
    );
    let exhausted = events
        .iter()
        .find(|event| event.kind == "budget.exhausted")
        .expect("durable budget policy event");
    assert_eq!(exhausted.payload["kind"], "tool_calls", "{exhausted:?}");
    instance.shutdown().await;
}
