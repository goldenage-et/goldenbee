//! Host fleet provisioning, inspection, upgrade, and rollback.

use crate::cli::output::{api_error, json};
use common::*;
use sdk::Client;
use std::process::ExitCode;

/// Work policy for an instance upgrade.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkPolicy {
    /// Drain work before replacing the instance.
    #[default]
    Drain,
    /// Pin work while replacing the instance.
    Pin,
    /// Migrate work during the replacement.
    Migrate,
}

/// Provisioning flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetProvisionOptions {
    /// External operation id.
    pub operation: String,
    /// Instance id.
    pub instance: String,
    /// Worker id.
    pub worker: String,
    /// Release digest.
    pub release: String,
    /// Host configuration path.
    pub config: Option<String>,
    /// Workspace path.
    pub workspace: Option<String>,
    /// Mount the workspace read-only.
    pub read_only: bool,
    /// Granted scopes.
    pub grants: Vec<String>,
    /// Secret references.
    pub secret_refs: Vec<String>,
    /// Optional profile selection name.
    pub profile_name: Option<String>,
    /// Create the profile when absent.
    pub profile_create: bool,
    /// Profile instruction.
    pub profile_instruction: Option<String>,
    /// Profile members.
    pub profile_members: Vec<String>,
    /// Profile capabilities.
    pub profile_capabilities: Vec<String>,
    /// Profile grants.
    pub profile_grants: Vec<String>,
    /// Bind address.
    pub addr: Option<String>,
    /// Control token.
    pub control_token: Option<String>,
}

/// Upgrade flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetUpgradeOptions {
    /// External operation id.
    pub operation: String,
    /// Release digest.
    pub release: String,
    /// Authorized permissions.
    pub authorized_permissions: Vec<String>,
    /// Work policy.
    pub work_policy: WorkPolicy,
    /// Optional verification prompt.
    pub verify_input: Option<String>,
    /// Bind address.
    pub addr: Option<String>,
}

/// Fleet commands.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetCommand {
    /// List hosts.
    Hosts,
    /// Show one host.
    Host {
        /// Host name.
        host: String,
    },
    /// Provision an instance.
    Provision {
        /// Host name.
        host: String,
        /// Provision options.
        options: FleetProvisionOptions,
    },
    /// Inspect one instance.
    Inspect {
        /// Host name.
        host: String,
        /// Instance id.
        instance: String,
    },
    /// Upgrade one instance.
    Upgrade {
        /// Host name.
        host: String,
        /// Instance id.
        instance: String,
        /// Upgrade options.
        options: FleetUpgradeOptions,
    },
    /// Roll back one instance.
    Rollback {
        /// Host name.
        host: String,
        /// Instance id.
        instance: String,
        /// External operation id.
        operation: String,
    },
}

pub(crate) async fn fleet_command(
    command: FleetCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    match command {
        FleetCommand::Hosts => match client.hosts().await {
            Ok(hosts) => {
                if json_output {
                    return match json(&hosts) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                for host in hosts {
                    println!(
                        "{} reachable={} protocol={} supported={}",
                        host.host,
                        host.reachable,
                        host.protocol,
                        host.supported.join(",")
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        FleetCommand::Host { host } => match client.host(&host).await {
            Ok(host) => {
                if json_output {
                    return match json(&host) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                println!(
                    "{} reachable={} protocol={} adapter={} supported={}",
                    host.host,
                    host.reachable,
                    host.protocol,
                    host.adapter_version,
                    host.supported.join(",")
                );
                if let Some(error) = host.error {
                    println!("error: {error}");
                }
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        FleetCommand::Provision { host, options } => {
            let profile = options
                .profile_name
                .as_ref()
                .map(|name| HostProfileSelection {
                    name: name.clone(),
                    create: options.profile_create,
                    instruction: options.profile_instruction.clone(),
                    members: options.profile_members.clone(),
                    capabilities: options.profile_capabilities.clone(),
                    grants: options.profile_grants.clone(),
                });
            let request = HostProvisionRequest {
                operation_id: options.operation,
                instance: options.instance,
                worker: options.worker,
                release_digest: options.release,
                config: options.config,
                workspace: options.workspace,
                read_only: options.read_only,
                grants: options.grants,
                limits: HostLimits::default(),
                secret_refs: options.secret_refs,
                profile,
                addr: options.addr,
                control_token: options.control_token,
            };
            match client.provision_instance(&host, &request).await {
                Ok(outcome) => {
                    if json_output {
                        return match json(&outcome) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    print_outcome(&outcome);
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        FleetCommand::Inspect { host, instance } => {
            match client.inspect_instance(&host, &instance).await {
                Ok(inspection) => {
                    if json_output {
                        return match json(&inspection) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    println!(
                        "{} {} digest={} lifecycle={} health={} compatible={} profile={} worker={}",
                        inspection.host,
                        inspection.view.instance,
                        inspection.view.artifact_digest,
                        inspection.view.lifecycle,
                        inspection.view.process_health,
                        inspection.view.compatibility.compatible,
                        inspection.view.active_profile.as_deref().unwrap_or("-"),
                        inspection
                            .worker
                            .as_ref()
                            .map(|worker| worker.id.as_str())
                            .unwrap_or("-")
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        FleetCommand::Upgrade {
            host,
            instance,
            options,
        } => {
            let work_policy = match options.work_policy {
                WorkPolicy::Drain => HostWorkPolicy::Drain,
                WorkPolicy::Pin => HostWorkPolicy::Pin,
                WorkPolicy::Migrate => HostWorkPolicy::Migrate,
            };
            let request = HostUpgradeRequest {
                operation_id: options.operation,
                release_digest: options.release,
                authorized_permissions: options.authorized_permissions,
                work_policy,
                verify_input: options.verify_input,
                addr: options.addr,
            };
            match client.upgrade_instance(&host, &instance, &request).await {
                Ok(outcome) => {
                    if json_output {
                        return match json(&outcome) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    print_outcome(&outcome);
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        FleetCommand::Rollback {
            host,
            instance,
            operation,
        } => {
            let request = HostRollbackRequest {
                operation_id: operation,
            };
            match client.rollback_instance(&host, &instance, &request).await {
                Ok(outcome) => {
                    if json_output {
                        return match json(&outcome) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    print_outcome(&outcome);
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
    }
}

pub(crate) fn print_outcome(outcome: &common::HostOperationOutcome) {
    println!(
        "{} {} {} status={} replay={}",
        outcome.operation_id, outcome.kind, outcome.instance, outcome.status, outcome.replayed
    );
    if let Some(view) = &outcome.view {
        println!(
            "digest={} lifecycle={} health={} compatible={} profile={}",
            view.artifact_digest,
            view.lifecycle,
            view.process_health,
            view.compatibility.compatible,
            view.active_profile.as_deref().unwrap_or("-")
        );
    }
    if let Some(worker) = &outcome.worker {
        println!(
            "worker={} status={} reachable={}",
            worker.id, worker.status, worker.reachable
        );
    }
}
