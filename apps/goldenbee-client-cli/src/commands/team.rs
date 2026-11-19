//! Team roster administration.

use crate::cli::output::{api_error, json, usage};
use common::*;
use sdk::Client;
use std::process::ExitCode;

/// Parsed roster inputs shared by create and revise.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamOptions {
    /// Master Worker.
    pub master: Option<String>,
    /// Raw member specifications.
    pub members: Vec<String>,
    /// Maximum delegation depth.
    pub max_depth: Option<u32>,
    /// Maximum fan-out.
    pub max_fanout: Option<u32>,
    /// Required locality.
    pub locality: Option<String>,
}

/// Team administration commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamCommand {
    /// List teams.
    List,
    /// Show one team.
    Show {
        /// Team name.
        name: String,
    },
    /// Create an inactive version 1 roster.
    Create {
        /// Team name.
        name: String,
        /// Roster inputs.
        options: TeamOptions,
    },
    /// Revise an observed roster version.
    Revise {
        /// Team name.
        name: String,
        /// Observed version.
        version: u64,
        /// Roster inputs.
        options: TeamOptions,
    },
    /// Validate one version.
    Validate {
        /// Team name.
        name: String,
        /// Optional version.
        version: Option<u64>,
    },
    /// Activate one version.
    Activate {
        /// Team name.
        name: String,
        /// Optional version.
        version: Option<u64>,
    },
    /// Deactivate one version.
    Deactivate {
        /// Team name.
        name: String,
        /// Optional version.
        version: Option<u64>,
    },
}

pub(crate) fn parse_member(raw: &str, master: &str) -> Result<TeamMember, String> {
    let (worker, capabilities) = raw
        .split_once(':')
        .map_or((raw, ""), |(worker, capabilities)| (worker, capabilities));
    let worker = worker.trim();
    if worker.is_empty() {
        return Err("gb-client: --member requires <worker[:capability[,capability]]>".into());
    }
    let capabilities: Vec<String> = capabilities
        .split(',')
        .map(str::trim)
        .filter(|capability| !capability.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if capabilities.is_empty() {
        return Err(
            "gb-client: --member requires at least one capability, for example worker-a:general"
                .into(),
        );
    }
    Ok(TeamMember {
        worker: worker.to_string(),
        role: if worker == master {
            "master".into()
        } else {
            "specialist".into()
        },
        capabilities,
        max_depth: 0,
        instance: None,
    })
}

pub(crate) fn team_spec(options: &TeamOptions) -> Result<TeamSpec, String> {
    let Some(master) = options.master.clone() else {
        return Err("gb-client: team create/revise requires --master <worker>".into());
    };
    if options.members.is_empty() {
        return Err("gb-client: team create/revise requires at least one --member".into());
    }
    let members = options
        .members
        .iter()
        .map(|member| parse_member(member, &master))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TeamSpec {
        master,
        members,
        limits: Limits {
            max_depth: options.max_depth.unwrap_or(0),
            max_fanout: options.max_fanout.unwrap_or(0),
            ..Limits::default()
        },
        locality: options.locality.clone(),
    })
}

pub(crate) fn team_line(team: &Team, worker: Option<&str>) -> String {
    let members = team
        .members
        .iter()
        .map(|member| format!("{}:{}", member.worker, member.capabilities.join(",")))
        .collect::<Vec<_>>()
        .join(",");
    let worker = worker.map_or_else(String::new, |worker| format!(" worker={worker}"));
    format!(
        "team={} version={} master={} active={} locality={}{} members={}",
        team.name,
        team.version,
        team.master,
        team.active,
        team.locality.as_deref().unwrap_or("-"),
        worker,
        members
    )
}

pub(crate) fn team_output(
    teams: &[Team],
    worker: Option<&str>,
    json_output: bool,
) -> Result<(), ExitCode> {
    if json_output {
        return json(teams);
    }
    if teams.is_empty() {
        println!("no teams");
        return Ok(());
    }
    for team in teams {
        println!("{}", team_line(team, worker));
    }
    Ok(())
}

pub(crate) fn one_team(team: &Team, json_output: bool) -> Result<(), ExitCode> {
    if json_output {
        return json(team);
    }
    println!("{}", team_line(team, None));
    Ok(())
}

pub(crate) async fn team_command(
    command: TeamCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    match command {
        TeamCommand::List => match client.teams().await {
            Ok(teams) => match team_output(&teams, None, json_output) {
                Ok(()) => ExitCode::SUCCESS,
                Err(code) => code,
            },
            Err(error) => api_error(error),
        },
        TeamCommand::Show { name } => match client.team(&name).await {
            Ok(team) => match one_team(&team, json_output) {
                Ok(()) => ExitCode::SUCCESS,
                Err(code) => code,
            },
            Err(error) => api_error(error),
        },
        TeamCommand::Create { name, options } => {
            let spec = match team_spec(&options) {
                Ok(spec) => spec,
                Err(message) => return usage(&message),
            };
            let request = CreateTeamRequest { name, team: spec };
            match client.create_team(&request).await {
                Ok(team) => match one_team(&team, json_output) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                },
                Err(error) => api_error(error),
            }
        }
        TeamCommand::Revise {
            name,
            version,
            options,
        } => {
            let spec = match team_spec(&options) {
                Ok(spec) => spec,
                Err(message) => return usage(&message),
            };
            let request = UpdateTeamRequest {
                expected_version: version,
                team: spec,
            };
            match client.update_team(&name, &request).await {
                Ok(team) => match one_team(&team, json_output) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                },
                Err(error) => api_error(error),
            }
        }
        TeamCommand::Validate { name, version } => {
            let request = TeamVersionRequest { version };
            match client.validate_team(&name, &request).await {
                Ok(team) => match one_team(&team, json_output) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                },
                Err(error) => api_error(error),
            }
        }
        TeamCommand::Activate { name, version } => {
            let request = TeamVersionRequest { version };
            match client.activate_team(&name, &request).await {
                Ok(team) => match one_team(&team, json_output) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                },
                Err(error) => api_error(error),
            }
        }
        TeamCommand::Deactivate { name, version } => {
            let request = TeamVersionRequest { version };
            match client.deactivate_team(&name, &request).await {
                Ok(team) => match one_team(&team, json_output) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                },
                Err(error) => api_error(error),
            }
        }
    }
}
