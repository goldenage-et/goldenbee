//! Task submission, inspection, events, cancellation, and watch.

use crate::cli::output::{api_error, default_key, json, state_name, usage};
use common::{GatewaySubmitRequest, Limits};
use sdk::Client;
use std::process::ExitCode;

/// Flags owned by `submit`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubmitOptions {
    /// Target Worker.
    pub worker: Option<String>,
    /// Target agent.
    pub agent: Option<String>,
    /// Prompt text.
    pub input: Option<String>,
    /// Conversation session id.
    pub session: Option<String>,
    /// Idempotency key.
    pub key: Option<String>,
    /// Runtime profile.
    pub profile: Option<String>,
    /// Team roster.
    pub team: Option<String>,
    /// Observed team version.
    pub team_version: Option<u64>,
}

/// Task commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskCommand {
    /// Submit one prompt.
    Submit(SubmitOptions),
    /// Show one task projection.
    Show {
        /// Task id.
        id: String,
    },
    /// Cancel one task.
    Cancel {
        /// Task id.
        id: String,
    },
    /// Read retained events.
    Events {
        /// Task id.
        id: String,
        /// First sequence.
        after: u64,
        /// Maximum events.
        limit: usize,
    },
    /// Follow retained and live events.
    Watch {
        /// Task id.
        id: String,
        /// First sequence.
        after: u64,
    },
}

pub(crate) async fn task_command(
    command: TaskCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    match command {
        TaskCommand::Submit(options) => submit(options, json_output, client).await,
        TaskCommand::Show { id } => match client.task(&id).await {
            Ok(projection) => {
                if json_output {
                    return match json(&projection) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                println!(
                    "task {} on {} state={} trace_id={}",
                    projection.task_id,
                    projection.worker,
                    state_name(projection.task.state),
                    projection.task.trace_id
                );
                if let Some(output) = &projection.task.output {
                    println!("{output}");
                }
                if let Some(error) = &projection.task.error {
                    println!("error {}: {}", error.code, error.message);
                }
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        TaskCommand::Cancel { id } => match client.cancel(&id).await {
            Ok(projection) => {
                if json_output {
                    return match json(&projection) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                println!(
                    "task {} on {} state={}",
                    projection.task_id,
                    projection.worker,
                    state_name(projection.task.state)
                );
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        TaskCommand::Events { id, after, limit } => match client.events(&id, after, limit).await {
            Ok(page) => {
                if json_output {
                    return match json(&page) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                for event in page.events {
                    println!("{} {} {}", event.seq, event.kind, event.payload);
                }
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        TaskCommand::Watch { id, after } => watch(&id, after, json_output, client).await,
    }
}

async fn submit(options: SubmitOptions, json_output: bool, client: &Client) -> ExitCode {
    let (Some(worker), Some(agent), Some(input)) = (options.worker, options.agent, options.input)
    else {
        return usage("gb-client: submit requires --worker, --agent, and --input");
    };
    let request = GatewaySubmitRequest {
        worker: Some(worker),
        agent,
        session_id: options
            .session
            .unwrap_or_else(|| "gb-client-session".to_string()),
        input,
        idempotency_key: options.key.unwrap_or_else(default_key),
        profile: options.profile,
        team: options.team,
        team_version: options.team_version,
        limits: Limits::default(),
    };
    match client.submit(&request).await {
        Ok(response) => {
            if json_output {
                return match json(&response) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                };
            }
            println!(
                "task {} on {} state={} replayed={}",
                response.task_id,
                response.worker,
                state_name(response.state),
                response.replayed
            );
            ExitCode::SUCCESS
        }
        Err(error) => api_error(error),
    }
}

async fn watch(id: &str, after: u64, json_output: bool, client: &Client) -> ExitCode {
    let mut watch = match client.watch_from(id, after).await {
        Ok(watch) => watch,
        Err(error) => return api_error(error),
    };
    loop {
        match watch.next().await {
            Ok(Some(event)) => {
                if json_output {
                    if let Err(code) = json(&event) {
                        return code;
                    }
                } else {
                    println!("{} {} {}", event.seq, event.kind, event.payload);
                }
                if matches!(
                    event.kind.as_str(),
                    "task.succeeded" | "task.failed" | "task.cancelled" | "task.unknown"
                ) {
                    break;
                }
            }
            Ok(None) => break,
            Err(error) => return api_error(error),
        }
    }
    ExitCode::SUCCESS
}
