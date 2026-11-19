//! Management shell state and dialog application.

use crate::session;
use crate::tui::event::{spawn, Action, UiEvent};
use crate::tui::forms::{
    capability_list, worker_list, ConfirmAction, Dialog, ProviderForm, SecretAction, SelectAction,
    TextAction, WorkerForm,
};
use crate::tui::pages::chat::ChatState;
use crate::tui::pages::models::ModelsPage;
use crate::tui::pages::providers::ProvidersPage;
use crate::tui::pages::workers::WorkersPage;
use crate::tui::pages::Page;
use common::{ApiError, WorkerSummary};
use sdk::Client;
use tokio::sync::mpsc::UnboundedSender;

/// Whether the chat or management surface is visible.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    /// Full conversation.
    Chat,
    /// Management pages.
    Manage,
}

/// Everything one interactive session owns.
pub struct App {
    /// Active Gateway profile name.
    pub profile: String,
    /// Active Gateway URL.
    pub gateway: String,
    /// Visible surface.
    pub screen: Screen,
    /// Active management page.
    pub page: Page,
    /// Chat state.
    pub chat: ChatState,
    /// Workers page.
    pub workers: WorkersPage,
    /// Providers page.
    pub providers: ProvidersPage,
    /// Models page.
    pub models: ModelsPage,
    /// Open dialog.
    pub dialog: Option<Dialog>,
    /// Bottom status text.
    pub status: String,
    /// Spinner animation for background work.
    pub spinner: usize,
    /// Whether the session should close.
    pub quit: bool,
}

impl App {
    /// Start a chat with the initial Worker and agent selection.
    pub fn new(
        profile: &str,
        gateway: &str,
        workers: Vec<WorkerSummary>,
        worker: String,
        agent: String,
    ) -> Self {
        Self {
            profile: profile.to_string(),
            gateway: gateway.to_string(),
            screen: Screen::Chat,
            page: Page::Workers,
            chat: ChatState::new(profile, gateway, workers, worker, agent),
            workers: WorkersPage::default(),
            providers: ProvidersPage::default(),
            models: ModelsPage::default(),
            dialog: None,
            status: String::new(),
            spinner: 0,
            quit: false,
        }
    }

    /// Show management on one page, optionally with a dialog.
    pub fn open_manage(&mut self, page: Page, dialog: Option<Dialog>) {
        self.screen = Screen::Manage;
        self.page = page;
        self.dialog = dialog;
    }

    /// Return to chat.
    pub fn open_chat(&mut self) {
        self.screen = Screen::Chat;
        self.dialog = None;
    }

    /// Apply one background completion.
    pub fn update(&mut self, event: UiEvent) {
        match event {
            UiEvent::ProvidersLoaded(providers) => {
                self.providers.set(providers);
                if let Some(selected) = &self.models.provider {
                    if self.providers.provider(selected).is_none() {
                        self.models.provider = None;
                    }
                }
                self.models.clamp(&self.providers.providers);
            }
            UiEvent::WorkersLoaded(workers) => self.workers.set(workers),
            UiEvent::ModelsLoaded { provider, metadata } => {
                self.providers.upsert(metadata);
                if self.models.provider.is_none() {
                    self.models.provider = Some(provider);
                }
                self.models.clamp(&self.providers.providers);
            }
            UiEvent::ActionFinished { message } => {
                let first = message.lines().next().unwrap_or_default().to_string();
                self.status = first;
                if message.lines().count() > 1 {
                    self.dialog = Some(Dialog::Text {
                        title: "result".into(),
                        buffer: text_input(&message),
                        action: TextAction::Dismiss,
                    });
                }
            }
            UiEvent::Failed(error) => self.show_error(&error),
            UiEvent::ChatDelta(text) => self.chat.delta(&text),
            UiEvent::ChatThinking(text) => self.chat.thinking_delta(&text),
            UiEvent::ChatToolCall {
                id,
                name,
                server,
                agent,
            } => self.chat.tool_call(id, name, server, agent),
            UiEvent::ChatToolResult {
                id,
                name,
                ok,
                error_code,
                duration_ms,
            } => self.chat.tool_result(id, name, ok, error_code, duration_ms),
            UiEvent::ChatFinished {
                printed,
                output,
                error,
            } => self.chat.finish(&printed, &output, error),
            UiEvent::ChatFailed(error) => self.chat.abort(&error),
        }
    }

    fn show_error(&mut self, error: &ApiError) {
        self.status = session::error_text(error);
        if self.screen == Screen::Manage {
            self.dialog = Some(Dialog::Text {
                title: "error".into(),
                buffer: text_input(&session::error_text(error)),
                action: TextAction::Dismiss,
            });
        }
    }

    /// Apply a submitted dialog. Validation failures keep the dialog open.
    pub fn apply_dialog(
        &mut self,
        dialog: Dialog,
        client: &Client,
        sender: &UnboundedSender<UiEvent>,
    ) {
        match dialog {
            Dialog::Confirm { action, .. } => match action {
                ConfirmAction::WorkerRemove(id) => {
                    self.status = format!("removing worker {id}…");
                    spawn(client.clone(), Action::WorkerRemove { id }, sender.clone());
                }
                ConfirmAction::ProviderRevoke(id) => {
                    self.status = format!("revoking credential for {id}…");
                    spawn(
                        client.clone(),
                        Action::ProviderRevokeCredential { id },
                        sender.clone(),
                    );
                }
            },
            Dialog::Secret {
                title,
                buffer,
                action,
            } => {
                let value = buffer.text();
                if value.trim().is_empty() {
                    self.status = "credential must not be empty".to_string();
                    self.dialog = Some(Dialog::Secret {
                        title,
                        buffer,
                        action,
                    });
                    return;
                }
                match action {
                    SecretAction::ProviderCredential(id) => {
                        self.status = format!("writing credential for {id}…");
                        spawn(
                            client.clone(),
                            Action::ProviderSetCredential {
                                id,
                                key: value.trim().to_string(),
                            },
                            sender.clone(),
                        );
                    }
                }
            }
            Dialog::Text {
                title,
                buffer,
                action,
            } => {
                let value = buffer.text();
                match action {
                    TextAction::Dismiss => {}
                    TextAction::ProviderWorkers(id) => {
                        let Some(provider) = self.providers.provider(&id) else {
                            self.status = format!("provider {id} is no longer loaded");
                            return;
                        };
                        let mut options = crate::commands::provider::ProviderUpdateOptions {
                            revision: provider.revision,
                            ..Default::default()
                        };
                        options.allowed_workers = Some(worker_list(&value));
                        self.status = format!("updating workers for {id}…");
                        spawn(
                            client.clone(),
                            Action::ProviderUpdate { id, options },
                            sender.clone(),
                        );
                    }
                    TextAction::PairInstance { worker } => {
                        let instance = value.trim().to_string();
                        if instance.is_empty() {
                            self.status = "instance must not be empty".to_string();
                            self.dialog = Some(Dialog::Text {
                                title,
                                buffer,
                                action: TextAction::PairInstance { worker },
                            });
                            return;
                        }
                        self.status = format!("minting pairing code for {worker}…");
                        spawn(
                            client.clone(),
                            Action::ProviderPair { worker, instance },
                            sender.clone(),
                        );
                    }
                }
            }
            Dialog::Select {
                items,
                selected,
                action,
                ..
            } => match action {
                SelectAction::PairWorker => {
                    let Some(worker) = items.get(selected).cloned() else {
                        self.status = "no Worker is loaded for pairing".to_string();
                        return;
                    };
                    self.dialog = Some(Dialog::Text {
                        title: "Pair instance".into(),
                        buffer: crate::tui::input::Input::default(),
                        action: TextAction::PairInstance { worker },
                    });
                }
            },
            Dialog::ProviderForm(form) => self.submit_provider_form(form, client, sender),
            Dialog::WorkerForm(form) => self.submit_worker_form(form, client, sender),
        }
    }

    fn submit_provider_form(
        &mut self,
        form: ProviderForm,
        client: &Client,
        sender: &UnboundedSender<UiEvent>,
    ) {
        let id = if form.editing {
            form.provider.clone()
        } else {
            form.id.text().trim().to_string()
        };
        let adapter = form.adapter.text().trim().to_string();
        let model = form.model.text().trim().to_string();
        let endpoint = form.endpoint.text().trim().to_string();
        let workers = worker_list(&form.workers.text());
        if id.is_empty() || adapter.is_empty() || workers.is_empty() {
            self.status = "provider id, adapter, and workers are required".to_string();
            self.dialog = Some(Dialog::ProviderForm(form));
            return;
        }
        let local =
            adapter == "ollama" || endpoint.contains("127.0.0.1") || endpoint.contains("localhost");
        if form.editing {
            let options = crate::commands::provider::ProviderUpdateOptions {
                revision: form.revision,
                adapter: Some(adapter),
                model: Some(model),
                endpoint: Some(endpoint),
                allowed_workers: Some(workers),
                local: Some(local),
                ..Default::default()
            };
            self.status = format!("updating provider {id}…");
            spawn(
                client.clone(),
                Action::ProviderUpdate { id, options },
                sender.clone(),
            );
        } else {
            let options = crate::commands::provider::ProviderAddOptions {
                adapter,
                model,
                endpoint: (!endpoint.is_empty()).then_some(endpoint),
                allowed_models: None,
                auto_default: false,
                modalities: vec!["text".to_string()],
                tool_calling: false,
                context_window: None,
                local,
                residency: None,
                allowed_workers: workers,
            };
            self.status = format!("creating provider {id}…");
            spawn(
                client.clone(),
                Action::ProviderCreate { id, options },
                sender.clone(),
            );
        }
    }

    fn submit_worker_form(
        &mut self,
        form: WorkerForm,
        client: &Client,
        sender: &UnboundedSender<UiEvent>,
    ) {
        let id = if form.editing {
            form.worker.clone()
        } else {
            form.id.text().trim().to_string()
        };
        let endpoint = form.endpoint.text().trim().to_string();
        let instance = form.instance.text().trim().to_string();
        let package = form.package.text().trim().to_string();
        let capabilities = capability_list(&form.capabilities.text());
        if id.is_empty() || endpoint.is_empty() {
            self.status = "worker id and endpoint are required".to_string();
            self.dialog = Some(Dialog::WorkerForm(form));
            return;
        }
        if form.editing {
            let options = crate::commands::worker::UpdateWorkerOptions {
                revision: form.revision,
                endpoint: Some(endpoint),
                instance: (!instance.is_empty()).then_some(instance),
                package: (!package.is_empty()).then_some(package),
                capabilities: (!capabilities.is_empty()).then_some(capabilities),
                key: None,
            };
            self.status = format!("updating worker {id}…");
            spawn(
                client.clone(),
                Action::WorkerUpdate { id, options },
                sender.clone(),
            );
        } else {
            let options = crate::commands::worker::AddWorkerOptions {
                id: id.clone(),
                endpoint,
                instance: (!instance.is_empty()).then_some(instance),
                package: (!package.is_empty()).then_some(package),
                capabilities: (!capabilities.is_empty()).then_some(capabilities),
                control_token_stdin: false,
                preflight: true,
                key: None,
            };
            self.status = format!("adding worker {id}…");
            spawn(client.clone(), Action::WorkerAdd(options), sender.clone());
        }
    }
}

fn text_input(text: &str) -> crate::tui::input::Input {
    let mut input = crate::tui::input::Input::default();
    input.set_text(text);
    input
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::forms::{ConfirmAction, Dialog};
    use common::AdminWorkerSummary;

    #[test]
    fn management_loads_and_clamps_pages() {
        let mut app = App::new(
            "local",
            "http://127.0.0.1:8088",
            Vec::new(),
            "w".into(),
            "a".into(),
        );
        app.open_manage(Page::Workers, None);
        app.update(UiEvent::WorkersLoaded(vec![worker()]));
        assert_eq!(app.workers.filtered().len(), 1);
        assert_eq!(app.screen, Screen::Manage);
    }

    #[test]
    fn failures_open_a_readable_dialog() {
        let mut app = App::new(
            "local",
            "http://127.0.0.1:8088",
            Vec::new(),
            "w".into(),
            "a".into(),
        );
        app.open_manage(Page::Providers, None);
        let mut error = ApiError::new("scope.denied", "denied");
        error.recovery = "admin/provider:admin scope required; switch to an admin profile".into();
        app.update(UiEvent::Failed(error));
        let Dialog::Text { buffer, .. } = app.dialog.as_ref().unwrap() else {
            panic!("expected text dialog");
        };
        assert!(buffer.text().contains("switch to an admin profile"));
    }

    fn worker() -> AdminWorkerSummary {
        AdminWorkerSummary {
            id: "worker-a".into(),
            endpoint: "http://127.0.0.1:9090".into(),
            instance: "one".into(),
            package: "echo".into(),
            capabilities: None,
            approved_capabilities: Vec::new(),
            declared_capabilities: Vec::new(),
            enabled: true,
            revision: 1,
            source: "config".into(),
            status: "approved".into(),
            reachable: true,
            control_token_set: true,
            incarnation: "1".into(),
            incarnations: Vec::new(),
            locality: "local".into(),
            lease_expires_at: 0,
            renewed_at: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn confirm_dialogs_carry_the_destructive_target() {
        let dialog = Dialog::Confirm {
            title: "remove".into(),
            body: "worker-a".into(),
            action: ConfirmAction::WorkerRemove("worker-a".into()),
        };
        assert_eq!(dialog.title(), "remove");
    }
}
