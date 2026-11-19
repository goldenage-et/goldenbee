//! Stored Gateway profile administration.

use crate::cli::output::{api_error, json};
use crate::cli::profiles::{mask, Profile as GatewayProfile, Store};
use crate::repl;
use serde::Serialize;
use std::process::ExitCode;

/// Typed Gateway profile commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayCommand {
    /// Register or update a profile.
    Add {
        /// Profile name.
        name: String,
        /// Gateway base URL.
        url: String,
        /// Caller token.
        token: String,
    },
    /// Select the active profile.
    Switch {
        /// Profile name.
        name: String,
    },
    /// Open a session on a named profile.
    Open {
        /// Profile name.
        name: String,
        /// Initial Worker.
        worker: Option<String>,
        /// Initial agent.
        agent: Option<String>,
    },
    /// List profiles.
    List,
    /// Show one profile.
    Detail {
        /// Profile name.
        name: String,
    },
}

#[derive(Serialize)]
struct ProfileView<'a> {
    name: &'a str,
    url: &'a str,
    token: String,
    active: bool,
}

fn view(profile: &GatewayProfile) -> ProfileView<'_> {
    ProfileView {
        name: &profile.name,
        url: &profile.url,
        token: mask(&profile.token),
        active: profile.active,
    }
}

pub(crate) async fn gateway_command(command: GatewayCommand, json_output: bool) -> ExitCode {
    let store = match Store::open().await {
        Ok(store) => store,
        Err(error) => return api_error(error),
    };
    match command {
        GatewayCommand::Add { name, url, token } => match store.add(&name, &url, &token).await {
            Ok(created) => {
                let active = store
                    .active()
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|profile| profile.name == name);
                println!(
                    "{} gateway {name} {url}{}",
                    if created { "added" } else { "updated" },
                    if active { " (active)" } else { "" }
                );
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        GatewayCommand::Switch { name } => match store.set_active(&name).await {
            Ok(()) => {
                println!("active gateway: {name}");
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        GatewayCommand::List => match store.list().await {
            Ok(profiles) => {
                if json_output {
                    let views: Vec<_> = profiles.iter().map(view).collect();
                    return match json(&views) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                if profiles.is_empty() {
                    println!("no gateways registered");
                    return ExitCode::SUCCESS;
                }
                for profile in &profiles {
                    println!(
                        "{} {} {}{}",
                        profile.name,
                        profile.url,
                        mask(&profile.token),
                        if profile.active { " active" } else { "" }
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        GatewayCommand::Detail { name } => match store.get(&name).await {
            Ok(profile) => {
                if json_output {
                    return match json(&view(&profile)) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                println!("name: {}", profile.name);
                println!("url: {}", profile.url);
                println!("token: {}", mask(&profile.token));
                println!("active: {}", profile.active);
                println!("database: {}", store.path().display());
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        GatewayCommand::Open {
            name,
            worker,
            agent,
        } => match store.get(&name).await {
            Ok(profile) => repl::open(profile, worker, agent).await,
            Err(error) => api_error(error),
        },
    }
}
