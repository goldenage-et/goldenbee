//! Worker diagnostics.

use crate::cli::output::{api_error, json, state_name};
use sdk::Client;
use std::process::ExitCode;

/// Diagnose command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnoseCommand {
    /// Target Worker.
    pub worker: String,
    /// Optional task id.
    pub task: Option<String>,
}

pub(crate) async fn diagnose_command(
    command: DiagnoseCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    let task = command.task.as_deref().filter(|task| !task.is_empty());
    match client.diagnostics(&command.worker, task).await {
        Ok(bundle) => {
            if json_output {
                return match json(&bundle) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                };
            }
            println!(
                "worker={} instance={} package={} version={} config_revision={} asset_revision={} state={} ready={}",
                command.worker,
                bundle.health.instance,
                bundle.health.package,
                bundle.health.version,
                bundle.health.config_revision,
                bundle.health.asset_revision,
                bundle.health.phase,
                bundle.health.ready
            );
            if !bundle.health.degraded.is_empty() {
                println!("degraded={}", bundle.health.degraded.join(","));
            }
            if let Some(task) = &bundle.task {
                println!(
                    "task={} state={} attempt={} trace_id={} profile={}:{}",
                    task.task_id,
                    state_name(task.state),
                    task.attempt,
                    task.trace_id.as_deref().unwrap_or("-"),
                    task.profile,
                    task.profile_revision
                );
                if let Some(error) = &task.error {
                    println!("error {}: {}", error.code, error.message);
                }
            }
            for finding in &bundle.findings {
                println!(
                    "finding kind={} source={} code={} action={}",
                    finding.kind, finding.source, finding.code, finding.action
                );
                if !finding.message.is_empty() {
                    println!("  message: {}", finding.message);
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => api_error(error),
    }
}
