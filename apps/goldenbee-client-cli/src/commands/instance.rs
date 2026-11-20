//! Local instance deployment, inspection, and process lifecycle.

use crate::cli::output::{api_error, json};
use common::{
    HostLimits, HostProfileSelection, HostProvisionRequest, HostRollbackRequest,
    HostUpgradeRequest, HostWorkPolicy, PackageInstallRequest, PackageSource,
};
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

/// Deployment flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployOptions {
    /// Folder path or URL.
    pub source: String,
    /// Package name.
    pub package: Option<String>,
    /// Expected digest.
    pub digest: Option<String>,
    /// Instance identity.
    pub instance: String,
    /// Worker identity. Defaults to the instance.
    pub worker: Option<String>,
    /// Operation identity. Defaults to a digest-derived id.
    pub operation: Option<String>,
    /// Host configuration selection.
    pub config: Option<String>,
    /// Workspace selection.
    pub workspace: Option<String>,
    /// Read-only workspace.
    pub read_only: bool,
    /// Instance grants.
    pub grants: Vec<String>,
    /// Secret references.
    pub secret_refs: Vec<String>,
    /// Profile name.
    pub profile: Option<String>,
    /// Create the profile before activation.
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
    /// Worker control token.
    pub control_token: Option<String>,
}

/// Upgrade flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpgradeOptions {
    /// Operation identity.
    pub operation: String,
    /// Candidate digest.
    pub release: String,
    /// Added permissions.
    pub authorize: Vec<String>,
    /// Work policy.
    pub work_policy: WorkPolicy,
    /// Verification input.
    pub verify_input: Option<String>,
    /// Bind address.
    pub addr: Option<String>,
}

/// Local instance commands.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceCommand {
    /// Install, provision, and start one instance.
    Deploy(DeployOptions),
    /// List local instances.
    List,
    /// Inspect one instance.
    Inspect {
        /// Instance id.
        instance: String,
    },
    /// Start one instance.
    Start {
        /// Instance id.
        instance: String,
    },
    /// Stop one instance.
    Stop {
        /// Instance id.
        instance: String,
    },
    /// Restart one instance.
    Restart {
        /// Instance id.
        instance: String,
    },
    /// Read the bounded process log.
    Logs {
        /// Instance id.
        instance: String,
        /// Tail length.
        tail: usize,
    },
    /// Upgrade one instance.
    Upgrade {
        /// Instance id.
        instance: String,
        /// Upgrade options.
        options: UpgradeOptions,
    },
    /// Recover one interrupted activation.
    Rollback {
        /// Instance id.
        instance: String,
        /// Operation identity.
        operation: String,
    },
}

pub(crate) async fn instance_command(
    command: InstanceCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    match command {
        InstanceCommand::Deploy(options) => deploy(options, json_output, client).await,
        InstanceCommand::List => match client.instances().await {
            Ok(instances) => {
                if json_output {
                    return match json(&instances) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                for view in instances {
                    println!(
                        "{} digest={} lifecycle={} health={} endpoint={} profile={}",
                        view.instance,
                        view.artifact_digest,
                        view.lifecycle,
                        view.process_health,
                        view.endpoint.as_deref().unwrap_or("-"),
                        view.active_profile.as_deref().unwrap_or("-")
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        InstanceCommand::Inspect { instance } => match client.local_instance(&instance).await {
            Ok(inspection) => {
                if json_output {
                    return match json(&inspection) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                print_inspection(&inspection);
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        InstanceCommand::Start { instance } => {
            lifecycle(client, &instance, "start", json_output).await
        }
        InstanceCommand::Stop { instance } => {
            lifecycle(client, &instance, "stop", json_output).await
        }
        InstanceCommand::Restart { instance } => {
            lifecycle(client, &instance, "restart", json_output).await
        }
        InstanceCommand::Logs { instance, tail } => {
            match client.instance_logs(&instance, tail).await {
                Ok(logs) => {
                    if json_output {
                        return match json(&logs) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    for line in logs.lines {
                        println!("{line}");
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        InstanceCommand::Upgrade { instance, options } => {
            let request = HostUpgradeRequest {
                operation_id: options.operation,
                release_digest: options.release,
                authorized_permissions: options.authorize,
                work_policy: match options.work_policy {
                    WorkPolicy::Drain => HostWorkPolicy::Drain,
                    WorkPolicy::Pin => HostWorkPolicy::Pin,
                    WorkPolicy::Migrate => HostWorkPolicy::Migrate,
                },
                verify_input: options.verify_input,
                addr: options.addr,
            };
            match client.upgrade_local_instance(&instance, &request).await {
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
        InstanceCommand::Rollback {
            instance,
            operation,
        } => match client
            .rollback_local_instance(
                &instance,
                &HostRollbackRequest {
                    operation_id: operation,
                },
            )
            .await
        {
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
        },
    }
}

async fn deploy(options: DeployOptions, json_output: bool, client: &Client) -> ExitCode {
    let install = PackageInstallRequest {
        source: if options.source.starts_with("http://") || options.source.starts_with("https://") {
            PackageSource::Url {
                url: options.source.clone(),
            }
        } else {
            PackageSource::Folder {
                path: options.source.clone(),
            }
        },
        package: options.package.clone(),
        expected_digest: options.digest.clone(),
    };
    let installed = match client.install_package(&install).await {
        Ok(installed) => installed,
        Err(error) => return api_error(error),
    };
    let digest = installed.package.artifact_digest.clone();
    let instance = options.instance.clone();
    let operation = options.operation.clone().unwrap_or_else(|| {
        let short = digest.trim_start_matches("sha256:");
        format!("deploy-{}-{}", instance, &short[..short.len().min(16)])
    });
    let grants = if options.grants.is_empty() {
        vec![
            "task:submit".into(),
            "task:read".into(),
            "task:cancel".into(),
            "session:write".into(),
            "profile:read".into(),
            "profile:write".into(),
        ]
    } else {
        options.grants
    };
    let profile = options.profile.as_ref().map(|name| HostProfileSelection {
        name: name.clone(),
        create: options.profile_create,
        instruction: options.profile_instruction.clone(),
        members: options.profile_members.clone(),
        capabilities: options.profile_capabilities.clone(),
        grants: options.profile_grants.clone(),
    });
    let request = HostProvisionRequest {
        operation_id: operation,
        instance: instance.clone(),
        worker: options.worker.unwrap_or(instance),
        release_digest: digest,
        config: options.config,
        workspace: options.workspace,
        read_only: options.read_only,
        grants,
        limits: HostLimits::default(),
        secret_refs: options.secret_refs,
        profile,
        addr: options.addr,
        control_token: options.control_token,
    };
    match client.provision_local_instance(&request).await {
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

async fn lifecycle(client: &Client, instance: &str, action: &str, json_output: bool) -> ExitCode {
    let result = match action {
        "start" => client.start_instance(instance).await,
        "stop" => client.stop_instance(instance).await,
        "restart" => client.restart_instance(instance).await,
        _ => unreachable!(),
    };
    match result {
        Ok(inspection) => {
            if json_output {
                return match json(&inspection) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                };
            }
            print_inspection(&inspection);
            ExitCode::SUCCESS
        }
        Err(error) => api_error(error),
    }
}

pub(crate) fn print_inspection(inspection: &common::HostInspection) {
    println!(
        "{} {} digest={} lifecycle={} health={} endpoint={} profile={} worker={}",
        inspection.host,
        inspection.view.instance,
        inspection.view.artifact_digest,
        inspection.view.lifecycle,
        inspection.view.process_health,
        inspection.view.endpoint.as_deref().unwrap_or("-"),
        inspection.view.active_profile.as_deref().unwrap_or("-"),
        inspection
            .worker
            .as_ref()
            .map(|worker| worker.id.as_str())
            .unwrap_or("-")
    );
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
