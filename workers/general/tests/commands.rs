//! Neutral and Worker-defined command behavior through a real Worker instance.
//!
//! These tests assert discovery, argument validation, model/tool narrowing, and
//! the absence of a widened capability. They never inspect the command loader's
//! private structures.

use common::{CommandRequest, Limits, Principal, Task, TaskState};
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

fn write_command(fixture: &fixture::Fixture, name: &str, body: &str) {
    std::fs::write(fixture.commands.join(format!("{name}.toml")), body).expect("command asset");
}

fn request(input: Option<&str>, session: &str, key: &str) -> CommandRequest {
    CommandRequest {
        input: input.map(str::to_string),
        arguments: Default::default(),
        session_id: session.into(),
        idempotency_key: key.into(),
        agent: None,
        limits: Limits::default(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn neutral_commands_are_discoverable_and_read_durable_state() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "commands-neutral");
    let instance = start(&fixture.config).await;
    let admin: Principal = instance.operator_principal().await.expect("operator");

    let descriptors = instance.command_descriptors().await;
    for expected in [
        "commands.list",
        "context.info",
        "permissions.info",
        "session.info",
        "session.list",
        "skills.list",
    ] {
        assert!(
            descriptors.iter().any(|command| command.name == expected),
            "neutral command '{expected}' must be discoverable: {descriptors:?}"
        );
    }

    let context = instance
        .execute_command(&admin, "context.info", request(None, "neutral-session", ""))
        .await
        .expect("context info");
    assert_eq!(context.kind, "neutral");
    assert!(context.task.is_none());
    assert_eq!(context.result["default_agent"], "general");
    assert!(context.result["skills"]
        .as_array()
        .expect("skills")
        .iter()
        .any(|skill| skill["name"] == "tone"));

    let permissions = instance
        .execute_command(
            &admin,
            "permissions.info",
            request(None, "neutral-session", ""),
        )
        .await
        .expect("permissions info");
    assert_eq!(permissions.result["mode"], "package");
    assert!(permissions.result["instance_grants"]
        .as_array()
        .expect("grants")
        .iter()
        .any(|grant| grant == "*"));

    let listed = instance
        .execute_command(&admin, "session.list", request(None, "neutral-session", ""))
        .await
        .expect("session list");
    assert_eq!(
        listed.result["sessions"]
            .as_array()
            .expect("sessions")
            .len(),
        0
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_command_expands_arguments_and_submits_a_narrowed_turn() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "commands-worker");
    write_command(
        &fixture,
        "summarize",
        "description = \"Summarize a topic\"\nprompt = \"COMMAND-PROMPT {{args}}\"\nallowed_tools = [\"save_artifact\"]\n",
    );
    let instance = start(&fixture.config).await;
    let admin = instance.operator_principal().await.expect("operator");

    let descriptor = instance
        .command_descriptors()
        .await
        .into_iter()
        .find(|command| command.name == "summarize")
        .expect("command discovered");
    assert_eq!(descriptor.kind, "worker");
    assert_eq!(descriptor.allowed_tools, vec!["save_artifact"]);

    let response = instance
        .execute_command(
            &admin,
            "summarize",
            request(
                Some("prompt: release notes"),
                "command-session",
                "command-1",
            ),
        )
        .await
        .expect("command admitted");
    assert_eq!(response.kind, "worker");
    let submitted = response.task.expect("command submits a task");
    let task = wait(&instance.runtime(), &submitted.task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let output = task.output.as_deref().unwrap_or_default();
    assert!(
        output.contains("prompt=COMMAND-PROMPT prompt: release notes"),
        "the command prompt must be the native instruction: {output}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_command_arguments_never_submit_a_task() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "commands-invalid");
    write_command(
        &fixture,
        "release",
        "prompt = \"{{topic}} in {{style}}\"\n\n[[arguments]]\nname = \"topic\"\nrequired = true\n\n[[arguments]]\nname = \"style\"\ndefault = \"brief\"\n",
    );
    let instance = start(&fixture.config).await;
    let admin = instance.operator_principal().await.expect("operator");

    let error = instance
        .execute_command(
            &admin,
            "release",
            request(Some("--style long"), "command-invalid", "command-invalid-1"),
        )
        .await
        .expect_err("missing required argument");
    assert_eq!(error.code, "command.arguments_missing");
    assert!(
        instance
            .runtime()
            .tasks(&admin, None)
            .await
            .expect("tasks")
            .is_empty(),
        "malformed input must not admit a task"
    );

    let error = instance
        .execute_command(
            &admin,
            "release",
            request(
                Some("--topic releases --unknown x"),
                "command-invalid",
                "command-invalid-2",
            ),
        )
        .await
        .expect_err("unknown argument");
    assert_eq!(error.code, "command.argument_unknown");
    assert!(instance
        .runtime()
        .tasks(&admin, None)
        .await
        .expect("tasks")
        .is_empty());
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn command_allowed_tools_cannot_expose_a_tool_the_target_did_not_bind() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "commands-tools");
    write_command(
        &fixture,
        "leak",
        "prompt = \"LEAK {{args}}\"\nallowed_tools = [\"save_artifact\"]\n",
    );
    let instance = start(&fixture.config).await;
    let admin = instance.operator_principal().await.expect("operator");

    let response = instance
        .execute_command(
            &admin,
            "leak",
            request(
                Some("call:exec_command {\"command\":\"printf COMMAND-LEAK\"}"),
                "command-leak",
                "command-leak-1",
            ),
        )
        .await
        .expect("command admitted");
    let task = wait(&instance.runtime(), &response.task.expect("task").task_id).await;
    let visible = format!(
        "{} {}",
        task.output.as_deref().unwrap_or_default(),
        task.error
            .as_ref()
            .map(|error| error.message.clone())
            .unwrap_or_default()
    );
    assert!(
        !visible.contains("COMMAND-LEAK"),
        "a command must not expose a tool the target agent did not bind: {task:?}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn undeclared_tools_models_and_manual_targets_fail_startup() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "commands-declared");
    write_command(
        &fixture,
        "widened",
        "prompt = \"{{args}}\"\nallowed_tools = [\"exec_command\"]\nagent = \"echo\"\n",
    );
    let error = general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
        .err()
        .expect("echo does not declare exec_command");
    assert_eq!(error.code, "asset.command_tool_unknown");

    std::fs::write(
        fixture.commands.join("widened.toml"),
        "prompt = \"{{args}}\"\nmodel = \"undeclared-model\"\n",
    )
    .expect("command");
    let error = general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
        .err()
        .expect("undeclared model");
    assert_eq!(error.code, "asset.command_model_unknown");

    std::fs::write(
        fixture.commands.join("widened.toml"),
        "prompt = \"{{args}}\"\nagent = \"manual\"\n",
    )
    .expect("command");
    let error = general::worker()
        .expect("package")
        .start(&fixture.config)
        .await
        .err()
        .expect("manual agent is unsupported");
    assert_eq!(error.code, "asset.command_agent_unsupported");
}

#[tokio::test(flavor = "multi_thread")]
async fn command_agent_override_must_match_the_declared_target() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture(data.path(), "commands-agent");
    write_command(&fixture, "summarize", "prompt = \"{{args}}\"\n");
    let instance = start(&fixture.config).await;
    let admin = instance.operator_principal().await.expect("operator");
    let mut override_request = request(Some("hello"), "command-agent", "command-agent-1");
    override_request.agent = Some("echo".into());
    let error = instance
        .execute_command(&admin, "summarize", override_request)
        .await
        .expect_err("agent override must not retarget a command");
    assert_eq!(error.code, "command.agent_mismatch");
    instance.shutdown().await;
}
