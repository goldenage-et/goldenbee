//! Separately compiled use-case application over the reusable Gateway SDK.
//!
//! It demonstrates the public Client journey without linking Worker runtime,
//! native ADK execution, or any UI framework: submit, inspect the stable
//! handle, resume the durable SSE stream, fetch and verify an artifact, and
//! optionally cancel a different authorized task.

use common::{
    GatewaySubmitRequest, Limits, SocketCommand, SocketCommandFrame, SocketOutcome, SocketScope,
};
use sdk::{cancellation_outcome, CancellationOutcome, Client, Socket};
use serde_json::{json, Value};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const HELP: &str = "Usage: sdk-client --gateway <url> --token <token> --worker <id> [options]

Options:
  --agent <name>        Agent capability (default: general)
  --input <text>        Task input (default: artifact:sdk-client evidence)
  --session <id>        Session identity (default: sdk-client-session)
  --key <key>           Caller idempotency key (default: sdk-client-journey)
  --cancel-task <id>    Cancel another authorized task after the journey
  --socket              Submit and watch through the SDK duplex adapter
  --timeout-ms <n>      Terminal deadline (default: 30000)
  --help, -h            Show this help
  --version, -V         Show the package version";

struct Options {
    gateway: String,
    token: String,
    worker: String,
    agent: String,
    input: String,
    session: String,
    key: String,
    cancel_task: Option<String>,
    socket: bool,
    timeout: Duration,
}

#[tokio::main]
async fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .iter()
        .any(|value| value == "--help" || value == "-h")
    {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if arguments
        .iter()
        .any(|value| value == "--version" || value == "-V")
    {
        println!("sdk-client {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let options = match parse(&arguments) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("sdk-client: {message}");
            return ExitCode::from(2);
        }
    };
    match run(options).await {
        Ok(summary) => {
            match serde_json::to_string_pretty(&summary) {
                Ok(rendered) => println!("{rendered}"),
                Err(_) => {
                    eprintln!("sdk-client: summary could not be rendered");
                    return ExitCode::from(1);
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("sdk-client: {}: {}", error.code, error.message);
            if !error.recovery.is_empty() {
                eprintln!("recovery: {}", error.recovery);
            }
            ExitCode::from(1)
        }
    }
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let mut options = Options {
        gateway: String::new(),
        token: String::new(),
        worker: String::new(),
        agent: "general".into(),
        input: "artifact:sdk-client evidence".into(),
        session: "sdk-client-session".into(),
        key: "sdk-client-journey".into(),
        cancel_task: None,
        socket: false,
        timeout: Duration::from_secs(30),
    };
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        let mut value = || {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{argument} requires a value"))
        };
        match argument.as_str() {
            "--gateway" => options.gateway = value()?,
            "--token" => options.token = value()?,
            "--worker" => options.worker = value()?,
            "--agent" => options.agent = value()?,
            "--input" => options.input = value()?,
            "--session" => options.session = value()?,
            "--key" => options.key = value()?,
            "--cancel-task" => options.cancel_task = Some(value()?),
            "--socket" => options.socket = true,
            "--timeout-ms" => {
                let millis = value()?
                    .parse::<u64>()
                    .map_err(|_| "--timeout-ms requires an integer".to_string())?;
                options.timeout = Duration::from_millis(millis.clamp(100, 300_000));
            }
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    if options.gateway.is_empty() || options.token.is_empty() || options.worker.is_empty() {
        return Err("--gateway, --token, and --worker are required".into());
    }
    Ok(options)
}

async fn run(options: Options) -> Result<Value, common::ApiError> {
    let transport = if options.socket { "socket" } else { "http" };
    let client = Client::new(options.gateway.clone(), options.token.clone());
    let request = GatewaySubmitRequest {
        worker: Some(options.worker),
        agent: options.agent,
        session_id: options.session,
        input: options.input,
        idempotency_key: options.key,
        profile: None,
        team: None,
        team_version: None,
        limits: Limits::default(),
    };

    let (submitted, resumed_from, streamed, mut socket_session) = if options.socket {
        let socket = Socket::new(options.gateway.clone(), options.token.clone());
        let mut session = socket.connect().await?;
        let submit = SocketCommandFrame::new(
            "sdk-client-submit",
            SocketScope {
                worker: request.worker.clone(),
                task_id: None,
            },
            SocketCommand::Submit {
                request: request.clone(),
            },
        );
        let response = match session.command(&submit).await? {
            SocketOutcome::Submitted { response } => *response,
            other => {
                return Err(common::ApiError::new(
                    "sdk-client.protocol",
                    format!("unexpected socket submit outcome: {other:?}"),
                ))
            }
        };
        let subscribe = SocketCommandFrame::new(
            "sdk-client-watch",
            SocketScope {
                worker: None,
                task_id: Some(response.task_id.clone()),
            },
            SocketCommand::Subscribe {
                task_id: response.task_id.clone(),
                after: 0,
            },
        );
        session.command(&subscribe).await?;
        let mut resumed_from = 0;
        let mut streamed = 0usize;
        loop {
            let event = session.next_event().await?;
            streamed += 1;
            if event.durable {
                resumed_from = resumed_from.max(event.seq);
            }
            if matches!(
                event.kind.as_str(),
                "task.succeeded" | "task.failed" | "task.cancelled" | "task.unknown"
            ) {
                break;
            }
        }
        (response, resumed_from, streamed, Some(session))
    } else {
        let submitted = client.submit(&request).await?;
        // Resume the durable stream explicitly: read inside one connection,
        // keep the durable cursor, drop it, then reopen from that cursor. This
        // never resubmits the task.
        let mut resumed_from = 0;
        let mut streamed = 0usize;
        if let Ok(mut watch) = client.watch(&submitted.task_id, 0).await {
            if let Ok(Some(_first)) = watch.next().await {
                resumed_from = watch.last_durable_seq();
            }
            drop(watch);
        }
        if let Ok(mut watch) = client.watch_from(&submitted.task_id, resumed_from).await {
            while let Ok(Some(_event)) = watch.next().await {
                streamed += 1;
            }
        }
        (submitted, resumed_from, streamed, None)
    };

    let task = wait_terminal(&client, &submitted.task_id, options.timeout).await?;

    let mut artifact = Value::Null;
    let artifacts = client.artifacts(&submitted.task_id).await?;
    if let Some(first) = artifacts.first() {
        let download = client
            .download_artifact(&submitted.task_id, &first.id)
            .await?;
        artifact = json!({
            "id": download.artifact.id,
            "name": download.artifact.name,
            "content_type": download.artifact.content_type,
            "size": download.artifact.size,
            "digest": download.artifact.digest,
            "verified": true,
            "text": String::from_utf8_lossy(&download.bytes),
        });
    }

    let mut cancellation = Value::Null;
    if let Some(task_id) = options.cancel_task {
        let cancelled = if let Some(session) = socket_session.as_mut() {
            let command = SocketCommandFrame::new(
                "sdk-client-cancel",
                SocketScope {
                    worker: None,
                    task_id: Some(task_id.clone()),
                },
                SocketCommand::Cancel {
                    task_id: task_id.clone(),
                },
            );
            match session.command(&command).await? {
                SocketOutcome::Cancelled { task } => *task,
                other => {
                    return Err(common::ApiError::new(
                        "sdk-client.protocol",
                        format!("unexpected socket cancel outcome: {other:?}"),
                    ))
                }
            }
        } else {
            client.cancel(&task_id).await?
        };
        let outcome = match cancellation_outcome(&cancelled) {
            CancellationOutcome::Confirmed => "confirmed",
            CancellationOutcome::Pending => "pending",
            CancellationOutcome::AlreadyTerminal => "already_terminal",
            CancellationOutcome::Uncertain => "uncertain",
        };
        cancellation = json!({
            "task_id": task_id,
            "outcome": outcome,
            "state": serde_json::to_value(cancelled.task.state)
                .unwrap_or_else(|_| json!("unknown")),
        });
    }
    drop(socket_session);

    Ok(json!({
        "transport": transport,
        "task_id": submitted.task_id,
        "worker": submitted.worker,
        "worker_instance": submitted.worker_instance,
        "state": serde_json::to_value(task.state).unwrap_or_else(|_| json!("unknown")),
        "replayed": submitted.replayed,
        "resumed_from": resumed_from,
        "streamed_events": streamed,
        "artifact": artifact,
        "cancellation": cancellation,
    }))
}

async fn wait_terminal(
    client: &Client,
    task_id: &str,
    timeout: Duration,
) -> Result<common::Task, common::ApiError> {
    let started = Instant::now();
    loop {
        let task = client.task(task_id).await?;
        if task.task.state.is_terminal() {
            return Ok(task.task);
        }
        if started.elapsed() >= timeout {
            return Err(common::ApiError::retryable(
                "sdk-client.timeout",
                format!("task {task_id} did not reach a terminal state in time"),
            )
            .with("task_id", task_id.to_string()));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
