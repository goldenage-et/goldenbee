//! Process, web, image, plan, and question tool parity.
//!
//! Every test starts a real Worker against a temporary configuration, submits a
//! durable task to the deterministic diagnostic model, and observes only
//! external behavior: host files, task state, task events, audit records, and
//! the runtime's scoped management reads. No test calls a tool implementation
//! or kernel internals directly.

use common::{ApprovalDecision, Limits, StreamEvent, SubmitTaskRequest, Task, TaskState};
use runtime::worker::{BuiltinTool, Worker};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn fixture(data: &Path, tools: &[&str], tools_config: &str) -> PathBuf {
    let assets = data.join("assets");
    let agents = assets.join("agents");
    let prompts = assets.join("prompts");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::write(
        agents.join("tools.toml"),
        format!(
            "name = \"tools\"\ndescription = \"process and web parity agent\"\nmodel = \"deterministic\"\nprompt = \"tools\"\ntools = [{}]\n",
            tools
                .iter()
                .map(|tool| format!("\"{tool}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
    .expect("agent definition");
    std::fs::write(prompts.join("tools.md"), "Use the supplied tools.").expect("prompt");
    let config = data.join("worker.toml");
    std::fs::write(
        &config,
        format!(
            "instance = \"parity-{}\"\nnamespace = \"local\"\ndefault_agent = \"tools\"\ntoken = \"dev-token\"\ntoken_scopes = [\"*\"]\n\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\n\n[storage]\ndirectory = \"{}\"\n\n[tools]\n{}\n\n[permissions]\nmode = \"full-auto\"\n",
            data.file_name().unwrap().to_string_lossy(),
            data.display(),
            tools_config
        ),
    )
    .expect("worker config");
    config
}

fn default_tools_config() -> &'static str {
    "workspace = \"workspace\"\ntimeout_ms = 120000\nmax_output_bytes = 1048576\nmax_parallel = 2\n"
}

fn runtime_principal() -> common::Principal {
    common::Principal::new("local", "local", &["*"])
}

async fn submit(runtime: &runtime::Runtime, input: &str, session: &str) -> String {
    let request = SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: "tools".into(),
        session_id: session.into(),
        input: input.into(),
        idempotency_key: format!("{session}-{}", common::canonical_digest(&input)),
        profile: None,
        team: None,
        worker: None,
        limits: Limits::default(),
    };
    runtime
        .submit_task(&runtime_principal(), request)
        .await
        .expect("task admitted")
        .task_id
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

async fn events(runtime: &runtime::Runtime, task_id: &str) -> Vec<StreamEvent> {
    runtime
        .events_since(task_id, 0, 10_000)
        .await
        .expect("events")
        .events
}

async fn run(runtime: &runtime::Runtime, input: &str, session: &str) -> Task {
    let task_id = submit(runtime, input, session).await;
    wait(runtime, &task_id).await
}

fn output(task: &Task) -> String {
    task.output.clone().unwrap_or_default()
}

fn result_json(text: &str) -> Value {
    let start = text
        .find("mcp result ")
        .map(|index| index + 11)
        .unwrap_or(0);
    serde_json::from_str(text[start..].trim()).unwrap_or(Value::Null)
}

#[tokio::test(flavor = "multi_thread")]
async fn task_plan_is_persisted_published_and_survives_restart() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &["write_todos"], default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .builtin_tools([BuiltinTool::WriteTodos])
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let task = run(
        &runtime,
        r#"mcp:write_todos {"todos":[{"content":"inspect","status":"completed"},{"content":"finish","status":"in_progress"}]}"#,
        "plan",
    )
    .await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let result = result_json(&output(&task));
    assert_eq!(result["total"], 2, "{result}");
    assert_eq!(result["remaining"], 1, "{result}");

    let events = events(&runtime, &task.id).await;
    let updated = events
        .iter()
        .find(|event| event.kind == "plan.updated")
        .expect("plan update event");
    assert_eq!(updated.payload["total"], 2);
    assert_eq!(updated.payload["items"][1]["content"], "finish");
    assert_eq!(updated.payload["items"][1]["status"], "in_progress");

    let plan = runtime
        .task_plan(&runtime_principal(), &task.id)
        .await
        .expect("plan visible")
        .expect("plan exists");
    assert_eq!(plan.items.len(), 2);
    assert_eq!(plan.items[0].content, "inspect");
    instance.shutdown().await;

    // The plan is durable state, not tool-instance state.
    let instance = Worker::new("parity", "0.1.0")
        .builtin_tools([BuiltinTool::WriteTodos])
        .start(&config)
        .await
        .expect("worker restarts");
    let recovered = instance
        .runtime()
        .task_plan(&runtime_principal(), &task.id)
        .await
        .expect("plan visible after restart")
        .expect("plan survived restart");
    assert_eq!(recovered.items, plan.items);
    instance.shutdown().await;
}

async fn wait_for_event(runtime: &runtime::Runtime, task_id: &str, kind: &str) -> StreamEvent {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(event) = events(runtime, task_id)
            .await
            .into_iter()
            .find(|event| event.kind == kind)
        {
            return event;
        }
        assert!(
            Instant::now() < deadline,
            "task never emitted '{kind}': {:?}",
            events(runtime, task_id).await
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_user_returns_the_answer_supplied_through_the_approval_channel() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &["ask_user"], default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .builtin_tools([BuiltinTool::AskUser])
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let task_id = submit(
        &runtime,
        r#"mcp:ask_user {"question":"Ship it?","options":["yes","no"],"ttl_ms":20000}"#,
        "ask",
    )
    .await;
    let asked = wait_for_event(&runtime, &task_id, "user.question").await;
    assert_eq!(asked.payload["question"], "Ship it?");
    assert_eq!(asked.payload["options"][0], "yes");
    let approval_id = asked.payload["approval_id"]
        .as_str()
        .expect("approval id")
        .to_string();
    let approval = runtime
        .approval(&runtime_principal(), &approval_id)
        .await
        .expect("approval visible");
    assert_eq!(approval.operation, "user.ask");
    runtime
        .decide_approval(
            &runtime_principal(),
            &approval_id,
            ApprovalDecision {
                decision: "approved".into(),
                digest: approval.digest.clone(),
                answer: Some("yes".into()),
            },
        )
        .await
        .expect("answer recorded");
    let task = wait(&runtime, &task_id).await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let result = result_json(&output(&task));
    assert_eq!(result["status"], "answered", "{result}");
    assert_eq!(result["answer"], "yes", "{result}");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_user_without_a_decision_reports_pending_not_an_answer() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &["ask_user"], default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .builtin_tools([BuiltinTool::AskUser])
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let task = run(
        &runtime,
        r#"mcp:ask_user {"question":"Anyone there?","ttl_ms":1000}"#,
        "ask-timeout",
    )
    .await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let result = result_json(&output(&task));
    assert_eq!(result["status"], "pending", "{result}");
    assert_eq!(result["error"], "user.pending", "{result}");
    assert!(result.get("answer").is_none(), "{result}");
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn image_tool_returns_a_typed_renderable_descriptor_and_rejects_non_images() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &["view_image"], default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .builtin_tools([BuiltinTool::ViewImage])
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let workspace = data.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    // A real 1x1 transparent PNG.
    const PNG: [u8; 67] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    std::fs::write(workspace.join("pixel.png"), PNG).expect("image written");
    let task = run(&runtime, r#"mcp:view_image {"path":"pixel.png"}"#, "image").await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let result = result_json(&output(&task));
    assert_eq!(result["kind"], "image", "{result}");
    assert_eq!(result["media_type"], "image/png", "{result}");
    assert_eq!(result["bytes"], 67, "{result}");
    assert!(result["digest"]
        .as_str()
        .is_some_and(|digest| !digest.is_empty()));
    assert!(result["data_url"]
        .as_str()
        .is_some_and(|url| url.starts_with("data:image/png;base64,")));

    std::fs::write(workspace.join("notes.txt"), "not an image").expect("text written");
    let task = run(
        &runtime,
        r#"mcp:view_image {"path":"notes.txt"}"#,
        "image-not-image",
    )
    .await;
    assert!(
        output(&task).contains("image.unsupported"),
        "{}",
        output(&task)
    );

    std::fs::write(data.path().join("host.png"), PNG).expect("host image");
    let task = run(
        &runtime,
        r#"mcp:view_image {"path":"../host.png"}"#,
        "image-escape",
    )
    .await;
    assert!(
        output(&task).contains("tool.path_denied"),
        "{}",
        output(&task)
    );
    instance.shutdown().await;
}

struct TestServer {
    base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_server() -> TestServer {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("test server");
    let addr = listener.local_addr().expect("test server address");
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut buffer = vec![0_u8; 8192];
            let read = stream.read(&mut buffer).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, content_type, location, body): (&str, &str, Option<String>, Vec<u8>) =
                match path.as_str() {
                    "/ok" => ("200 OK", "text/plain", None, b"hello from web".to_vec()),
                    "/large" => ("200 OK", "text/plain", None, vec![b'x'; 4096]),
                    "/binary" => (
                        "200 OK",
                        "application/octet-stream",
                        None,
                        vec![0_u8, 1, 2, 3],
                    ),
                    "/redirect" => (
                        "302 Found",
                        "text/plain",
                        Some("/ok".to_string()),
                        Vec::new(),
                    ),
                    "/redirect-denied" => (
                        "302 Found",
                        "text/plain",
                        Some("http://denied.invalid/".to_string()),
                        Vec::new(),
                    ),
                    path if path.starts_with("/search") => (
                        "200 OK",
                        "application/json",
                        None,
                        serde_json::to_vec(&serde_json::json!({
                            "results": [
                                {
                                    "title": "Allowed",
                                    "url": format!("http://127.0.0.1:{}/ok", addr.port()),
                                    "snippet": "allowed result"
                                },
                                {
                                    "title": "Denied",
                                    "url": "http://denied.invalid/",
                                    "snippet": "denied result"
                                }
                            ]
                        }))
                        .expect("search json"),
                    ),
                    _ => ("404 Not Found", "text/plain", None, b"missing".to_vec()),
                };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                body.len(),
                location
                    .map(|location| format!("Location: {location}\r\n"))
                    .unwrap_or_default()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
            let _ = stream.shutdown().await;
        }
    });
    TestServer {
        base: format!("http://127.0.0.1:{}", addr.port()),
        task,
    }
}

fn web_instance(
    data: &Path,
    tools: &[&str],
    policy: runtime::native::WebPolicy,
) -> impl std::future::Future<Output = runtime::worker::WorkerInstance> {
    let config = fixture(data, tools, default_tools_config());
    async move {
        Worker::new("parity", "0.1.0")
            .web_policy(policy)
            .builtin_tools(BuiltinTool::WEB)
            .start(&config)
            .await
            .expect("worker starts")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn web_fetch_enforces_destinations_private_network_and_size_bounds() {
    use runtime::native::WebPolicy;

    let server = start_server().await;
    let data = tempfile::tempdir().expect("data");
    let instance = web_instance(
        data.path(),
        &["web_fetch"],
        WebPolicy::new()
            .allow_domain("127.0.0.1")
            .allow_http(true)
            .allow_private_network(true)
            .max_bytes(32),
    )
    .await;
    let runtime = instance.runtime();

    let task = run(
        &runtime,
        &format!("mcp:web_fetch {{\"url\":\"{}/ok\"}}", server.base),
        "web-ok",
    )
    .await;
    let result = result_json(&output(&task));
    assert_eq!(result["status"], 200, "{result}");
    assert_eq!(result["body"], "hello from web", "{result}");
    assert_eq!(result["truncated"], false, "{result}");

    let task = run(
        &runtime,
        &format!("mcp:web_fetch {{\"url\":\"{}/large\"}}", server.base),
        "web-large",
    )
    .await;
    let result = result_json(&output(&task));
    assert_eq!(result["truncated"], true, "{result}");
    assert_eq!(result["bytes"], 32, "{result}");

    let task = run(
        &runtime,
        "mcp:web_fetch {\"url\":\"http://denied.invalid/\"}",
        "web-denied",
    )
    .await;
    assert!(
        output(&task).contains("web.destination_denied"),
        "{}",
        output(&task)
    );

    let task = run(
        &runtime,
        &format!("mcp:web_fetch {{\"url\":\"{}/binary\"}}", server.base),
        "web-binary",
    )
    .await;
    assert!(
        output(&task).contains("web.unsupported_content"),
        "{}",
        output(&task)
    );
    instance.shutdown().await;

    // The same allowed loopback destination is refused without an explicit
    // private-network allowance.
    let data = tempfile::tempdir().expect("data");
    let instance = web_instance(
        data.path(),
        &["web_fetch"],
        WebPolicy::new().allow_domain("127.0.0.1").allow_http(true),
    )
    .await;
    let runtime = instance.runtime();
    let task = run(
        &runtime,
        &format!("mcp:web_fetch {{\"url\":\"{}/ok\"}}", server.base),
        "web-private",
    )
    .await;
    assert!(
        output(&task).contains("web.private_network_denied"),
        "{}",
        output(&task)
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn web_redirects_are_checked_per_hop_and_need_explicit_allowance() {
    use runtime::native::WebPolicy;

    let server = start_server().await;
    let data = tempfile::tempdir().expect("data");
    let instance = web_instance(
        data.path(),
        &["web_fetch"],
        WebPolicy::new()
            .allow_domain("127.0.0.1")
            .allow_http(true)
            .allow_private_network(true)
            .allow_redirects(2),
    )
    .await;
    let runtime = instance.runtime();
    let task = run(
        &runtime,
        &format!("mcp:web_fetch {{\"url\":\"{}/redirect\"}}", server.base),
        "web-redirect",
    )
    .await;
    let result = result_json(&output(&task));
    assert_eq!(result["status"], 200, "{result}");
    assert!(result["final_url"]
        .as_str()
        .is_some_and(|url| url.ends_with("/ok")));

    let task = run(
        &runtime,
        &format!(
            "mcp:web_fetch {{\"url\":\"{}/redirect-denied\"}}",
            server.base
        ),
        "web-redirect-denied",
    )
    .await;
    assert!(
        output(&task).contains("web.redirect_denied"),
        "{}",
        output(&task)
    );
    instance.shutdown().await;

    let data = tempfile::tempdir().expect("data");
    let instance = web_instance(
        data.path(),
        &["web_fetch"],
        WebPolicy::new()
            .allow_domain("127.0.0.1")
            .allow_http(true)
            .allow_private_network(true),
    )
    .await;
    let runtime = instance.runtime();
    let task = run(
        &runtime,
        &format!("mcp:web_fetch {{\"url\":\"{}/redirect\"}}", server.base),
        "web-redirect-off",
    )
    .await;
    assert!(
        output(&task).contains("web.redirect_denied"),
        "{}",
        output(&task)
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn web_search_returns_typed_results_under_the_same_destination_policy() {
    use runtime::native::WebPolicy;

    let server = start_server().await;
    let data = tempfile::tempdir().expect("data");
    let instance = web_instance(
        data.path(),
        &["web_search"],
        WebPolicy::new()
            .allow_domain("127.0.0.1")
            .allow_http(true)
            .allow_private_network(true)
            .search_url(format!("{}/search?q={{query}}", server.base)),
    )
    .await;
    let runtime = instance.runtime();
    let task = run(
        &runtime,
        "mcp:web_search {\"query\":\"parity tools\",\"limit\":5}",
        "web-search",
    )
    .await;
    let result = result_json(&output(&task));
    assert_eq!(result["count"], 1, "{result}");
    assert_eq!(result["filtered"], 1, "{result}");
    assert_eq!(result["results"][0]["title"], "Allowed", "{result}");
    assert!(result["results"][0]["url"]
        .as_str()
        .is_some_and(|url| url.ends_with("/ok")));
    instance.shutdown().await;

    let data = tempfile::tempdir().expect("data");
    let instance = web_instance(
        data.path(),
        &["web_search"],
        WebPolicy::new().allow_domain("127.0.0.1").allow_http(true),
    )
    .await;
    let runtime = instance.runtime();
    let task = run(
        &runtime,
        "mcp:web_search {\"query\":\"anything\"}",
        "web-search-off",
    )
    .await;
    assert!(
        output(&task).contains("web.not_configured"),
        "{}",
        output(&task)
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn web_tools_cannot_start_without_an_explicit_policy() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &["web_fetch"], default_tools_config());
    let error = Worker::new("parity", "0.1.0")
        .builtin_tools([BuiltinTool::WebFetch])
        .start(&config)
        .await
        .err()
        .expect("a web tool without a policy must fail startup");
    assert_eq!(error.code, "config.web_policy");
}

fn steps_result(text: &str) -> Value {
    let start = text
        .find("steps result ")
        .map(|index| index + 13)
        .unwrap_or(0);
    serde_json::from_str(text[start..].trim()).unwrap_or(Value::Null)
}

fn process_policy(data: &Path) -> runtime::native::ProcessPolicy {
    let workspace = data.join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    runtime::native::ProcessPolicy::new(runtime::native::SandboxPolicy::new(&workspace))
        .max_lifetime(Duration::from_secs(30))
        .retention(Duration::from_secs(10))
}

const PROCESS_TOOLS: [&str; 5] = [
    "shell",
    "pty",
    "background_process",
    "process_wait",
    "process_kill",
];

#[tokio::test(flavor = "multi_thread")]
async fn process_tools_are_opt_in_advertised_and_cannot_start_without_a_policy() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &PROCESS_TOOLS, default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .process_tools(process_policy(data.path()))
        .start(&config)
        .await
        .expect("worker starts");
    let manifest = instance.manifest();
    for tool in PROCESS_TOOLS {
        let capability = manifest
            .capabilities
            .iter()
            .find(|capability| capability.id == tool)
            .unwrap_or_else(|| panic!("missing {tool} in inventory"));
        assert_eq!(capability.kind, "tool");
    }
    instance.shutdown().await;

    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &["shell"], default_tools_config());
    let error = Worker::new("parity", "0.1.0")
        .builtin_tools([BuiltinTool::Shell])
        .start(&config)
        .await
        .err()
        .expect("a process tool without a policy must fail startup");
    assert_eq!(error.code, "config.process_policy");
}

#[tokio::test(flavor = "multi_thread")]
async fn shell_runs_in_the_sandbox_and_streams_bounded_output_events() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &["shell"], default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .process_tools(process_policy(data.path()))
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let marker = format!("goldenbee-t32-{}", std::process::id());
    let task = run(
        &runtime,
        &format!(
            "mcp:shell {}",
            serde_json::json!({
                "command": format!(
                    "printf 'hello-stdout'; printf 'hello-stderr' >&2; touch /tmp/{marker} || true"
                )
            })
        ),
        "shell",
    )
    .await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let result = result_json(&output(&task));
    assert_eq!(result["state"], "exited", "{result}");
    assert_eq!(result["exit_code"], 0, "{result}");
    assert!(result["stdout"]
        .as_str()
        .is_some_and(|stdout| stdout.contains("hello-stdout")));
    assert!(result["stderr"]
        .as_str()
        .is_some_and(|stderr| stderr.contains("hello-stderr")));
    assert!(
        !Path::new(&format!("/tmp/{marker}")).exists(),
        "the contained command wrote to the host filesystem"
    );

    let events = events(&runtime, &task.id).await;
    assert!(events.iter().any(|event| {
        event.kind == "tool.output"
            && event.payload["tool"] == "shell"
            && event.payload["chunk"]
                .as_str()
                .is_some_and(|chunk| chunk.contains("hello-stdout"))
    }));
    assert!(events
        .iter()
        .any(|event| event.kind == "process.started" && event.payload["mode"] == "pipe"));
    assert!(events.iter().any(|event| event.kind == "process.exited"));
    let started = events
        .iter()
        .find(|event| event.kind == "tool.started" && event.payload["tool"] == "shell")
        .expect("shell tool started");
    assert_eq!(started.payload["sandbox"]["profile"], "workspace-write");
    let audit = runtime.audit_records(&task.id).await;
    assert!(audit
        .iter()
        .any(|record| record.action == "tool.call" && record.outcome == "ok"));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn background_process_wait_drains_output_and_kill_reports_the_observed_state() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &PROCESS_TOOLS, default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .process_tools(process_policy(data.path()))
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();

    let steps = serde_json::json!([
        {"tool":"background_process","args":{"command":"printf 'done-output'; exit 3","yield_time_ms":0}},
        {"tool":"process_wait","args":{"process_id":"$last.process_id","timeout_ms":5000}}
    ]);
    let task = run(&runtime, &format!("steps: {steps}"), "background-wait").await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let result = steps_result(&output(&task));
    assert_eq!(result["state"], "exited", "{result}");
    assert_eq!(result["exit_code"], 3, "{result}");
    assert!(result["stdout"]
        .as_str()
        .is_some_and(|stdout| stdout.contains("done-output")));
    assert_eq!(result["wait_timed_out"], false, "{result}");

    let steps = serde_json::json!([
        {"tool":"background_process","args":{"command":"printf 'started'; sleep 30","yield_time_ms":200}},
        {"tool":"process_wait","args":{"process_id":"$last.process_id","timeout_ms":100}},
        {"tool":"process_kill","args":{"process_id":"$last.process_id"}}
    ]);
    let task = run(&runtime, &format!("steps: {steps}"), "background-kill").await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let result = steps_result(&output(&task));
    assert!(
        result["kill_requested"].as_bool().unwrap_or(false),
        "{result}"
    );
    assert_eq!(result["reason"], "killed", "{result}");
    assert_ne!(result["state"], "running", "{result}");
    let events = events(&runtime, &task.id).await;
    assert!(events.iter().any(|event| {
        event.kind == "process.terminated" && event.payload["reason"] == "killed"
    }));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_task_terminates_its_owned_background_process() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &PROCESS_TOOLS, default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .process_tools(process_policy(data.path()))
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let steps = serde_json::json!([
        {"tool":"background_process","args":{"command":"sleep 1; printf 'late' > marker.txt","yield_time_ms":0}},
        {"tool":"process_wait","args":{"process_id":"$last.process_id","timeout_ms":60000}}
    ]);
    let task_id = submit(&runtime, &format!("steps: {steps}"), "background-cancel").await;
    wait_for_event(&runtime, &task_id, "process.started").await;
    runtime
        .cancel_task(&runtime_principal(), &task_id)
        .await
        .expect("cancel accepted");
    let task = wait(&runtime, &task_id).await;
    assert_eq!(task.state, TaskState::Cancelled, "{task:?}");
    let terminated = wait_for_event(&runtime, &task_id, "process.terminated").await;
    assert_eq!(terminated.payload["reason"], "cancelled");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !data.path().join("workspace/marker.txt").exists(),
        "a cancelled task left its background process running"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pty_session_streams_output_and_accepts_input() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &PROCESS_TOOLS, default_tools_config());
    let instance = Worker::new("parity", "0.1.0")
        .process_tools(process_policy(data.path()))
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let steps = serde_json::json!([
        {"tool":"pty","args":{"command":"sh -i","yield_time_ms":200}},
        {"tool":"pty","args":{"process_id":"$last.process_id","input":"printf pty-ok; exit\n","yield_time_ms":2000}}
    ]);
    let task = run(&runtime, &format!("steps: {steps}"), "pty").await;
    assert_eq!(task.state, TaskState::Succeeded, "{task:?}");
    let result = steps_result(&output(&task));
    assert_eq!(result["mode"], "pty", "{result}");
    assert!(
        result["stdout"]
            .as_str()
            .is_some_and(|stdout| stdout.contains("pty-ok")),
        "{result}"
    );
    let events = events(&runtime, &task.id).await;
    assert!(events
        .iter()
        .any(|event| event.kind == "tool.output" && event.payload["tool"] == "pty"));
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shell_timeout_and_large_output_are_typed_not_fabricated() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &["shell"], default_tools_config());
    let workspace = data.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let policy = runtime::native::ProcessPolicy::new(
        runtime::native::SandboxPolicy::new(&workspace).with_timeout(Duration::from_millis(300)),
    )
    .max_buffer_bytes(4096);
    let instance = Worker::new("parity", "0.1.0")
        .process_tools(policy)
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let started = Instant::now();
    let task = run(
        &runtime,
        r#"mcp:shell {"command":"sleep 10"}"#,
        "shell-timeout",
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the command deadline did not stop the shell"
    );
    let result = result_json(&output(&task));
    assert_eq!(result["error"], "process.timeout", "{result}");
    assert_ne!(result["state"], "running", "{result}");

    let task = run(
        &runtime,
        r#"mcp:shell {"command":"yes x | head -c 65536"}"#,
        "shell-bounded",
    )
    .await;
    let result = result_json(&output(&task));
    assert_eq!(result["truncated"], true, "{result}");
    assert!(
        result["dropped_bytes"].as_u64().unwrap_or(0) > 0,
        "{result}"
    );
    assert!(
        result["stdout"].as_str().unwrap_or_default().len() <= 4096,
        "{result}"
    );
    instance.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn process_limit_denies_a_second_running_process() {
    let data = tempfile::tempdir().expect("data");
    let config = fixture(data.path(), &PROCESS_TOOLS, default_tools_config());
    let workspace = data.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let policy =
        runtime::native::ProcessPolicy::new(runtime::native::SandboxPolicy::new(&workspace))
            .max_processes(1)
            .retention(Duration::from_secs(5));
    let instance = Worker::new("parity", "0.1.0")
        .process_tools(policy)
        .start(&config)
        .await
        .expect("worker starts");
    let runtime = instance.runtime();
    let steps = serde_json::json!([
        {"tool":"background_process","args":{"command":"sleep 30","yield_time_ms":50}},
        {"tool":"background_process","args":{"command":"printf second","yield_time_ms":50}},
        {"tool":"process_kill","args":{"process_id":"$last.process_id"}}
    ]);
    let task = run(&runtime, &format!("steps: {steps}"), "process-limit").await;
    let value = steps_result(&output(&task));
    // The third step receives the second step's error, so kill reports that no
    // process with the (absent) id exists: both outcomes are truthful denials,
    // never a fabricated start.
    assert!(
        value["error"] == "process.limit"
            || value["error"] == "process.not_found"
            || value["error"] == "process.denied",
        "{value}"
    );
    let events = events(&runtime, &task.id).await;
    assert!(events.iter().any(|event| {
        event.kind == "tool.completed" && event.payload["error_code"] == "process.limit"
    }));
    instance.shutdown().await;
}
