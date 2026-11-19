//! Human control lease acquisition, commands, handback, and audit.

use crate::cli::output::{api_error, json};
use sdk::Client;
use std::process::ExitCode;

/// Control commands.
#[derive(Debug, Clone)]
pub enum ControlCommand {
    /// Acquire a fenced lease.
    Acquire {
        /// Worker id.
        worker: String,
        /// Session id.
        session: String,
        /// Lease lifetime.
        ttl: Option<u64>,
    },
    /// Read the current lease.
    Status {
        /// Worker id.
        worker: String,
        /// Session id.
        session: String,
    },
    /// Send one control command.
    Command {
        /// Worker id.
        worker: String,
        /// Session id.
        session: String,
        /// Lease generation.
        generation: u64,
        /// Lease epoch.
        epoch: String,
        /// Command payload.
        command: common::ControlCommand,
    },
    /// Hand the lease back.
    HandBack {
        /// Worker id.
        worker: String,
        /// Session id.
        session: String,
        /// Lease generation.
        generation: u64,
        /// Lease epoch.
        epoch: String,
    },
    /// Read retained control audit.
    Audit {
        /// Worker id.
        worker: String,
        /// Session id.
        session: String,
    },
}

pub(crate) async fn control_command(
    command: ControlCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    match command {
        ControlCommand::Acquire {
            worker,
            session,
            ttl,
        } => {
            let request = common::ControlLeaseRequest {
                session_id: session,
                ttl_ms: ttl,
            };
            match client.acquire_control(&worker, &request).await {
                Ok(lease) => {
                    if json_output {
                        return match json(&lease) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    println!(
                        "control generation={} owner={:?} epoch={} expires_at={}",
                        lease.generation, lease.owner, lease.epoch, lease.expires_at_ms
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        ControlCommand::Status { worker, session } => {
            match client.control_status(&worker, &session).await {
                Ok(status) => {
                    if json_output {
                        return match json(&status) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    println!(
                        "control generation={} owner={:?} epoch={} expires_at={}",
                        status.generation, status.owner, status.epoch, status.expires_at_ms
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        ControlCommand::HandBack {
            worker,
            session,
            generation,
            epoch,
        } => {
            let request = common::ControlHandBackRequest {
                session_id: session,
                generation,
                epoch,
            };
            match client.hand_back_control(&worker, &request).await {
                Ok(lease) => {
                    if json_output {
                        return match json(&lease) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    println!(
                        "control generation={} owner={:?}",
                        lease.generation, lease.owner
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        ControlCommand::Audit { worker, session } => {
            match client.control_audit(&worker, &session).await {
                Ok(entries) => {
                    if json_output {
                        return match json(&entries) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    for entry in entries {
                        println!(
                            "{} {} {} {} {}",
                            entry.at, entry.action, entry.generation, entry.outcome, entry.detail
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        ControlCommand::Command {
            worker,
            session,
            generation,
            epoch,
            command,
        } => {
            let request = common::ControlCommandRequest {
                session_id: session,
                generation,
                epoch,
                command,
            };
            match client.control_command(&worker, &request).await {
                Ok(response) => {
                    if json_output {
                        return match json(&response) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    println!(
                        "control command={} generation={} url={}",
                        response.command, response.generation, response.page.url
                    );
                    if !response.output.is_empty() {
                        println!("{}", response.output);
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
    }
}
