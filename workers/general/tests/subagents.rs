//! Runtime-local subagent lifecycle through the real General Worker runtime.
//!
//! Every case drives the pinned native Runner with a deterministic model and
//! observes durable task projections and events. The subagent tools are the
//! real runtime tools; nothing here reaches into executor internals.

use adk_rust::{Tool, ToolContext};
use async_trait::async_trait;
use common::{Limits, ProfileRevision, SubmitTaskRequest, Task, TaskState};
use runtime::policy::PermissionMode;
use runtime::subagents::{SubagentConfig, SubagentDefinition};
use runtime::worker::{ToolDescriptor, ToolRisk, WorkerInstance};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod fixture;

fn default_subagents() -> SubagentConfig {
    SubagentConfig::new()
        .max_depth(1)
        .max_fanout(4)
        .permission_mode(PermissionMode::Default)
        .definition(
            "helper",
            SubagentDefinition::new("echo").description("Deterministic helper."),
        )
        .definition(
            "researcher",
            SubagentDefinition::new("general").description("General specialist."),
        )
}

fn fixture_options(instance: &str) -> fixture::Options {
    fixture::Options {
        instance: instance.into(),
        token: Some("dev-token".into()),
        model: "deterministic".into(),
        principals: Vec::new(),
        settings: Vec::new(),
        token_scopes: vec!["*".into()],
    }
}

fn parent_request(input: &str, key: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: "general".into(),
        session_id: format!("session-{key}"),
        input: input.into(),
        idempotency_key: key.into(),
        profile: None,
        team: None,
        worker: None,
        limits: Limits::default(),
    }
}

async fn start(config: &Path, subagents: SubagentConfig) -> WorkerInstance {
    general::worker_with_subagents(subagents)
        .expect("package definition")
        .start(config)
        .await
        .expect("worker starts")
}

/// A package-style tool whose descriptor declares the command it runs.
///
/// The built-in package `exec_command` descriptor is deliberately generic, so
/// it cannot show the permission-mode ceiling. This tool lets the real kernel
/// evaluate an observed high-risk command without a second executor.
struct HighRiskTool;

#[async_trait]
impl Tool for HighRiskTool {
    fn name(&self) -> &str {
        "high_risk_tool"
    }

    fn description(&self) -> &str {
        "Run one declared high-risk command."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        Ok(serde_json::json!({"ran": args}))
    }
}

async fn start_with_high_risk_tool(config: &Path, subagents: SubagentConfig) -> WorkerInstance {
    general::worker_with_subagents(subagents)
        .expect("package definition")
        .tool_with_descriptor(
            "high_risk_tool",
            Arc::new(HighRiskTool),
            ToolDescriptor::new("process:execute")
                .risk(ToolRisk::High)
                .command(|args| {
                    args.get("command")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }),
            ["process:execute"],
        )
        .start(config)
        .await
        .expect("worker starts")
}

async fn wait_terminal(runtime: &runtime::Runtime, task_id: &str) -> Task {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let task = runtime
            .task_unscoped(task_id)
            .await
            .expect("task remains visible");
        if task.state.is_terminal() {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "task {task_id} stayed in {:?}",
            task.state
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

async fn wait_state(runtime: &runtime::Runtime, task_id: &str, wanted: TaskState) -> Task {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let task = runtime
            .task_unscoped(task_id)
            .await
            .expect("task remains visible");
        if task.state == wanted {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "task {task_id} stayed in {:?}, wanted {wanted:?}",
            task.state
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

async fn children(runtime: &runtime::Runtime, parent_id: &str) -> Vec<Task> {
    runtime
        .all_tasks()
        .await
        .into_iter()
        .filter(|task| task.parent_task_id.as_deref() == Some(parent_id))
        .collect()
}

async fn events(runtime: &runtime::Runtime, task_id: &str) -> Vec<common::StreamEvent> {
    runtime
        .events_since(task_id, 0, 2_000)
        .await
        .expect("task events")
        .events
}

fn kinds(events: &[common::StreamEvent]) -> Vec<&str> {
    events.iter().map(|event| event.kind.as_str()).collect()
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("JSON string")
}

fn spawn_call(definition: &str, input: &str, extra: &str) -> String {
    format!(
        "call:spawn_subagent {{\"definition\":{},\"input\":{}{extra}}}",
        json_string(definition),
        json_string(input)
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn background_spawn_returns_before_completion_and_reports_terminal_once() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), fixture_options("subagent-background"));
    fixture::set_agent_tools(
        &fixture,
        "general",
        &["spawn_subagent", "wait_subagent", "list_subagents"],
    );
    let instance = start(&fixture.config, default_subagents()).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    let submitted = runtime
        .submit_task(
            &admin,
            parent_request(
                &spawn_call("helper", "child:bg", ",\"background\":true"),
                "parent-background",
            ),
        )
        .await
        .expect("parent admitted");
    let parent = wait_terminal(&runtime, &submitted.task_id).await;
    assert_eq!(parent.state, TaskState::Succeeded);
    let output = parent.output.clone().unwrap_or_default();
    // The parent finished while the child was still running: the spawn result
    // is a non-terminal handle, not a fabricated success.
    assert!(output.contains("\"terminal\":false"), "{output}");
    assert!(output.contains("\"background\":true"), "{output}");
    assert!(output.contains("\"task_id\":\"task_"), "{output}");
    assert_eq!(children(&runtime, &parent.id).await.len(), 1);
    let child = children(&runtime, &parent.id).await.remove(0);

    let child = wait_terminal(&runtime, &child.id).await;
    assert_eq!(child.state, TaskState::Succeeded);
    assert!(child
        .output
        .as_deref()
        .unwrap_or_default()
        .contains("child-output:bg"));
    assert_eq!(child.root_task_id.as_deref(), Some(parent.id.as_str()));
    assert_eq!(
        child
            .subagent
            .as_ref()
            .map(|policy| policy.depth)
            .unwrap_or_default(),
        1
    );

    // The completion notification is durable and exactly once.
    let deadline = Instant::now() + Duration::from_secs(5);
    let terminal = loop {
        let events = events(&runtime, &parent.id).await;
        let found: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "subagent.terminal")
            .cloned()
            .collect();
        if !found.is_empty() {
            break found;
        }
        assert!(Instant::now() < deadline, "no terminal notification");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].payload["task_id"], child.id);
    assert_eq!(terminal[0].payload["state"], "succeeded");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        events(&runtime, &parent.id)
            .await
            .iter()
            .filter(|event| event.kind == "subagent.terminal")
            .count(),
        1,
        "a second terminal notification would duplicate the result"
    );
    let parent_events = events(&runtime, &parent.id).await;
    assert_eq!(
        parent_events
            .iter()
            .filter(|event| event.kind == "subagent.spawned")
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn foreground_spawn_uses_reserved_capacity_and_releases_the_parent_slot() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), fixture_options("subagent-reserved"));
    // One execution slot and one principal slot. The child is admitted anyway
    // because child admission uses reserved capacity, and it can execute
    // because a waiting parent releases its slot.
    fixture::set_top_level(&fixture, "max_active = 1\nper_principal = 1\nmax_queue = 8");
    fixture::set_agent_tools(&fixture, "general", &["spawn_subagent"]);
    let instance = start(&fixture.config, default_subagents()).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    let submitted = runtime
        .submit_task(
            &admin,
            parent_request(
                &spawn_call("helper", "child:fg", ",\"background\":false"),
                "parent-foreground",
            ),
        )
        .await
        .expect("parent admitted");
    let parent = wait_terminal(&runtime, &submitted.task_id).await;
    assert_eq!(parent.state, TaskState::Succeeded);
    let output = parent.output.unwrap_or_default();
    assert!(output.contains("\"state\":\"succeeded\""), "{output}");
    assert!(output.contains("\"terminal\":true"), "{output}");
    assert!(output.contains("child-output:fg"), "{output}");

    let children = children(&runtime, &parent.id).await;
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].state, TaskState::Succeeded);
}

#[tokio::test(flavor = "multi_thread")]
async fn depth_and_fanout_violations_are_typed_before_admission() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), fixture_options("subagent-bounds"));
    fixture::set_agent_tools(
        &fixture,
        "general",
        &["spawn_subagent", "wait_subagent", "list_subagents"],
    );
    let instance = start(&fixture.config, default_subagents().max_fanout(1)).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    // The second live child is denied before admission, and the parent learns
    // the exact typed reason.
    let script = "steps:[{\"tool\":\"spawn_subagent\",\"args\":{\"definition\":\"helper\",\"input\":\"delay:900 child:first\",\"background\":true}},{\"tool\":\"spawn_subagent\",\"args\":{\"definition\":\"helper\",\"input\":\"second\",\"background\":true}}]";
    let submitted = runtime
        .submit_task(&admin, parent_request(script, "parent-fanout"))
        .await
        .expect("parent admitted");
    let parent = wait_terminal(&runtime, &submitted.task_id).await;
    assert_eq!(parent.state, TaskState::Succeeded);
    let output = parent.output.clone().unwrap_or_default();
    assert!(output.contains("subagent.fanout_exceeded"), "{output}");
    assert_eq!(
        children(&runtime, &parent.id).await.len(),
        1,
        "a denied spawn must not create a task"
    );

    // A child at the depth ceiling cannot spawn again.
    let submitted = runtime
        .submit_task(
            &admin,
            parent_request(
                &spawn_call(
                    "researcher",
                    &spawn_call("helper", "grandchild", ""),
                    ",\"background\":false",
                ),
                "parent-depth",
            ),
        )
        .await
        .expect("parent admitted");
    let parent = wait_terminal(&runtime, &submitted.task_id).await;
    assert_eq!(parent.state, TaskState::Succeeded);
    let output = parent.output.unwrap_or_default();
    assert!(output.contains("subagent.depth_exceeded"), "{output}");
    let children = children(&runtime, &parent.id).await;
    assert_eq!(children.len(), 1);
    assert_eq!(
        children[0]
            .subagent
            .as_ref()
            .map(|policy| policy.depth)
            .unwrap_or_default(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn child_policy_narrows_agent_tools_model_and_permission() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), fixture_options("subagent-narrow"));
    fixture::set_agent_tools(
        &fixture,
        "general",
        &[
            "spawn_subagent",
            "wait_subagent",
            "list_subagents",
            "exec_command",
            "high_risk_tool",
        ],
    );
    let subagents = default_subagents()
        .definition(
            "restricted",
            SubagentDefinition::new("general").tools(["list_subagents"]),
        )
        .definition(
            "planned",
            SubagentDefinition::new("general").permission_mode(PermissionMode::Plan),
        );
    let instance = start_with_high_risk_tool(&fixture.config, subagents).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    // The helper selects the echo base agent instead of the parent's agent.
    let submitted = runtime
        .submit_task(
            &admin,
            parent_request(
                &spawn_call("helper", "child-agent-check", ",\"background\":false"),
                "narrow-agent",
            ),
        )
        .await
        .expect("parent admitted");
    let parent = wait_terminal(&runtime, &submitted.task_id).await;
    let child = children(&runtime, &parent.id).await.remove(0);
    assert_eq!(child.agent, "echo");

    // The frozen allowlist denies a tool the child's agent binds.
    let submitted = runtime
        .submit_task(
            &admin,
            parent_request(
                &spawn_call(
                    "restricted",
                    r#"call:exec_command {"command":"true"}"#,
                    ",\"background\":false",
                ),
                "narrow-tools",
            ),
        )
        .await
        .expect("parent admitted");
    let parent = wait_terminal(&runtime, &submitted.task_id).await;
    let output = parent.output.unwrap_or_default();
    assert!(output.contains("frozen tool allowlist"), "{output}");
    let child = children(&runtime, &parent.id).await.remove(0);
    assert_eq!(
        child
            .subagent
            .as_ref()
            .and_then(|policy| policy.tools.clone()),
        Some(vec!["list_subagents".to_string()])
    );

    // The permission ceiling denies a high-risk command at the policy layer.
    let submitted = runtime
        .submit_task(
            &admin,
            parent_request(
                &spawn_call(
                    "planned",
                    r#"call:high_risk_tool {"command":"true"}"#,
                    ",\"background\":false",
                ),
                "narrow-mode",
            ),
        )
        .await
        .expect("parent admitted");
    let parent = wait_terminal(&runtime, &submitted.task_id).await;
    let output = parent.output.unwrap_or_default();
    assert!(output.contains("mode_plan_ceiling"), "{output}");
    let child = children(&runtime, &parent.id).await.remove(0);
    assert_eq!(
        child
            .subagent
            .as_ref()
            .and_then(|policy| policy.permission_mode.clone()),
        Some("plan".to_string())
    );

    // Widening attempts are rejected before any child is admitted.
    for (extra, code) in [
        (
            ",\"permission_mode\":\"full-auto\"",
            "subagent.permission_widened",
        ),
        (",\"model\":\"echo\"", "subagent.model_widened"),
    ] {
        let submitted = runtime
            .submit_task(
                &admin,
                parent_request(
                    &spawn_call("helper", "widening", extra),
                    &format!("widen-{code}"),
                ),
            )
            .await
            .expect("parent admitted");
        let parent = wait_terminal(&runtime, &submitted.task_id).await;
        let output = parent.output.unwrap_or_default();
        assert!(output.contains(code), "{output}");
        assert!(
            children(&runtime, &parent.id).await.is_empty(),
            "a widening denial must not admit a child"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn parent_cancellation_cascades_to_children_within_the_drain_boundary() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), fixture_options("subagent-cancel"));
    fixture::set_agent_tools(
        &fixture,
        "general",
        &["spawn_subagent", "wait_subagent", "exec_command"],
    );
    let instance = start(&fixture.config, default_subagents()).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    let script = "steps:[{\"tool\":\"spawn_subagent\",\"args\":{\"definition\":\"researcher\",\"input\":\"sandbox:sleep 5\",\"background\":true}},{\"tool\":\"wait_subagent\",\"args\":{\"task_id\":\"$last.task_id\",\"timeout_ms\":8000}}]";
    let submitted = runtime
        .submit_task(&admin, parent_request(script, "parent-cancel"))
        .await
        .expect("parent admitted");
    let parent_id = submitted.task_id.clone();
    let deadline = Instant::now() + Duration::from_secs(10);
    let child_id = loop {
        if let Some(child) = children(&runtime, &parent_id).await.into_iter().next() {
            break child.id;
        }
        assert!(Instant::now() < deadline, "child was never admitted");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    wait_state(&runtime, &child_id, TaskState::Running).await;

    let started = Instant::now();
    runtime
        .cancel_task(&admin, &parent_id)
        .await
        .expect("parent cancelled");
    let child = wait_terminal(&runtime, &child_id).await;
    let parent = wait_terminal(&runtime, &parent_id).await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "cascade exceeded the drain boundary: {:?}",
        started.elapsed()
    );
    assert_eq!(parent.state, TaskState::Cancelled);
    assert_eq!(child.state, TaskState::Cancelled);
}

#[tokio::test(flavor = "multi_thread")]
async fn follow_up_reuses_the_child_session_and_close_denies_more_input() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), fixture_options("subagent-follow-up"));
    fixture::set_agent_tools(
        &fixture,
        "general",
        &[
            "spawn_subagent",
            "wait_subagent",
            "send_input",
            "close_subagent",
            "list_subagents",
        ],
    );
    let instance = start(&fixture.config, default_subagents()).await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");

    let script = "steps:[\
        {\"tool\":\"spawn_subagent\",\"args\":{\"definition\":\"helper\",\"input\":\"child:first\",\"background\":true}},\
        {\"tool\":\"wait_subagent\",\"args\":{\"task_id\":\"$last.task_id\"}},\
        {\"tool\":\"send_input\",\"args\":{\"task_id\":\"$last.task_id\",\"input\":\"child:second\",\"background\":true}},\
        {\"tool\":\"wait_subagent\",\"args\":{\"task_id\":\"$last.task_id\"}},\
        {\"tool\":\"close_subagent\",\"args\":{\"task_id\":\"$last.task_id\"}},\
        {\"tool\":\"send_input\",\"args\":{\"task_id\":\"$last.task_id\",\"input\":\"child:third\",\"background\":true}}\
    ]";
    let submitted = runtime
        .submit_task(&admin, parent_request(script, "parent-follow-up"))
        .await
        .expect("parent admitted");
    let parent = wait_terminal(&runtime, &submitted.task_id).await;
    assert_eq!(parent.state, TaskState::Succeeded);
    let output = parent.output.clone().unwrap_or_default();
    assert!(output.contains("subagent.closed"), "{output}");

    let mut children = children(&runtime, &parent.id).await;
    children.sort_by_key(|task| task.created_at);
    assert_eq!(children.len(), 2, "one spawn and one follow-up");
    assert_eq!(children[0].session_id, children[1].session_id);
    assert_eq!(
        children[0]
            .subagent
            .as_ref()
            .map(|policy| &policy.definition),
        children[1]
            .subagent
            .as_ref()
            .map(|policy| &policy.definition)
    );
    assert_eq!(children[0].state, TaskState::Succeeded);
    assert_eq!(children[1].state, TaskState::Succeeded);
    assert!(children[1].closed_at.is_some());
    assert!(children[0].closed_at.is_none());

    let parent_events = events(&runtime, &parent.id).await;
    let kinds = kinds(&parent_events);
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "subagent.spawned")
            .count(),
        1
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "subagent.continued")
            .count(),
        1
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "subagent.terminal")
            .count(),
        2
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "subagent.closed")
            .count(),
        1
    );

    // The default listing hides the closed child; the caller can still ask
    // for it explicitly.
    assert_eq!(runtime.subagents_of(&parent.id, false).await.len(), 1);
    assert_eq!(runtime.subagents_of(&parent.id, true).await.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn durable_parent_linkage_and_child_result_survive_restart() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), fixture_options("subagent-restart"));
    fixture::set_agent_tools(&fixture, "general", &["spawn_subagent"]);
    let instance = start(&fixture.config, default_subagents()).await;
    {
        let runtime = instance.runtime();
        let admin = instance.operator_principal().await.expect("operator");
        let submitted = runtime
            .submit_task(
                &admin,
                parent_request(
                    &spawn_call("helper", "child:durable", ",\"background\":false"),
                    "parent-restart",
                ),
            )
            .await
            .expect("parent admitted");
        let parent = wait_terminal(&runtime, &submitted.task_id).await;
        assert_eq!(parent.state, TaskState::Succeeded);
        let children = children(&runtime, &parent.id).await;
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].state, TaskState::Succeeded);
    }
    instance.shutdown().await;
    drop(instance);

    let restarted = start(&fixture.config, default_subagents()).await;
    let runtime = restarted.runtime();
    let tasks = runtime.all_tasks().await;
    let parent = tasks
        .iter()
        .find(|task| task.idempotency_key == "parent-restart")
        .expect("parent survived restart");
    let child = tasks
        .iter()
        .find(|task| task.parent_task_id.as_deref() == Some(parent.id.as_str()))
        .expect("child survived restart");
    assert_eq!(child.state, TaskState::Succeeded);
    assert_eq!(child.root_task_id.as_deref(), Some(parent.id.as_str()));
    assert!(child
        .output
        .as_deref()
        .unwrap_or_default()
        .contains("child-output:durable"));
    assert_eq!(
        child
            .subagent
            .as_ref()
            .map(|policy| policy.definition.as_str()),
        Some("helper")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn composed_parent_profile_honours_an_allowed_child_model_and_rejects_agent_swap() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(data.path(), fixture_options("subagent-model"));
    fixture::set_agent_tools(&fixture, "general", &["spawn_subagent"]);
    let instance = start(
        &fixture.config,
        default_subagents().models(["deterministic", "echo"]),
    )
    .await;
    let runtime = instance.runtime();
    let admin = instance.operator_principal().await.expect("operator");
    runtime
        .create_profile(
            &admin,
            "local",
            "research",
            ProfileRevision {
                revision: 0,
                instruction: "COMPOSED-PARENT".into(),
                model: "deterministic".into(),
                capabilities: vec!["general".into()],
                grants: vec!["task:submit".into()],
                restrictions: Vec::new(),
                composition: "single".into(),
                members: vec!["general".into()],
                skills: Vec::new(),
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
        .activate_profile(&admin, "local", "research", 1)
        .await
        .expect("profile activated");

    let mut request = parent_request(
        &spawn_call(
            "researcher",
            "child:model",
            ",\"background\":false,\"model\":\"echo\"",
        ),
        "parent-model",
    );
    request.profile = Some("research".into());
    let submitted = runtime
        .submit_task(&admin, request)
        .await
        .expect("parent admitted");
    let parent = wait_terminal(&runtime, &submitted.task_id).await;
    assert_eq!(parent.state, TaskState::Succeeded);
    let child = children(&runtime, &parent.id).await.remove(0);
    assert_eq!(child.state, TaskState::Succeeded);
    assert_eq!(
        child
            .resolved_model
            .as_ref()
            .map(|model| model.provider.as_str()),
        Some("echo")
    );
    assert_eq!(child.profile_revision, parent.profile_revision);
    assert_eq!(
        child
            .subagent
            .as_ref()
            .and_then(|policy| policy.model.clone()),
        Some("echo".to_string())
    );

    // A composed parent runs its own members; swapping the child agent would
    // silently not apply, so the runtime denies it instead.
    let mut request = parent_request(
        &spawn_call("helper", "child:agent", ",\"background\":false"),
        "parent-composed-agent",
    );
    request.profile = Some("research".into());
    let submitted = runtime
        .submit_task(&admin, request)
        .await
        .expect("parent admitted");
    let parent = wait_terminal(&runtime, &submitted.task_id).await;
    let output = parent.output.unwrap_or_default();
    assert!(output.contains("subagent.agent_unsupported"), "{output}");
    assert!(children(&runtime, &parent.id).await.is_empty());
}
