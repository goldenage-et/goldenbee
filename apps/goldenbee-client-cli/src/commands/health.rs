//! Gateway liveness.

use crate::cli::output::{api_error, json};
use sdk::Client;
use std::process::ExitCode;

pub(crate) async fn health_command(json_output: bool, client: &Client) -> ExitCode {
    match client.health().await {
        Ok(health) => {
            if json_output {
                return match json(&health) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                };
            }
            println!("status: {}", health.status);
            println!("workers: {}", health.workers);
            println!("reachable: {}", health.reachable);
            ExitCode::SUCCESS
        }
        Err(error) => api_error(error),
    }
}
