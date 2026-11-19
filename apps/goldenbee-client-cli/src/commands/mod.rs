pub(crate) mod approval;
pub(crate) mod artifact;
pub(crate) mod chat;
pub(crate) mod control;
pub(crate) mod diagnose;
pub(crate) mod fleet;
pub(crate) mod gateway;
pub(crate) mod health;
pub(crate) mod model;
pub(crate) mod profile;
pub(crate) mod provider;
pub(crate) mod task;
pub(crate) mod team;
pub(crate) mod worker;

use crate::cli::args::Command;
use crate::cli::output::usage;
use sdk::Client;
use std::process::ExitCode;

/// Run one SDK-backed command. Chat, Gateway profile, and management entry
/// points resolve their own local state and are dispatched by `main`.
pub(crate) async fn dispatch(command: Command, json: bool, client: &Client) -> ExitCode {
    match command {
        Command::Provider(command) => provider::provider_command(command, json, client).await,
        Command::Worker(command) => worker::worker_command(command, json, client).await,
        Command::Workers(query) => workers(client, query, json).await,
        Command::Model(command) => model::model_command(command, json, client).await,
        Command::Team(command) => team::team_command(command, json, client).await,
        Command::Profile(command) => profile::profile_command(command, json, client).await,
        Command::Task(command) => task::task_command(command, json, client).await,
        Command::Artifact(command) => artifact::artifact_command(command, json, client).await,
        Command::Fleet(command) => fleet::fleet_command(command, json, client).await,
        Command::Approval(command) => approval::approval_command(command, json, client).await,
        Command::Control(command) => control::control_command(command, json, client).await,
        Command::Diagnose(command) => diagnose::diagnose_command(command, json, client).await,
        Command::Health => health::health_command(json, client).await,
        Command::Chat(_) | Command::Gateway(_) | Command::Manage => {
            usage("gb-client: invalid dispatch")
        }
    }
}

async fn workers(client: &Client, query: worker::WorkersQuery, json_output: bool) -> ExitCode {
    let filter = common::WorkerDiscoveryFilter {
        capability: query.capability,
        protocol: query.protocol,
        locality: query.locality,
        health: query.health,
    };
    let filtered = filter.capability.is_some()
        || filter.protocol.is_some()
        || filter.locality.is_some()
        || filter.health.is_some();
    let result = if filtered {
        client.discover(&filter).await
    } else {
        client.workers().await
    };
    match result {
        Ok(workers) => {
            if json_output {
                return match crate::cli::output::json(&workers) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                };
            }
            for worker in workers {
                let capabilities = worker
                    .capabilities
                    .iter()
                    .map(|capability| capability.id.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                println!(
                    "{} {} reachable={} status={} locality={} instance={} package={} version={} incarnation={} capacity={}/{} capabilities={}",
                    worker.id,
                    worker.endpoint,
                    worker.reachable,
                    worker.status,
                    worker.locality,
                    worker.instance,
                    worker.package,
                    worker.version,
                    worker.incarnation,
                    worker.capacity.active,
                    worker.capacity.limit,
                    capabilities
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => crate::cli::output::api_error(error),
    }
}
