//! Provider-owned model management.
//!
//! Models are discovered from a provider. They are not standalone Gateway
//! entities, so every action observes and updates one provider revision.

use crate::cli::output::{api_error, json};
use crate::commands::provider::provider_view;
use sdk::Client;
use std::process::ExitCode;

/// Model management commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelCommand {
    /// List providers and their discovered models.
    List {
        /// Optional provider filter.
        provider: Option<String>,
    },
    /// Mark models as allowed.
    Allow {
        /// Provider id.
        provider: String,
        /// Models to allow.
        models: Vec<String>,
    },
    /// Clear the provider allow-list.
    Clear {
        /// Provider id.
        provider: String,
    },
    /// Set the preferred model.
    Preferred {
        /// Provider id.
        provider: String,
        /// Preferred model id.
        model: String,
    },
    /// Refresh discovery.
    Discover {
        /// Provider id.
        provider: String,
    },
}

pub(crate) async fn model_command(
    command: ModelCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    match command {
        ModelCommand::List { provider } => {
            let result = match provider {
                Some(id) => client.provider(&id).await.map(|provider| vec![provider]),
                None => client.providers().await,
            };
            match result {
                Ok(providers) => {
                    if json_output {
                        return match json(&providers) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    for provider in &providers {
                        if provider.models.is_empty() {
                            println!("{} no discovered models", provider.id);
                            continue;
                        }
                        let allowed = provider.allowed_models.as_ref();
                        for model in &provider.models {
                            let marked = allowed.is_none_or(|allowed| allowed.contains(model));
                            let preferred = provider.model == *model;
                            println!(
                                "{} {} allowed={} preferred={}",
                                provider.id, model, marked, preferred
                            );
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        ModelCommand::Allow { provider, models } => {
            update(
                client,
                &provider,
                |options| {
                    options.allowed_models = Some(models);
                },
                json_output,
            )
            .await
        }
        ModelCommand::Clear { provider } => {
            update(
                client,
                &provider,
                |options| {
                    options.allowed_models = Some(Vec::new());
                },
                json_output,
            )
            .await
        }
        ModelCommand::Preferred { provider, model } => {
            update(
                client,
                &provider,
                |options| {
                    options.model = Some(model);
                },
                json_output,
            )
            .await
        }
        ModelCommand::Discover { provider } => match client.discover_provider(&provider).await {
            Ok(metadata) => {
                if json_output {
                    return match json(&metadata) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                provider_view(&metadata);
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
    }
}

async fn update(
    client: &Client,
    provider: &str,
    change: impl FnOnce(&mut common::UpdateProviderRequest),
    json_output: bool,
) -> ExitCode {
    let observed = match client.provider(provider).await {
        Ok(observed) => observed,
        Err(error) => return api_error(error),
    };
    let mut request = common::UpdateProviderRequest {
        revision: observed.revision,
        adapter: None,
        model: None,
        endpoint: None,
        allowed_models: None,
        allow_auto_default: None,
        modalities: None,
        tool_calling: None,
        context_window: None,
        local: None,
        residency: None,
        allowed_workers: None,
        active: None,
    };
    change(&mut request);
    match client.update_provider(provider, &request).await {
        Ok(metadata) => {
            if json_output {
                return match json(&metadata) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => code,
                };
            }
            provider_view(&metadata);
            ExitCode::SUCCESS
        }
        Err(error) => api_error(error),
    }
}
