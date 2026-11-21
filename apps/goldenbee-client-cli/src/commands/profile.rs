//! Runtime profile administration on one owning Worker.

use crate::cli::output::{api_error, json, usage};
use common::*;
use sdk::Client;
use std::process::ExitCode;

/// Revision inputs shared by create and revise.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileRevisionOptions {
    /// Required instruction text.
    pub instruction: String,
    /// Concrete model id.
    pub model: Option<String>,
    /// Enabled capabilities.
    pub capabilities: Vec<String>,
    /// Granted scopes.
    pub grants: Vec<String>,
    /// Restricted scopes.
    pub restrictions: Vec<String>,
    /// Composition kind.
    pub composition: Option<String>,
    /// Selected agent members.
    pub members: Vec<String>,
    /// Enabled skills.
    pub skills: Vec<String>,
    /// MCP connections.
    pub mcp: Vec<String>,
    /// Secret references.
    pub secret_refs: Vec<String>,
    /// Context compaction budget.
    pub compaction_budget: Option<u64>,
    /// Reasoning effort applied through the selected provider.
    pub reasoning: Option<ReasoningLevel>,
}

/// One profile action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileAction {
    /// List profiles on the Worker.
    List,
    /// Show one profile.
    Show {
        /// Profile name.
        name: String,
    },
    /// Create a profile.
    Create {
        /// Profile name.
        name: String,
        /// Revision inputs.
        options: ProfileRevisionOptions,
    },
    /// Revise an observed profile revision.
    Revise {
        /// Profile name.
        name: String,
        /// Observed revision.
        expected: u64,
        /// Revision inputs.
        options: ProfileRevisionOptions,
    },
    /// Validate one revision.
    Validate {
        /// Profile name.
        name: String,
        /// Optional revision.
        revision: Option<u64>,
    },
    /// Activate one revision.
    Activate {
        /// Profile name.
        name: String,
        /// Revision.
        revision: u64,
    },
    /// Deactivate the profile.
    Deactivate {
        /// Profile name.
        name: String,
    },
    /// Delete the profile.
    Delete {
        /// Profile name.
        name: String,
    },
}

/// Profile commands plus the owning Worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileCommand {
    /// Owning Worker.
    pub worker: Option<String>,
    /// Requested action.
    pub action: ProfileAction,
}

pub(crate) fn redact_profile(mut profile: Profile) -> Profile {
    for revision in &mut profile.revisions {
        revision.instruction = common::redact(&revision.instruction);
    }
    profile
}

pub(crate) fn profile_line(profile: &Profile, worker: &str) -> String {
    let revisions = profile
        .revisions
        .iter()
        .map(|revision| revision.revision.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "profile={} worker={} active_revision={} revisions={}",
        profile.name, worker, profile.active_revision, revisions
    )
}

pub(crate) fn profile_output(
    profile: &Profile,
    worker: &str,
    json_output: bool,
) -> Result<(), ExitCode> {
    let profile = redact_profile(profile.clone());
    if json_output {
        return json(&profile);
    }
    println!("{}", profile_line(&profile, worker));
    for revision in &profile.revisions {
        println!(
            "revision={} model={} composition={} members={} capabilities={} skills={} instruction={}",
            revision.revision,
            revision.model,
            revision.composition,
            revision.members.join(","),
            revision.capabilities.join(","),
            revision.skills.join(","),
            revision.instruction
        );
    }
    Ok(())
}

pub(crate) fn profile_revision(options: &ProfileRevisionOptions) -> ProfileRevision {
    ProfileRevision {
        revision: 0,
        instruction: options.instruction.clone(),
        model: options
            .model
            .clone()
            .unwrap_or_else(|| "deterministic".into()),
        capabilities: options.capabilities.clone(),
        grants: if options.grants.is_empty() {
            vec!["task:submit".into()]
        } else {
            options.grants.clone()
        },
        restrictions: options.restrictions.clone(),
        composition: options
            .composition
            .clone()
            .unwrap_or_else(|| "single".into()),
        members: options.members.clone(),
        skills: options.skills.clone(),
        mcp: options.mcp.clone(),
        workflow: None,
        limits: Limits::default(),
        compaction: options
            .compaction_budget
            .map(|context_budget| common::ContextCompaction {
                context_budget,
                ..common::ContextCompaction::default()
            }),
        secret_refs: options.secret_refs.clone(),
        reasoning: options.reasoning,
        created_at: 0,
    }
}

pub(crate) fn profile_worker(command: &ProfileCommand) -> Result<&str, String> {
    command
        .worker
        .as_deref()
        .ok_or_else(|| "gb-client: profile commands require --worker <id>".to_string())
}

pub(crate) async fn profile_command(
    command: ProfileCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    let worker = match profile_worker(&command) {
        Ok(worker) => worker.to_string(),
        Err(message) => return usage(&message),
    };
    let worker = worker.as_str();
    match command.action {
        ProfileAction::List => match client.profiles(worker).await {
            Ok(profiles) => {
                if json_output {
                    let profiles: Vec<_> = profiles.into_iter().map(redact_profile).collect();
                    return match json(&profiles) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                if profiles.is_empty() {
                    println!("no profiles on worker {worker}");
                    return ExitCode::SUCCESS;
                }
                for profile in profiles {
                    println!("{}", profile_line(&profile, worker));
                }
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        ProfileAction::Show { name } => match client.profile(worker, &name).await {
            Ok(profile) => match profile_output(&profile, worker, json_output) {
                Ok(()) => ExitCode::SUCCESS,
                Err(code) => code,
            },
            Err(error) => api_error(error),
        },
        ProfileAction::Create { name, options } => {
            let request = CreateProfileRequest {
                name,
                revision: profile_revision(&options),
            };
            match client.create_profile(worker, &request).await {
                Ok(profile) => match profile_output(&profile, worker, json_output) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                },
                Err(error) => api_error(error),
            }
        }
        ProfileAction::Revise {
            name,
            expected,
            options,
        } => {
            let request = ReviseProfileRequest {
                expected_revision: expected,
                revision: profile_revision(&options),
            };
            match client.revise_profile(worker, &name, &request).await {
                Ok(profile) => match profile_output(&profile, worker, json_output) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                },
                Err(error) => api_error(error),
            }
        }
        ProfileAction::Validate { name, revision } => {
            let request = ProfileVersionRequest { revision };
            match client.validate_profile(worker, &name, &request).await {
                Ok(profile) => match profile_output(&profile, worker, json_output) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                },
                Err(error) => api_error(error),
            }
        }
        ProfileAction::Activate { name, revision } => {
            let request = ProfileVersionRequest {
                revision: Some(revision),
            };
            match client.activate_profile(worker, &name, &request).await {
                Ok(profile) => match profile_output(&profile, worker, json_output) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                },
                Err(error) => api_error(error),
            }
        }
        ProfileAction::Deactivate { name } => {
            match client.deactivate_profile(worker, &name).await {
                Ok(profile) => match profile_output(&profile, worker, json_output) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                },
                Err(error) => api_error(error),
            }
        }
        ProfileAction::Delete { name } => match client.delete_profile(worker, &name).await {
            Ok(deleted) => {
                if json_output {
                    return match json(&deleted) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                println!("deleted profile {} on worker {worker}", deleted.deleted);
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
    }
}
