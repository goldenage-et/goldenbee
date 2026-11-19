//! Approved Worker administration and caller-scoped discovery.

use crate::cli::output::{api_error, default_key, json, usage};
use crate::cli::secret::read_stdin_secret;
use common::*;
use sdk::Client;
use std::process::ExitCode;

/// Filters owned by the caller-scoped `workers` command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkersQuery {
    /// Required capability.
    pub capability: Option<String>,
    /// Required protocol version.
    pub protocol: Option<String>,
    /// Required locality.
    pub locality: Option<String>,
    /// Required health.
    pub health: Option<String>,
}

/// Options for adding an approved Worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddWorkerOptions {
    /// Worker id.
    pub id: String,
    /// Worker endpoint.
    pub endpoint: String,
    /// Expected instance identity.
    pub instance: Option<String>,
    /// Expected package name.
    pub package: Option<String>,
    /// Approved capability subset.
    pub capabilities: Option<Vec<String>>,
    /// Read a control token from stdin.
    pub control_token_stdin: bool,
    /// Verify the endpoint before returning.
    pub preflight: bool,
    /// Idempotency key.
    pub key: Option<String>,
}

/// Options for updating an approved Worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateWorkerOptions {
    /// Observed revision.
    pub revision: u64,
    /// New endpoint.
    pub endpoint: Option<String>,
    /// New expected instance.
    pub instance: Option<String>,
    /// New expected package.
    pub package: Option<String>,
    /// New approved capabilities.
    pub capabilities: Option<Vec<String>>,
    /// Idempotency key.
    pub key: Option<String>,
}

/// Worker administration commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerCommand {
    /// Full approved inventory.
    List,
    /// Inspect one approved Worker.
    Show {
        /// Worker id.
        id: String,
    },
    /// Add one approved Worker.
    Add(AddWorkerOptions),
    /// Update one approval.
    Update {
        /// Worker id.
        id: String,
        /// Update options.
        options: UpdateWorkerOptions,
    },
    /// Disable registration and routing.
    Disable {
        /// Worker id.
        id: String,
    },
    /// Enable registration again.
    Enable {
        /// Worker id.
        id: String,
    },
    /// Probe the live endpoint.
    Probe {
        /// Worker id.
        id: String,
    },
    /// Decommission the identity.
    Remove {
        /// Worker id.
        id: String,
    },
    /// Retained admin audit.
    Audit {
        /// Optional Worker filter.
        worker: Option<String>,
    },
}

pub(crate) fn admin_table(workers: &[AdminWorkerSummary]) {
    if workers.is_empty() {
        println!("no approved workers");
        return;
    }
    println!(
        "{:<24} {:<14} {:<8} {:<6} {:<8} {:<32} CAPABILITIES",
        "ID", "STATUS", "REACH", "REV", "SOURCE", "ENDPOINT"
    );
    for worker in workers {
        let capabilities = worker
            .capabilities
            .as_ref()
            .map_or_else(|| "-".to_string(), |capabilities| capabilities.join(","));
        println!(
            "{:<24} {:<14} {:<8} {:<6} {:<8} {:<32} {}",
            worker.id,
            worker.status,
            worker.reachable,
            worker.revision,
            worker.source,
            worker.endpoint,
            capabilities
        );
    }
}

pub(crate) fn read_control_token() -> Result<String, String> {
    read_stdin_secret("control token")
}

pub(crate) async fn worker_command(
    command: WorkerCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    match command {
        WorkerCommand::List => match client.admin_workers().await {
            Ok(workers) => {
                if json_output {
                    return match json(&workers) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                admin_table(&workers);
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        WorkerCommand::Show { id } => match client.admin_worker(&id).await {
            Ok(worker) => {
                if json_output {
                    return match json(&worker) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                println!("id: {}", worker.id);
                println!("endpoint: {}", worker.endpoint);
                println!("status: {}", worker.status);
                println!("reachable: {}", worker.reachable);
                println!("enabled: {}", worker.enabled);
                println!("revision: {}", worker.revision);
                println!("source: {}", worker.source);
                println!("instance: {}", worker.instance);
                println!("package: {}", worker.package);
                println!("control-token-set: {}", worker.control_token_set);
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        WorkerCommand::Add(options) => {
            let control_token = if options.control_token_stdin {
                match read_control_token() {
                    Ok(token) => Some(token),
                    Err(message) => return usage(&message),
                }
            } else {
                None
            };
            let request = AddWorkerRequest {
                id: options.id,
                endpoint: options.endpoint,
                instance: options.instance,
                package: options.package,
                capabilities: options.capabilities,
                control_token,
                preflight: options.preflight,
                idempotency_key: options.key.unwrap_or_else(default_key),
            };
            match client.add_worker(&request).await {
                Ok(response) => {
                    if json_output {
                        if let Err(code) = json(&response) {
                            return code;
                        }
                    } else {
                        admin_table(std::slice::from_ref(&response.worker));
                        if let Some(token) = &response.control_token {
                            println!(
                                "warning: control token shown once; configure it on the Worker now and store it securely:"
                            );
                            println!("{token}");
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        WorkerCommand::Update { id, options } => {
            let request = UpdateWorkerRequest {
                revision: options.revision,
                endpoint: options.endpoint,
                instance: options.instance,
                package: options.package,
                capabilities: options.capabilities,
                idempotency_key: Some(options.key.unwrap_or_else(default_key)),
            };
            match client.update_worker(&id, &request).await {
                Ok(worker) => {
                    if json_output {
                        return match json(&worker) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    admin_table(std::slice::from_ref(&worker));
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        WorkerCommand::Disable { id } => lifecycle(client, &id, "disable", json_output).await,
        WorkerCommand::Enable { id } => lifecycle(client, &id, "enable", json_output).await,
        WorkerCommand::Probe { id } => lifecycle(client, &id, "probe", json_output).await,
        WorkerCommand::Remove { id } => lifecycle(client, &id, "remove", json_output).await,
        WorkerCommand::Audit { worker } => match client.worker_audit(worker.as_deref()).await {
            Ok(entries) => {
                if json_output {
                    return match json(&entries) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                for entry in entries {
                    println!(
                        "{} {} {} {} {} {}",
                        entry.at,
                        entry.action,
                        entry.worker,
                        entry.principal,
                        entry.outcome,
                        entry.detail
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
    }
}

async fn lifecycle(client: &Client, id: &str, action: &str, json_output: bool) -> ExitCode {
    let result = match action {
        "disable" => client.disable_worker(id).await,
        "enable" => client.enable_worker(id).await,
        "probe" => client.probe_worker(id).await,
        _ => client.decommission_worker(id).await,
    };
    match result {
        Ok(worker) => {
            if json_output {
                return match json(&worker) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                };
            }
            admin_table(std::slice::from_ref(&worker));
            ExitCode::SUCCESS
        }
        Err(error) => api_error(error),
    }
}
