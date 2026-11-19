//! Model-provider administration.

use crate::cli::output::{api_error, json, usage};
use crate::cli::secret::read_stdin_secret;
use sdk::Client;
use std::process::ExitCode;

/// Provider metadata accepted when creating a provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderAddOptions {
    /// Native adapter kind.
    pub adapter: String,
    /// Preferred concrete model.
    pub model: String,
    /// Endpoint base URL.
    pub endpoint: Option<String>,
    /// Optional discovered-model allow-list.
    pub allowed_models: Option<Vec<String>>,
    /// Whether discovery may choose a default.
    pub auto_default: bool,
    /// Supported modalities.
    pub modalities: Vec<String>,
    /// Whether tool calling is supported.
    pub tool_calling: bool,
    /// Declared context window.
    pub context_window: Option<u64>,
    /// Whether the provider is local.
    pub local: bool,
    /// Declared residency.
    pub residency: Option<String>,
    /// Worker assignments.
    pub allowed_workers: Vec<String>,
}

/// Provider metadata accepted when updating a provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderUpdateOptions {
    /// Observed revision.
    pub revision: u64,
    /// New adapter.
    pub adapter: Option<String>,
    /// New preferred model.
    pub model: Option<String>,
    /// New endpoint.
    pub endpoint: Option<String>,
    /// New allow-list.
    pub allowed_models: Option<Vec<String>>,
    /// New auto-default policy.
    pub allow_auto_default: Option<bool>,
    /// New modalities.
    pub modalities: Option<Vec<String>>,
    /// New tool-calling declaration.
    pub tool_calling: Option<bool>,
    /// New context window.
    pub context_window: Option<u64>,
    /// New locality.
    pub local: Option<bool>,
    /// New residency.
    pub residency: Option<String>,
    /// New Worker assignments.
    pub allowed_workers: Option<Vec<String>>,
    /// New active state.
    pub active: Option<bool>,
}

/// Worker binding for a one-time pairing code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairOptions {
    /// Approved Worker id.
    pub worker: String,
    /// Worker instance identity.
    pub instance: String,
}

/// Provider administration commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderCommand {
    /// List providers.
    List,
    /// Show one provider.
    Show {
        /// Provider id.
        id: String,
    },
    /// Create a provider.
    Add {
        /// Provider id.
        id: String,
        /// Creation metadata.
        options: ProviderAddOptions,
    },
    /// Update provider metadata.
    Update {
        /// Provider id.
        id: String,
        /// Update metadata.
        options: ProviderUpdateOptions,
    },
    /// Refresh discovered models.
    Discover {
        /// Provider id.
        id: String,
    },
    /// Write a credential read from stdin.
    CredentialSet {
        /// Provider id.
        id: String,
    },
    /// Revoke the active credential.
    CredentialRevoke {
        /// Provider id.
        id: String,
    },
    /// Mint a one-time Worker pairing code.
    Pair(PairOptions),
}

pub(crate) fn provider_view(provider: &common::ProviderMetadata) {
    let workers = if provider.allowed_workers.is_empty() {
        "-".to_string()
    } else {
        provider.allowed_workers.join(",")
    };
    let models = if provider.models.is_empty() {
        "-".to_string()
    } else {
        provider.models.join(",")
    };
    println!(
        "{} adapter={} model={} ready={} active={} revision={} workers={} models={} credential={}",
        provider.id,
        provider.adapter,
        if provider.model.is_empty() {
            "-"
        } else {
            &provider.model
        },
        provider.ready,
        provider.active,
        provider.revision,
        workers,
        models,
        provider
            .credential_version
            .map_or("-".to_string(), |v| v.to_string())
    );
    if let Some(error) = &provider.discovery_error {
        println!("  discovery_error={error}");
    }
}

pub(crate) async fn provider_command(
    command: ProviderCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    match command {
        ProviderCommand::List => match client.providers().await {
            Ok(providers) => {
                if json_output {
                    return match json(&providers) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                if providers.is_empty() {
                    println!("no model providers configured");
                }
                for provider in &providers {
                    provider_view(provider);
                }
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        ProviderCommand::Show { id } => match client.provider(&id).await {
            Ok(provider) => {
                if json_output {
                    return match json(&provider) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                provider_view(&provider);
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        ProviderCommand::Add { id, options } => {
            let request = common::CreateProviderRequest {
                id,
                adapter: options.adapter,
                model: options.model,
                endpoint: options.endpoint,
                allowed_models: options.allowed_models,
                allow_auto_default: options.auto_default,
                modalities: options.modalities,
                tool_calling: options.tool_calling,
                context_window: options.context_window,
                local: options.local,
                residency: options.residency.unwrap_or_else(|| {
                    if options.local {
                        "local".into()
                    } else {
                        "remote".into()
                    }
                }),
                allowed_workers: options.allowed_workers,
            };
            match client.create_provider(&request).await {
                Ok(provider) => {
                    if json_output {
                        return match json(&provider) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    provider_view(&provider);
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        ProviderCommand::Update { id, options } => {
            let request = common::UpdateProviderRequest {
                revision: options.revision,
                adapter: options.adapter,
                model: options.model,
                endpoint: options.endpoint,
                allowed_models: options.allowed_models,
                allow_auto_default: options.allow_auto_default,
                modalities: options.modalities,
                tool_calling: options.tool_calling,
                context_window: options.context_window,
                local: options.local,
                residency: options.residency,
                allowed_workers: options.allowed_workers,
                active: options.active,
            };
            match client.update_provider(&id, &request).await {
                Ok(provider) => {
                    if json_output {
                        return match json(&provider) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    provider_view(&provider);
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        ProviderCommand::Discover { id } => match client.discover_provider(&id).await {
            Ok(provider) => {
                if json_output {
                    return match json(&provider) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                provider_view(&provider);
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        ProviderCommand::CredentialSet { id } => {
            let key = match read_stdin_secret("provider credential") {
                Ok(key) => key,
                Err(message) => return usage(&message),
            };
            match client.set_provider_credential(&id, &key).await {
                Ok(summary) => {
                    if json_output {
                        return match json(&summary) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    println!(
                        "{} credential={} fingerprint={} ready={} models={}",
                        summary.provider,
                        summary.version,
                        summary.fingerprint,
                        summary.ready,
                        summary.models.join(",")
                    );
                    if let Some(error) = &summary.discovery_error {
                        println!("discovery_error={error}");
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        ProviderCommand::CredentialRevoke { id } => {
            match client.revoke_provider_credential(&id).await {
                Ok(summary) => {
                    if json_output {
                        return match json(&summary) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    println!(
                        "{} credential={} revoked={}",
                        summary.provider,
                        summary.version,
                        summary.revoked_at.is_some()
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        ProviderCommand::Pair(options) => {
            match client
                .provider_pair_code(&options.worker, &options.instance)
                .await
            {
                Ok(pair) => {
                    if json_output {
                        return match json(&pair) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    println!(
                        "code={} worker={} instance={} expires_at={}",
                        pair.code, pair.worker_id, pair.instance, pair.expires_at
                    );
                    println!("link={}", pair.link);
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
    }
}
