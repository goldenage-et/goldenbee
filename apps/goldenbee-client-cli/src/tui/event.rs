//! Async UI events and background Gateway actions.
//!
//! Every SDK call runs in its own task. Key handling only enqueues an action;
//! it never awaits a Gateway call. All completions return through one channel.

use crate::cli::output::default_key;
use crate::commands::provider::{ProviderAddOptions, ProviderUpdateOptions};
use crate::commands::worker::{AddWorkerOptions, UpdateWorkerOptions};
use common::{AdminWorkerSummary, ApiError, ProviderMetadata};
use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use sdk::Client;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// Provider administrative scope shown when a caller is under-privileged.
const PROVIDER_SCOPE: &str = "admin/provider:admin";
/// Worker administrative scope shown when a caller is under-privileged.
const WORKER_SCOPE: &str = "admin";

/// One completion or data change delivered to the UI.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum UiEvent {
    /// Provider catalog loaded.
    ProvidersLoaded(Vec<ProviderMetadata>),
    /// Approved Worker inventory loaded.
    WorkersLoaded(Vec<AdminWorkerSummary>),
    /// One provider's models refreshed.
    ModelsLoaded {
        /// Provider id.
        provider: String,
        /// Fresh provider metadata.
        metadata: ProviderMetadata,
    },
    /// A management action completed.
    ActionFinished {
        /// Human-readable result.
        message: String,
    },
    /// A management action failed.
    Failed(ApiError),
    /// Live model text for the chat.
    ChatDelta(String),
    /// Live model reasoning text for the chat.
    ChatThinking(String),
    /// One tool call from the chat task stream.
    ChatToolCall {
        /// Model-assigned call identity.
        id: String,
        /// Tool name.
        name: String,
        /// Tool server or namespace.
        server: Option<String>,
        /// Requesting agent.
        agent: Option<String>,
    },
    /// One tool result from the chat task stream.
    ChatToolResult {
        /// Model-assigned call identity.
        id: String,
        /// Tool name.
        name: String,
        /// Whether the result is successful.
        ok: bool,
        /// Stable error code on failure.
        error_code: Option<String>,
        /// Call duration when reported.
        duration_ms: Option<u64>,
    },
    /// Chat task completion.
    ChatFinished {
        /// Text already streamed.
        printed: String,
        /// Authoritative final output.
        output: String,
        /// Terminal task error.
        error: Option<ApiError>,
    },
    /// Chat task failure.
    ChatFailed(ApiError),
}

/// Input events mirrored into the async loop.
pub enum InputEvent {
    /// Key press.
    Key(KeyEvent),
    /// Bracketed paste.
    Paste(String),
    /// Terminal resize.
    Resize,
}

/// One background Gateway operation.
#[derive(Clone)]
pub enum Action {
    /// Refresh the provider catalog.
    LoadProviders,
    /// Refresh the approved Worker inventory.
    LoadWorkers,
    /// Refresh one provider's models.
    LoadModels {
        /// Provider id.
        provider: String,
    },
    /// Create a provider.
    ProviderCreate {
        /// Provider id.
        id: String,
        /// Creation metadata.
        options: ProviderAddOptions,
    },
    /// Update provider metadata.
    ProviderUpdate {
        /// Provider id.
        id: String,
        /// Observed revision and changes.
        options: ProviderUpdateOptions,
    },
    /// Refresh provider discovery.
    ProviderDiscover {
        /// Provider id.
        id: String,
    },
    /// Write a provider credential.
    ProviderSetCredential {
        /// Provider id.
        id: String,
        /// Secret value. Never rendered or logged.
        key: String,
    },
    /// Revoke a provider credential.
    ProviderRevokeCredential {
        /// Provider id.
        id: String,
    },
    /// Mint a one-time pairing code.
    ProviderPair {
        /// Worker id.
        worker: String,
        /// Instance identity.
        instance: String,
    },
    /// Add an approved Worker.
    WorkerAdd(AddWorkerOptions),
    /// Update an approved Worker.
    WorkerUpdate {
        /// Worker id.
        id: String,
        /// Observed revision and changes.
        options: UpdateWorkerOptions,
    },
    /// Disable one Worker.
    WorkerDisable {
        /// Worker id.
        id: String,
    },
    /// Enable one Worker.
    WorkerEnable {
        /// Worker id.
        id: String,
    },
    /// Probe one Worker.
    WorkerProbe {
        /// Worker id.
        id: String,
    },
    /// Remove one Worker.
    WorkerRemove {
        /// Worker id.
        id: String,
    },
    /// Read Worker audit.
    WorkerAudit {
        /// Optional Worker filter.
        worker: Option<String>,
    },
    /// Update one provider's model allow-list or preferred model.
    ModelUpdate {
        /// Provider id.
        id: String,
        /// Observed revision.
        revision: u64,
        /// New allow-list.
        allowed_models: Option<Vec<String>>,
        /// New preferred model.
        model: Option<String>,
    },
}

/// Run one action and send every resulting UI event.
pub fn spawn(client: Client, action: Action, sender: UnboundedSender<UiEvent>) {
    tokio::spawn(async move {
        for event in execute(&client, action).await {
            if sender.send(event).is_err() {
                return;
            }
        }
    });
}

/// Input reader thread mirrored into the async loop.
pub fn spawn_input() -> UnboundedReceiver<InputEvent> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let _ = std::thread::Builder::new()
        .name("cli-input".to_string())
        .spawn(move || {
            while let Ok(event) = event::read() {
                let event = match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => InputEvent::Key(key),
                    Event::Paste(text) => InputEvent::Paste(text),
                    Event::Resize(_, _) => InputEvent::Resize,
                    _ => continue,
                };
                if sender.send(event).is_err() {
                    break;
                }
            }
        });
    receiver
}

async fn execute(client: &Client, action: Action) -> Vec<UiEvent> {
    match action {
        Action::LoadProviders => load_providers(client).await,
        Action::LoadWorkers => load_workers(client).await,
        Action::LoadModels { provider } => load_models(client, &provider).await,
        Action::ProviderCreate { id, options } => {
            let request = common::CreateProviderRequest {
                id,
                adapter: options.adapter.clone(),
                model: options.model.clone(),
                endpoint: options.endpoint.clone(),
                allowed_models: options.allowed_models.clone(),
                allow_auto_default: options.auto_default,
                modalities: options.modalities.clone(),
                tool_calling: options.tool_calling,
                context_window: options.context_window,
                local: options.local,
                residency: options.residency.clone().unwrap_or_else(|| {
                    if options.local {
                        "local".into()
                    } else {
                        "remote".into()
                    }
                }),
                allowed_workers: options.allowed_workers.clone(),
            };
            match client.create_provider(&request).await {
                Ok(provider) => {
                    let mut events = vec![UiEvent::ActionFinished {
                        message: format!("provider {} created", provider.id),
                    }];
                    events.extend(load_providers(client).await);
                    events.extend(load_models(client, &provider.id).await);
                    events
                }
                Err(error) => vec![failure(error, PROVIDER_SCOPE)],
            }
        }
        Action::ProviderUpdate { id, options } => {
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
                    let mut events = vec![UiEvent::ActionFinished {
                        message: format!("provider {} updated", provider.id),
                    }];
                    events.extend(load_providers(client).await);
                    events.extend(load_models(client, &provider.id).await);
                    events
                }
                Err(error) => vec![failure(error, PROVIDER_SCOPE)],
            }
        }
        Action::ProviderDiscover { id } => match client.discover_provider(&id).await {
            Ok(provider) => {
                let mut events = vec![UiEvent::ActionFinished {
                    message: format!(
                        "provider {} discovery refreshed: {} model(s)",
                        provider.id,
                        provider.models.len()
                    ),
                }];
                events.extend(load_providers(client).await);
                events.extend(load_models(client, &id).await);
                events
            }
            Err(error) => vec![failure(error, PROVIDER_SCOPE)],
        },
        Action::ProviderSetCredential { id, key } => {
            match client.set_provider_credential(&id, &key).await {
                Ok(summary) => {
                    let mut events = vec![UiEvent::ActionFinished {
                        message: format!(
                            "provider {} credential v{} set; ready={} models={}",
                            summary.provider,
                            summary.version,
                            summary.ready,
                            summary.models.len()
                        ),
                    }];
                    events.extend(load_providers(client).await);
                    events.extend(load_models(client, &summary.provider).await);
                    events
                }
                Err(error) => vec![failure(error, PROVIDER_SCOPE)],
            }
        }
        Action::ProviderRevokeCredential { id } => {
            match client.revoke_provider_credential(&id).await {
                Ok(summary) => {
                    let mut events = vec![UiEvent::ActionFinished {
                        message: format!(
                            "provider {} credential v{} revoked",
                            summary.provider, summary.version
                        ),
                    }];
                    events.extend(load_providers(client).await);
                    events.extend(load_models(client, &summary.provider).await);
                    events
                }
                Err(error) => vec![failure(error, PROVIDER_SCOPE)],
            }
        }
        Action::ProviderPair { worker, instance } => {
            match client.provider_pair_code(&worker, &instance).await {
                Ok(pair) => vec![UiEvent::ActionFinished {
                    message: format!(
                        "one-time pairing code (treat as secret)\ncode: {}\nlink: {}\nworker: {}  instance: {}\nexpires_at: {}",
                        pair.code, pair.link, pair.worker_id, pair.instance, pair.expires_at
                    ),
                }],
                Err(error) => vec![failure(error, PROVIDER_SCOPE)],
            }
        }
        Action::WorkerAdd(options) => {
            let request = common::AddWorkerRequest {
                id: options.id.clone(),
                endpoint: options.endpoint,
                instance: options.instance,
                package: options.package,
                capabilities: options.capabilities,
                control_token: None,
                preflight: options.preflight,
                idempotency_key: options.key.unwrap_or_else(default_key),
            };
            match client.add_worker(&request).await {
                Ok(response) => {
                    let message = match response.control_token {
                        Some(token) => format!(
                            "worker {} added\ncontrol token (shown once): {token}",
                            response.worker.id
                        ),
                        None => format!("worker {} added", response.worker.id),
                    };
                    let mut events = vec![UiEvent::ActionFinished { message }];
                    events.extend(load_workers(client).await);
                    events
                }
                Err(error) => vec![failure(error, WORKER_SCOPE)],
            }
        }
        Action::WorkerUpdate { id, options } => {
            let request = common::UpdateWorkerRequest {
                revision: options.revision,
                endpoint: options.endpoint,
                instance: options.instance,
                package: options.package,
                capabilities: options.capabilities,
                idempotency_key: Some(options.key.unwrap_or_else(default_key)),
            };
            match client.update_worker(&id, &request).await {
                Ok(worker) => {
                    let mut events = vec![UiEvent::ActionFinished {
                        message: format!("worker {} updated", worker.id),
                    }];
                    events.extend(load_workers(client).await);
                    events
                }
                Err(error) => vec![failure(error, WORKER_SCOPE)],
            }
        }
        Action::WorkerDisable { id } => {
            worker_lifecycle(client, id, WorkerLifecycle::Disable).await
        }
        Action::WorkerEnable { id } => worker_lifecycle(client, id, WorkerLifecycle::Enable).await,
        Action::WorkerProbe { id } => worker_lifecycle(client, id, WorkerLifecycle::Probe).await,
        Action::WorkerRemove { id } => worker_lifecycle(client, id, WorkerLifecycle::Remove).await,
        Action::WorkerAudit { worker } => match client.worker_audit(worker.as_deref()).await {
            Ok(entries) => {
                let message = if entries.is_empty() {
                    "no audit entries".to_string()
                } else {
                    entries
                        .iter()
                        .map(|entry| {
                            format!(
                                "{} {} {} {} {} {}",
                                entry.at,
                                entry.action,
                                entry.worker,
                                entry.principal,
                                entry.outcome,
                                entry.detail
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                vec![UiEvent::ActionFinished { message }]
            }
            Err(error) => vec![failure(error, WORKER_SCOPE)],
        },
        Action::ModelUpdate {
            id,
            revision,
            allowed_models,
            model,
        } => {
            let request = common::UpdateProviderRequest {
                revision,
                adapter: None,
                model,
                endpoint: None,
                allowed_models,
                allow_auto_default: None,
                modalities: None,
                tool_calling: None,
                context_window: None,
                local: None,
                residency: None,
                allowed_workers: None,
                active: None,
            };
            match client.update_provider(&id, &request).await {
                Ok(metadata) => {
                    let mut events = vec![UiEvent::ActionFinished {
                        message: format!("provider {} models updated", metadata.id),
                    }];
                    events.push(UiEvent::ModelsLoaded {
                        provider: metadata.id.clone(),
                        metadata,
                    });
                    events.extend(load_providers(client).await);
                    events
                }
                Err(error) => vec![failure(error, PROVIDER_SCOPE)],
            }
        }
    }
}

enum WorkerLifecycle {
    Disable,
    Enable,
    Probe,
    Remove,
}

async fn worker_lifecycle(client: &Client, id: String, action: WorkerLifecycle) -> Vec<UiEvent> {
    let result = match action {
        WorkerLifecycle::Disable => client.disable_worker(&id).await,
        WorkerLifecycle::Enable => client.enable_worker(&id).await,
        WorkerLifecycle::Probe => client.probe_worker(&id).await,
        WorkerLifecycle::Remove => client.decommission_worker(&id).await,
    };
    match result {
        Ok(worker) => {
            let label = match action {
                WorkerLifecycle::Disable => "disabled",
                WorkerLifecycle::Enable => "enabled",
                WorkerLifecycle::Probe => "probed",
                WorkerLifecycle::Remove => "removed",
            };
            let mut events = vec![UiEvent::ActionFinished {
                message: format!(
                    "worker {} {label} status={} reachable={}",
                    worker.id, worker.status, worker.reachable
                ),
            }];
            events.extend(load_workers(client).await);
            events
        }
        Err(error) => vec![failure(error, WORKER_SCOPE)],
    }
}

async fn load_providers(client: &Client) -> Vec<UiEvent> {
    match client.providers().await {
        Ok(providers) => vec![UiEvent::ProvidersLoaded(providers)],
        Err(error) => vec![failure(error, PROVIDER_SCOPE)],
    }
}

async fn load_workers(client: &Client) -> Vec<UiEvent> {
    match client.admin_workers().await {
        Ok(workers) => vec![UiEvent::WorkersLoaded(workers)],
        Err(error) => vec![failure(error, WORKER_SCOPE)],
    }
}

async fn load_models(client: &Client, provider: &str) -> Vec<UiEvent> {
    match client.provider(provider).await {
        Ok(metadata) => vec![UiEvent::ModelsLoaded {
            provider: metadata.id.clone(),
            metadata,
        }],
        Err(error) => vec![failure(error, PROVIDER_SCOPE)],
    }
}

fn failure(mut error: ApiError, scope: &str) -> UiEvent {
    if error.code.starts_with("scope.") || error.code.starts_with("auth.") {
        error.recovery = format!("{scope} scope required; switch to an admin profile");
    }
    UiEvent::Failed(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_failures_become_actionable() {
        let event = failure(ApiError::new("scope.denied", "denied"), PROVIDER_SCOPE);
        let UiEvent::Failed(error) = event else {
            panic!("expected failure");
        };
        assert_eq!(
            error.recovery,
            "admin/provider:admin scope required; switch to an admin profile"
        );
        let event = failure(ApiError::new("scope.denied", "denied"), WORKER_SCOPE);
        let UiEvent::Failed(error) = event else {
            panic!("expected failure");
        };
        assert_eq!(
            error.recovery,
            "admin scope required; switch to an admin profile"
        );
    }
}
