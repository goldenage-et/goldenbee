//! Full-screen chat and management interface.
//!
//! The session keeps the same architecture as the rest of the CLI: submit and
//! follow tasks through the SDK, never through Gateway or Worker HTTP directly.
//! Management actions run in background tasks and return through one UI event
//! channel, so key handling never waits on a Gateway call.

pub mod app;
pub mod event;
pub mod forms;
pub mod frame;
pub mod input;
pub mod pages;
pub mod terminal;

use crate::cli::profiles::Profile;
use crate::session;
use app::{App, Screen};
use common::{GatewaySubmitRequest, WorkerSummary};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};
use event::{spawn, Action, InputEvent, UiEvent};
use forms::{
    ConfirmAction, Dialog, DialogOutcome, ProviderForm, SecretAction, SelectAction, TextAction,
    WorkerForm,
};
use pages::chat::{ChatCommand, ChatState, Pick, Picker};
use pages::Page;
use sdk::Client;
use std::io::{self, IsTerminal};
use std::process::ExitCode;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

/// Whether this process can drive a full-screen session.
pub fn supported() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Run one full-screen chat until the user leaves.
pub async fn run(
    profile: Profile,
    client: Client,
    workers: Vec<WorkerSummary>,
    worker: String,
    agent: String,
) -> ExitCode {
    let state = App::new(&profile.name, &profile.url, workers, worker, agent);
    run_app(state, client).await
}

/// Run one full-screen management session until the user leaves.
pub async fn manage(
    profile: String,
    gateway: String,
    client: Client,
    workers: Vec<WorkerSummary>,
    worker: String,
    agent: String,
) -> ExitCode {
    let mut state = App::new(&profile, &gateway, workers, worker, agent);
    state.open_manage(Page::Workers, None);
    run_app(state, client).await
}

#[allow(unused_assignments)]
async fn run_app(mut state: App, client: Client) -> ExitCode {
    let (sender, mut events) = mpsc::unbounded_channel();
    let mut inputs = event::spawn_input();
    let mut task: Option<JoinHandle<()>> = None;
    let terminal = match terminal::Terminal::enter() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("gb-client: cannot start full-screen session: {error}");
            return ExitCode::from(1);
        }
    };
    if state.screen == Screen::Manage {
        load_page(&state, &client, &sender);
    }
    let mut ticker = tokio::time::interval(Duration::from_millis(80));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let mut code = ExitCode::SUCCESS;
    while !state.quit {
        if dirty && draw(&state).is_err() {
            code = ExitCode::from(1);
            break;
        }
        dirty = false;
        tokio::select! {
            event = inputs.recv() => {
                let Some(event) = event else { break };
                dirty = true;
                match event {
                    InputEvent::Key(key) => {
                        handle_key(&mut state, &client, key, &sender, &mut task);
                    }
                    InputEvent::Paste(text) => {
                        if state.screen == Screen::Chat && state.dialog.is_none() {
                            state.chat.reset_palette();
                            state.chat.input.insert_text(&text);
                            state.chat.sync_palette();
                        }
                    }
                    InputEvent::Resize => {}
                }
            }
            Some(event) = events.recv() => {
                dirty = true;
                state.update(event);
                if state.screen == Screen::Manage && state.dialog.is_none() {
                    // Keep the page selection valid after list refreshes.
                    state.models.clamp(&state.providers.providers);
                }
            }
            _ = ticker.tick(), if state.screen == Screen::Manage || state.chat.busy => {
                state.spinner = state.spinner.wrapping_add(1);
                if state.chat.busy {
                    state.chat.spinner = state.chat.spinner.wrapping_add(1);
                }
                dirty = true;
            }
        }
    }
    if let Some(task) = task.take() {
        task.abort();
    }
    drop(terminal);
    code
}

fn draw(state: &App) -> io::Result<()> {
    let (width, height) = terminal::size();
    let frame = match state.screen {
        Screen::Chat => frame::chat::render(&state.chat, width, height),
        Screen::Manage => frame::management::render(state, width, height),
    };
    terminal::draw(&frame)
}

fn handle_key(
    state: &mut App,
    client: &Client,
    key: KeyEvent,
    sender: &UnboundedSender<UiEvent>,
    task: &mut Option<JoinHandle<()>>,
) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        state.quit = true;
        return;
    }
    if let Some(dialog) = state.dialog.as_mut() {
        let outcome = dialog.handle(key);
        match outcome {
            DialogOutcome::Continue => {}
            DialogOutcome::Cancel => state.dialog = None,
            DialogOutcome::Submit => {
                if let Some(dialog) = state.dialog.take() {
                    state.apply_dialog(dialog, client, sender);
                }
            }
        }
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p') {
        if state.screen == Screen::Chat {
            open_manage(state, Page::Workers, None, client, sender);
        } else {
            state.open_chat();
        }
        return;
    }
    if state.screen == Screen::Manage {
        handle_manage_key(state, client, key, sender);
    } else {
        handle_chat_key(state, client, key, sender, task);
    }
}

fn handle_chat_key(
    state: &mut App,
    client: &Client,
    key: KeyEvent,
    sender: &UnboundedSender<UiEvent>,
    task: &mut Option<JoinHandle<()>>,
) {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('l') => {
                let _ = execute!(io::stdout(), Clear(ClearType::All));
                return;
            }
            KeyCode::Char('u') => {
                state.chat.reset_palette();
                state.chat.input.kill_to_start();
                state.chat.sync_palette();
                return;
            }
            KeyCode::Char('w') => {
                state.chat.reset_palette();
                state.chat.input.kill_word();
                state.chat.sync_palette();
                return;
            }
            KeyCode::Char('t') => {
                state.chat.toggle_thinking();
                return;
            }
            KeyCode::Char('o') => {
                state.chat.toggle_tool_detail();
                return;
            }
            _ => return,
        }
    }
    if state.chat.picker.is_some() {
        match key.code {
            KeyCode::Up => {
                if let Some(picker) = state.chat.picker.as_mut() {
                    picker.up();
                }
            }
            KeyCode::Down => {
                if let Some(picker) = state.chat.picker.as_mut() {
                    picker.down();
                }
            }
            KeyCode::Enter => state.chat.confirm_picker(),
            KeyCode::Esc => state.chat.picker = None,
            _ => {}
        }
        return;
    }
    if state.chat.palette.is_some() {
        match key.code {
            KeyCode::Esc => {
                state.chat.reset_palette();
                return;
            }
            KeyCode::Up => {
                state.chat.palette_up();
                return;
            }
            KeyCode::Down => {
                state.chat.palette_down();
                return;
            }
            KeyCode::Tab => {
                state.chat.complete_palette();
                return;
            }
            KeyCode::Enter => {
                if let Some(command) = state.chat.selected_command() {
                    state.chat.input.set_text(command.name);
                    state.chat.reset_palette();
                    submit_chat(state, client, sender, task);
                    return;
                }
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Esc => state.quit = true,
        KeyCode::Enter => submit_chat(state, client, sender, task),
        KeyCode::Char(character) => {
            state.chat.reset_palette();
            state.chat.input.insert(character);
            state.chat.sync_palette();
        }
        KeyCode::Backspace => {
            state.chat.reset_palette();
            state.chat.input.backspace();
            state.chat.sync_palette();
        }
        KeyCode::Delete => {
            state.chat.reset_palette();
            state.chat.input.delete();
            state.chat.sync_palette();
        }
        KeyCode::Left => state.chat.input.left(),
        KeyCode::Right => state.chat.input.right(),
        KeyCode::Home => {
            if state.chat.input.is_empty() {
                state.chat.scroll_to_start();
            } else {
                state.chat.input.home();
            }
        }
        KeyCode::End => {
            if state.chat.input.is_empty() {
                let (width, height) = terminal::size();
                let maximum = frame::chat::max_scroll(&state.chat, width, height);
                state.chat.scroll_to_end(maximum);
            } else {
                state.chat.input.end();
            }
        }
        KeyCode::Up => {
            if state.chat.input.previous() {
                state.chat.sync_palette();
            } else {
                scroll(&mut state.chat, -1);
            }
        }
        KeyCode::Down => {
            if state.chat.input.next() {
                state.chat.sync_palette();
            } else {
                scroll(&mut state.chat, 1);
            }
        }
        KeyCode::PageUp => scroll(&mut state.chat, -(page_rows() as isize)),
        KeyCode::PageDown => scroll(&mut state.chat, page_rows() as isize),
        _ => {}
    }
}

fn submit_chat(
    state: &mut App,
    client: &Client,
    sender: &UnboundedSender<UiEvent>,
    task: &mut Option<JoinHandle<()>>,
) {
    state.chat.reset_palette();
    let input = state.chat.input.text();
    let trimmed = input.trim();
    if trimmed.is_empty() {
        state.chat.input.clear();
        return;
    }
    state.chat.input.remember();
    if let Some(command) = pages::chat::command(trimmed) {
        run_chat_command(state, command, client, sender);
        return;
    }
    if state.chat.busy {
        state
            .chat
            .complain("a task is already running; wait for it or press Esc to leave");
        return;
    }
    let request = state.chat.begin(trimmed.to_string());
    *task = Some(spawn_task(client.clone(), request, sender.clone()));
}

fn run_chat_command(
    state: &mut App,
    command: ChatCommand,
    client: &Client,
    sender: &UnboundedSender<UiEvent>,
) {
    match command {
        ChatCommand::Help => state.chat.notice(pages::chat::HELP.to_string()),
        ChatCommand::Gateway => {
            state
                .chat
                .notice(format!("{} · {}", state.profile, state.gateway));
        }
        ChatCommand::Thinking(flag) => {
            state.chat.show_thinking = flag.unwrap_or(!state.chat.show_thinking);
            state.chat.follow = true;
        }
        ChatCommand::Tools(flag) => {
            state.chat.show_tool_detail = flag.unwrap_or(!state.chat.show_tool_detail);
            state.chat.follow = true;
        }
        ChatCommand::New => state.chat.new_session(),
        ChatCommand::Exit => state.quit = true,
        ChatCommand::Workers => open_manage(state, Page::Workers, None, client, sender),
        ChatCommand::Providers => open_manage(state, Page::Providers, None, client, sender),
        ChatCommand::Models => open_manage(state, Page::Models, None, client, sender),
        ChatCommand::Manage => {
            let page = state.page;
            open_manage(state, page, None, client, sender);
        }
        ChatCommand::ProviderAdd => open_manage(
            state,
            Page::Providers,
            Some(Dialog::ProviderForm(ProviderForm::add())),
            client,
            sender,
        ),
        ChatCommand::WorkerAdd => open_manage(
            state,
            Page::Workers,
            Some(Dialog::WorkerForm(WorkerForm::add())),
            client,
            sender,
        ),
        ChatCommand::Worker(Some(id)) => {
            if state.chat.workers.iter().any(|worker| worker.id == id) {
                state.chat.switch_worker(&id);
            } else {
                state
                    .chat
                    .complain(format!("Worker '{id}' is not reachable; type /workers"));
            }
        }
        ChatCommand::Worker(None) => {
            if state.chat.workers.is_empty() {
                state.chat.complain("no reachable Worker is registered");
            } else {
                let items = state
                    .chat
                    .workers
                    .iter()
                    .map(|worker| worker.id.clone())
                    .collect();
                state.chat.picker = Some(Picker::new(Pick::Worker, state.profile.clone(), items));
            }
        }
        ChatCommand::Agents => {
            let listing = state.chat.agent_listing();
            state.chat.notice(listing);
        }
        ChatCommand::Agent(Some(name)) => state.chat.switch_agent(&name),
        ChatCommand::Agent(None) => {
            let items = session::capabilities(&state.chat.workers, &state.chat.worker);
            if items.is_empty() {
                state.chat.complain(format!(
                    "Worker '{}' advertises no agents",
                    state.chat.worker
                ));
            } else {
                state.chat.picker =
                    Some(Picker::new(Pick::Agent, state.chat.worker.clone(), items));
            }
        }
        ChatCommand::Unknown(name) => {
            state
                .chat
                .complain(format!("unknown command '{name}'; type /help"));
        }
    }
}

fn handle_manage_key(
    state: &mut App,
    client: &Client,
    key: KeyEvent,
    sender: &UnboundedSender<UiEvent>,
) {
    if key.code == KeyCode::Tab {
        state.page = state.page.next();
        load_page(state, client, sender);
        return;
    }
    if key.code == KeyCode::BackTab {
        state.page = state.page.previous();
        load_page(state, client, sender);
        return;
    }
    if key.code == KeyCode::Esc {
        match state.page {
            Page::Models => {
                state.page = Page::Providers;
                load_page(state, client, sender);
            }
            _ => state.open_chat(),
        }
        return;
    }
    match state.page {
        Page::Workers => workers_key(state, client, key, sender),
        Page::Providers => providers_key(state, client, key, sender),
        Page::Models => models_key(state, client, key, sender),
    }
}

fn workers_key(state: &mut App, client: &Client, key: KeyEvent, sender: &UnboundedSender<UiEvent>) {
    if state.workers.filtering {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => state.workers.stop_filter(),
            KeyCode::Backspace => state.workers.filter_pop(),
            KeyCode::Up => state.workers.up(),
            KeyCode::Down => state.workers.down(),
            KeyCode::Char(character) => state.workers.filter_push(character),
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => state.workers.up(),
        KeyCode::Down | KeyCode::Char('j') => state.workers.down(),
        KeyCode::Enter => {
            if let Some(worker) = state.workers.selected_worker() {
                state.dialog = Some(Dialog::Text {
                    title: format!("Worker {}", worker.id),
                    buffer: text_input(&worker_detail(worker)),
                    action: TextAction::Dismiss,
                });
            }
        }
        KeyCode::Char('/') => state.workers.start_filter(),
        KeyCode::Char('r') => {
            state.status = "refreshing workers…".to_string();
            spawn(client.clone(), Action::LoadWorkers, sender.clone());
        }
        KeyCode::Char('a') => {
            state.dialog = Some(Dialog::WorkerForm(WorkerForm::add()));
        }
        KeyCode::Char('e') => {
            if let Some(worker) = state.workers.selected_worker() {
                state.dialog = Some(Dialog::WorkerForm(WorkerForm::edit(worker)));
            } else {
                state.status = "select a Worker to edit".to_string();
            }
        }
        KeyCode::Char('p') => {
            if let Some(worker) = state.workers.selected_worker() {
                let id = worker.id.clone();
                state.status = format!("probing worker {id}…");
                spawn(client.clone(), Action::WorkerProbe { id }, sender.clone());
            }
        }
        KeyCode::Char('d') => {
            if let Some(worker) = state.workers.selected_worker() {
                let id = worker.id.clone();
                state.status = format!("disabling worker {id}…");
                spawn(client.clone(), Action::WorkerDisable { id }, sender.clone());
            }
        }
        KeyCode::Char('E') => {
            if let Some(worker) = state.workers.selected_worker() {
                let id = worker.id.clone();
                state.status = format!("enabling worker {id}…");
                spawn(client.clone(), Action::WorkerEnable { id }, sender.clone());
            }
        }
        KeyCode::Char('x') => {
            if let Some(worker) = state.workers.selected_worker() {
                state.dialog = Some(Dialog::Confirm {
                    title: "Remove Worker".into(),
                    body: format!(
                        "Remove '{}'? Task history is retained; the id becomes terminal.",
                        worker.id
                    ),
                    action: ConfirmAction::WorkerRemove(worker.id.clone()),
                });
            }
        }
        KeyCode::Char('l') => {
            let worker = state
                .workers
                .selected_worker()
                .map(|worker| worker.id.clone());
            state.status = "loading audit…".to_string();
            spawn(
                client.clone(),
                Action::WorkerAudit { worker },
                sender.clone(),
            );
        }
        _ => {}
    }
}

fn providers_key(
    state: &mut App,
    client: &Client,
    key: KeyEvent,
    sender: &UnboundedSender<UiEvent>,
) {
    if state.providers.filtering {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => state.providers.stop_filter(),
            KeyCode::Backspace => state.providers.filter_pop(),
            KeyCode::Up => state.providers.up(),
            KeyCode::Down => state.providers.down(),
            KeyCode::Char(character) => state.providers.filter_push(character),
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => state.providers.up(),
        KeyCode::Down | KeyCode::Char('j') => state.providers.down(),
        KeyCode::Enter => {
            if let Some(provider) = state.providers.selected_provider() {
                state.dialog = Some(Dialog::Text {
                    title: format!("Provider {}", provider.id),
                    buffer: text_input(&provider_detail(provider)),
                    action: TextAction::Dismiss,
                });
            }
        }
        KeyCode::Char('/') => state.providers.start_filter(),
        KeyCode::Char('r') => {
            state.status = "refreshing providers…".to_string();
            spawn(client.clone(), Action::LoadProviders, sender.clone());
        }
        KeyCode::Char('a') => {
            state.dialog = Some(Dialog::ProviderForm(ProviderForm::add()));
        }
        KeyCode::Char('e') => {
            if let Some(provider) = state.providers.selected_provider() {
                state.dialog = Some(Dialog::ProviderForm(ProviderForm::edit(provider)));
            } else {
                state.status = "select a provider to edit".to_string();
            }
        }
        KeyCode::Char('s') => {
            if let Some(provider) = state.providers.selected_provider() {
                state.dialog = Some(Dialog::Secret {
                    title: format!("Set credential for {}", provider.id),
                    buffer: input::Input::default(),
                    action: SecretAction::ProviderCredential(provider.id.clone()),
                });
            }
        }
        KeyCode::Char('R') => {
            if let Some(provider) = state.providers.selected_provider() {
                state.dialog = Some(Dialog::Confirm {
                    title: "Revoke credential".into(),
                    body: format!("Revoke the active credential for '{}'?", provider.id),
                    action: ConfirmAction::ProviderRevoke(provider.id.clone()),
                });
            }
        }
        KeyCode::Char('d') => {
            if let Some(provider) = state.providers.selected_provider() {
                let id = provider.id.clone();
                state.status = format!("discovering {}…", id);
                spawn(
                    client.clone(),
                    Action::ProviderDiscover { id },
                    sender.clone(),
                );
            }
        }
        KeyCode::Char('x') => {
            if let Some(provider) = state.providers.selected_provider() {
                let mut options = crate::commands::provider::ProviderUpdateOptions {
                    revision: provider.revision,
                    active: Some(!provider.active),
                    ..Default::default()
                };
                options.active = Some(!provider.active);
                let id = provider.id.clone();
                state.status = format!("toggling {id}…");
                spawn(
                    client.clone(),
                    Action::ProviderUpdate { id, options },
                    sender.clone(),
                );
            }
        }
        KeyCode::Char('w') => {
            if let Some(provider) = state.providers.selected_provider() {
                state.dialog = Some(Dialog::Text {
                    title: format!("Workers for {}", provider.id),
                    buffer: text_input(&provider.allowed_workers.join(",")),
                    action: TextAction::ProviderWorkers(provider.id.clone()),
                });
            }
        }
        KeyCode::Char('m') => {
            if let Some(provider) = state.providers.selected_provider() {
                let id = provider.id.clone();
                state.models.provider = Some(id.clone());
                state.models.selected = 0;
                state.page = Page::Models;
                state.status = format!("models for {id}");
                spawn(
                    client.clone(),
                    Action::LoadModels { provider: id },
                    sender.clone(),
                );
            }
        }
        KeyCode::Char('p') => {
            let items = state
                .workers
                .workers
                .iter()
                .map(|worker| worker.id.clone())
                .collect::<Vec<_>>();
            if items.is_empty() {
                state.status = "workers are not loaded yet; open Workers and press r".to_string();
                spawn(client.clone(), Action::LoadWorkers, sender.clone());
            } else {
                state.dialog = Some(Dialog::Select {
                    title: "Pair Worker".into(),
                    items,
                    selected: 0,
                    action: SelectAction::PairWorker,
                });
            }
        }
        _ => {}
    }
}

fn models_key(state: &mut App, client: &Client, key: KeyEvent, sender: &UnboundedSender<UiEvent>) {
    if state.models.filtering {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => state.models.stop_filter(),
            KeyCode::Backspace => state.models.filter_pop(),
            KeyCode::Up => state.models.up(),
            KeyCode::Down => {
                let length = state.models.rows(&state.providers.providers).len();
                state.models.down(length);
            }
            KeyCode::Char(character) => state.models.filter_push(character),
            _ => {}
        }
        return;
    }
    let length = state.models.rows(&state.providers.providers).len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => state.models.up(),
        KeyCode::Down | KeyCode::Char('j') => state.models.down(length),
        KeyCode::Char('/') => state.models.start_filter(),
        KeyCode::Esc => {
            state.page = Page::Providers;
            load_page(state, client, sender);
        }
        KeyCode::Char(' ') => {
            let Some(row) = state.models.selected_row(&state.providers.providers) else {
                return;
            };
            let Some(provider) = state.providers.provider(&row.provider) else {
                return;
            };
            let mut allowed = provider
                .allowed_models
                .clone()
                .unwrap_or_else(|| provider.models.clone());
            if row.allowed {
                allowed.retain(|model| model != &row.model);
            } else if !allowed.contains(&row.model) {
                allowed.push(row.model.clone());
            }
            state.status = format!("updating allow-list for {}…", provider.id);
            spawn(
                client.clone(),
                Action::ModelUpdate {
                    id: provider.id.clone(),
                    revision: provider.revision,
                    allowed_models: Some(allowed),
                    model: None,
                },
                sender.clone(),
            );
        }
        KeyCode::Enter => {
            let Some(row) = state.models.selected_row(&state.providers.providers) else {
                return;
            };
            let Some(provider) = state.providers.provider(&row.provider) else {
                return;
            };
            state.status = format!("setting preferred model {}…", row.model);
            spawn(
                client.clone(),
                Action::ModelUpdate {
                    id: provider.id.clone(),
                    revision: provider.revision,
                    allowed_models: None,
                    model: Some(row.model.clone()),
                },
                sender.clone(),
            );
        }
        KeyCode::Char('c') => {
            let provider = state
                .models
                .selected_row(&state.providers.providers)
                .map(|row| row.provider)
                .or_else(|| state.models.provider.clone());
            let Some(id) = provider else {
                state.status = "select a model first".to_string();
                return;
            };
            let Some(provider) = state.providers.provider(&id) else {
                return;
            };
            state.status = format!("clearing allow-list for {id}…");
            spawn(
                client.clone(),
                Action::ModelUpdate {
                    id: id.clone(),
                    revision: provider.revision,
                    allowed_models: Some(Vec::new()),
                    model: None,
                },
                sender.clone(),
            );
        }
        KeyCode::Char('r') => {
            let provider = state
                .models
                .selected_row(&state.providers.providers)
                .map(|row| row.provider)
                .or_else(|| state.models.provider.clone());
            if let Some(id) = provider {
                state.status = format!("rediscovering {id}…");
                spawn(
                    client.clone(),
                    Action::ProviderDiscover { id },
                    sender.clone(),
                );
            } else {
                state.status = "select a model first".to_string();
            }
        }
        _ => {}
    }
}

fn open_manage(
    state: &mut App,
    page: Page,
    dialog: Option<Dialog>,
    client: &Client,
    sender: &UnboundedSender<UiEvent>,
) {
    state.open_manage(page, dialog);
    load_page(state, client, sender);
}

fn load_page(state: &App, client: &Client, sender: &UnboundedSender<UiEvent>) {
    match state.page {
        Page::Workers => spawn(client.clone(), Action::LoadWorkers, sender.clone()),
        Page::Providers => {
            spawn(client.clone(), Action::LoadProviders, sender.clone());
            spawn(client.clone(), Action::LoadWorkers, sender.clone());
        }
        Page::Models => {
            spawn(client.clone(), Action::LoadProviders, sender.clone());
            if let Some(provider) = &state.models.provider {
                spawn(
                    client.clone(),
                    Action::LoadModels {
                        provider: provider.clone(),
                    },
                    sender.clone(),
                );
            }
        }
    }
}

fn text_input(text: &str) -> input::Input {
    let mut input = input::Input::default();
    input.set_text(text);
    input
}

fn worker_detail(worker: &common::AdminWorkerSummary) -> String {
    format!(
        "id: {}\nendpoint: {}\nstatus: {}\nreachable: {}\nenabled: {}\nrevision: {}\nsource: {}\ninstance: {}\npackage: {}\nlocality: {}\ncontrol-token-set: {}\ncapabilities: {}",
        worker.id,
        worker.endpoint,
        worker.status,
        worker.reachable,
        worker.enabled,
        worker.revision,
        worker.source,
        worker.instance,
        worker.package,
        worker.locality,
        worker.control_token_set,
        worker
            .capabilities
            .clone()
            .unwrap_or_else(|| worker.approved_capabilities.clone())
            .join(",")
    )
}

fn provider_detail(provider: &common::ProviderMetadata) -> String {
    format!(
        "id: {}\nadapter: {}\nmodel: {}\nendpoint: {}\nready: {}\nactive: {}\nrevision: {}\ncredential: {}\nworkers: {}\nmodels: {}\nallow-list: {}",
        provider.id,
        provider.adapter,
        empty(&provider.model),
        provider.endpoint.as_deref().unwrap_or("-"),
        provider.ready,
        provider.active,
        provider.revision,
        provider
            .credential_version
            .map_or("-".to_string(), |version| format!("v{version}")),
        provider.allowed_workers.join(","),
        provider.models.join(","),
        provider
            .allowed_models
            .as_ref()
            .map_or("all".to_string(), |models| models.join(","))
    )
}

fn empty(value: &str) -> &str {
    if value.is_empty() {
        "-"
    } else {
        value
    }
}

fn scroll(state: &mut ChatState, delta: isize) {
    let (width, height) = terminal::size();
    let maximum = frame::chat::max_scroll(state, width, height);
    state.scroll_by(delta, maximum);
}

fn page_rows() -> usize {
    let (_, height) = terminal::size();
    frame::chat::conversation_height(height).max(1)
}

fn spawn_task(
    client: Client,
    request: GatewaySubmitRequest,
    sender: UnboundedSender<UiEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let response = match client.submit(&request).await {
            Ok(response) => response,
            Err(error) => {
                let _ = sender.send(UiEvent::ChatFailed(error));
                return;
            }
        };
        let mut printed = String::new();
        let mut thinking_seen = false;
        let mut cursor = 0;
        for _ in 0..2 {
            let Ok(mut watch) = client.watch_from(&response.task_id, cursor).await else {
                break;
            };
            let mut terminal = false;
            loop {
                match watch.next().await {
                    Ok(Some(event)) => {
                        cursor = watch.last_durable_seq().max(event.seq);
                        match event.kind.as_str() {
                            "model.delta" => {
                                if let Some(text) =
                                    event.payload.get("text").and_then(|text| text.as_str())
                                {
                                    printed.push_str(text);
                                    if sender.send(UiEvent::ChatDelta(text.to_string())).is_err() {
                                        return;
                                    }
                                }
                            }
                            "model.delta.complete" if printed.is_empty() => {
                                if let Some(text) =
                                    event.payload.get("text").and_then(|text| text.as_str())
                                {
                                    printed.push_str(text);
                                    if sender.send(UiEvent::ChatDelta(text.to_string())).is_err() {
                                        return;
                                    }
                                }
                            }
                            "model.thinking" | "model.thinking.complete" if !thinking_seen => {
                                if let Some(text) =
                                    event.payload.get("text").and_then(|text| text.as_str())
                                {
                                    thinking_seen = true;
                                    if sender
                                        .send(UiEvent::ChatThinking(text.to_string()))
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                            "tool.call" => {
                                let Some(name) = event
                                    .payload
                                    .get("name")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_string)
                                else {
                                    continue;
                                };
                                let id = event
                                    .payload
                                    .get("id")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_string)
                                    .unwrap_or_else(|| name.clone());
                                let server = event
                                    .payload
                                    .get("server")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_string);
                                let agent = event
                                    .payload
                                    .get("agent")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_string);
                                if sender
                                    .send(UiEvent::ChatToolCall {
                                        id,
                                        name,
                                        server,
                                        agent,
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            "tool.result" => {
                                let Some(name) = event
                                    .payload
                                    .get("name")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_string)
                                else {
                                    continue;
                                };
                                let id = event
                                    .payload
                                    .get("id")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_string)
                                    .unwrap_or_else(|| name.clone());
                                let ok = event
                                    .payload
                                    .get("ok")
                                    .and_then(|value| value.as_bool())
                                    .unwrap_or(false);
                                let error_code = event
                                    .payload
                                    .get("error_code")
                                    .and_then(|value| value.as_str())
                                    .filter(|value| !value.is_empty())
                                    .map(str::to_string);
                                let duration_ms = event
                                    .payload
                                    .get("duration_ms")
                                    .and_then(|value| value.as_u64());
                                if sender
                                    .send(UiEvent::ChatToolResult {
                                        id,
                                        name,
                                        ok,
                                        error_code,
                                        duration_ms,
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            "task.succeeded" | "task.failed" | "task.cancelled"
                            | "task.unknown" => {
                                terminal = true;
                                break;
                            }
                            _ => {}
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            cursor = watch.last_durable_seq().max(cursor);
            if terminal {
                break;
            }
        }
        match session::wait_terminal(&client, &response.task_id).await {
            Ok(task) => {
                let _ = sender.send(UiEvent::ChatFinished {
                    printed,
                    output: task.output.unwrap_or_default(),
                    error: task.error,
                });
            }
            Err(error) => {
                let _ = sender.send(UiEvent::ChatFailed(error));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::ApiError;

    #[test]
    fn terminal_support_is_a_boolean_check() {
        let _ = supported();
    }

    #[test]
    fn worker_details_include_the_inventory_fields() {
        let worker = common::AdminWorkerSummary {
            id: "worker-a".into(),
            endpoint: "http://127.0.0.1:9090".into(),
            instance: "one".into(),
            package: "echo".into(),
            capabilities: Some(vec!["general".into()]),
            approved_capabilities: Vec::new(),
            declared_capabilities: Vec::new(),
            enabled: true,
            revision: 2,
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
        };
        let text = worker_detail(&worker);
        assert!(text.contains("worker-a"));
        assert!(text.contains("revision: 2"));
    }

    #[test]
    fn api_errors_are_available_to_the_shell() {
        let error = ApiError::new("cli.timeout", "timed out");
        assert_eq!(error.code, "cli.timeout");
    }
}
