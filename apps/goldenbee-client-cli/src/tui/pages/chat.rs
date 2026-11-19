//! Full-screen chat state and slash-command vocabulary.
//!
//! The state owns everything the frame renders: the selected target, the
//! conversation, the input buffer, scroll position, and transient command
//! handlers such as the Worker and agent pickers.

use crate::session;
use crate::tui::input::Input;
use common::{ApiError, GatewaySubmitRequest, Limits, WorkerSummary};

/// Who produced one conversation entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// Submitted chat text.
    User,
    /// Worker or assistant text.
    Assistant,
    /// Model reasoning trace.
    Thinking,
    /// One tool call and its result.
    Tool,
    /// Local status from the CLI.
    Notice,
    /// Recoverable failure.
    Error,
}

/// Lifecycle of one tool call.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ToolState {
    /// Dispatch was observed; no result yet.
    Pending,
    /// Tool completed successfully.
    Ok,
    /// Tool completed with an error.
    Failed,
}

/// One tool call paired with its result.
#[derive(Clone, Debug)]
pub struct ToolInfo {
    /// Model-assigned call identity.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Tool server or namespace, when present.
    pub server: Option<String>,
    /// Agent that requested the call.
    pub agent: Option<String>,
    /// Current lifecycle state.
    pub state: ToolState,
    /// Error code or result detail, never credentials.
    pub detail: String,
    /// Call duration in milliseconds when reported.
    pub duration_ms: Option<u64>,
}

/// One conversation entry.
pub struct Message {
    /// Who produced the entry.
    pub role: Role,
    /// Raw text, wrapped when rendered.
    pub text: String,
    /// Whether more text may still arrive.
    pub streaming: bool,
    /// Tool metadata when this is a tool entry.
    pub tool: Option<ToolInfo>,
}

impl Message {
    fn new(role: Role, text: String) -> Self {
        Self {
            role,
            text,
            streaming: false,
            tool: None,
        }
    }

    fn streaming_assistant() -> Self {
        Self {
            role: Role::Assistant,
            text: String::new(),
            streaming: true,
            tool: None,
        }
    }

    fn streaming_thinking() -> Self {
        Self {
            role: Role::Thinking,
            text: String::new(),
            streaming: true,
            tool: None,
        }
    }

    fn tool_call(id: String, name: String, server: Option<String>, agent: Option<String>) -> Self {
        Self {
            role: Role::Tool,
            text: String::new(),
            streaming: false,
            tool: Some(ToolInfo {
                id,
                name,
                server,
                agent,
                state: ToolState::Pending,
                detail: String::new(),
                duration_ms: None,
            }),
        }
    }

    fn tool_result(
        id: String,
        name: String,
        ok: bool,
        error_code: Option<String>,
        duration_ms: Option<u64>,
    ) -> Self {
        Self {
            role: Role::Tool,
            text: String::new(),
            streaming: false,
            tool: Some(ToolInfo {
                id,
                name,
                server: None,
                agent: None,
                state: if ok { ToolState::Ok } else { ToolState::Failed },
                detail: error_code.unwrap_or_default(),
                duration_ms,
            }),
        }
    }
}

/// One slash command offered by the input popover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    /// Command name including the leading slash.
    pub name: &'static str,
    /// One-line hint shown beside the name.
    pub description: &'static str,
}

/// Commands the popover suggests, in display order.
pub const SLASH_COMMANDS: [SlashCommand; 14] = [
    SlashCommand {
        name: "/worker",
        description: "choose a Worker",
    },
    SlashCommand {
        name: "/workers",
        description: "open the Workers page",
    },
    SlashCommand {
        name: "/agent",
        description: "choose an agent",
    },
    SlashCommand {
        name: "/agents",
        description: "list agents on the current Worker",
    },
    SlashCommand {
        name: "/provider",
        description: "open Providers or add one",
    },
    SlashCommand {
        name: "/providers",
        description: "open the Providers page",
    },
    SlashCommand {
        name: "/models",
        description: "open the Models page",
    },
    SlashCommand {
        name: "/manage",
        description: "open management",
    },
    SlashCommand {
        name: "/gateway",
        description: "show the active Gateway",
    },
    SlashCommand {
        name: "/thinking",
        description: "show or hide model reasoning",
    },
    SlashCommand {
        name: "/tools",
        description: "show compact or detailed tool calls",
    },
    SlashCommand {
        name: "/new",
        description: "start a new session",
    },
    SlashCommand {
        name: "/help",
        description: "show commands and keys",
    },
    SlashCommand {
        name: "/exit",
        description: "leave the chat",
    },
];

/// Highlight state for the slash-command popover.
pub struct Palette {
    /// Highlighted command in the current filter.
    pub selected: usize,
}

/// Command filter for one input line.
///
/// Only an unfinished command token opens the popover: the line must start
/// with `/` and contain no whitespace yet.
pub fn palette_query(input: &str) -> Option<&str> {
    if !input.starts_with('/') || input.chars().any(char::is_whitespace) {
        return None;
    }
    Some(input)
}

/// Commands whose names start with `query`.
pub fn matching_commands(query: &str) -> Vec<&'static SlashCommand> {
    SLASH_COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(query))
        .collect()
}

/// Which list a picker is choosing from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pick {
    /// One reachable Worker.
    Worker,
    /// One agent capability on the current Worker.
    Agent,
}

/// A small selection modal over the conversation pane.
pub struct Picker {
    /// What the selection changes.
    pub kind: Pick,
    /// Modal heading.
    pub title: String,
    /// Selectable identifiers.
    pub items: Vec<String>,
    /// Highlighted item.
    pub selected: usize,
}

impl Picker {
    /// Build a picker with the first item highlighted.
    pub fn new(kind: Pick, title: impl Into<String>, items: Vec<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            items,
            selected: 0,
        }
    }

    /// Move the highlight up.
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the highlight down.
    pub fn down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
        }
    }

    /// Highlighted identifier.
    pub fn selected_item(&self) -> Option<&str> {
        self.items.get(self.selected).map(String::as_str)
    }
}

/// Everything one interactive chat session owns.
pub struct ChatState {
    /// Active Gateway profile name.
    pub profile: String,
    /// Active Gateway URL.
    pub gateway: String,
    /// Last known reachable Workers.
    pub workers: Vec<WorkerSummary>,
    /// Selected Worker.
    pub worker: String,
    /// Selected agent capability.
    pub agent: String,
    /// Conversation session identifier.
    pub session: String,
    /// Conversation entries, oldest first.
    pub messages: Vec<Message>,
    /// Current input line.
    pub input: Input,
    /// First visible conversation line when not following new text.
    pub scroll: usize,
    /// Whether the viewport tracks new text.
    pub follow: bool,
    /// Whether a submitted task is still running.
    pub busy: bool,
    /// Animation frame for the streaming indicator.
    pub spinner: usize,
    /// Open selection modal.
    pub picker: Option<Picker>,
    /// Open slash-command popover and its highlighted row.
    pub palette: Option<Palette>,
    /// Show expanded model reasoning traces.
    pub show_thinking: bool,
    /// Show expanded tool-call metadata.
    pub show_tool_detail: bool,
    submitted: u64,
}

impl ChatState {
    /// Start a chat on one resolved Worker and agent.
    pub fn new(
        profile: &str,
        gateway: &str,
        workers: Vec<WorkerSummary>,
        worker: String,
        agent: String,
    ) -> Self {
        let mut state = Self {
            profile: profile.to_string(),
            gateway: gateway.to_string(),
            workers,
            worker,
            agent,
            session: session::session_id(),
            messages: Vec::new(),
            input: Input::default(),
            scroll: 0,
            follow: true,
            busy: false,
            spinner: 0,
            picker: None,
            palette: None,
            show_thinking: false,
            show_tool_detail: false,
            submitted: 0,
        };
        state.notice(format!(
            "connected to {} · {}",
            state.profile, state.gateway
        ));
        state.notice("type /help for commands, /exit to leave".to_string());
        state
    }

    /// Append a local status entry.
    pub fn notice(&mut self, text: String) {
        self.messages.push(Message::new(Role::Notice, text));
        self.follow = true;
    }

    /// Append a recoverable failure with its recovery guidance.
    pub fn fail(&mut self, error: &ApiError) {
        self.messages
            .push(Message::new(Role::Error, session::error_text(error)));
        self.follow = true;
    }

    /// Append local, non-Gateway failure text.
    pub fn complain(&mut self, text: impl Into<String>) {
        self.messages.push(Message::new(Role::Error, text.into()));
        self.follow = true;
    }

    /// Start a submitted task and return its Gateway request.
    pub fn begin(&mut self, input: String) -> GatewaySubmitRequest {
        self.messages.push(Message::new(Role::User, input.clone()));
        self.messages.push(Message::streaming_assistant());
        self.submitted += 1;
        self.busy = true;
        self.follow = true;
        GatewaySubmitRequest {
            worker: Some(self.worker.clone()),
            agent: self.agent.clone(),
            session_id: self.session.clone(),
            input,
            idempotency_key: format!("gb-client-{}-{}", std::process::id(), self.submitted),
            profile: None,
            team: None,
            team_version: None,
            limits: Limits::default(),
        }
    }

    /// Append live model text to the streaming assistant entry.
    ///
    /// Text after a tool call starts a new assistant block, so the transcript
    /// keeps the model's answer and the tool work in causal order.
    pub fn delta(&mut self, text: &str) {
        self.close_thinking();
        if !matches!(
            self.messages.last(),
            Some(Message {
                role: Role::Assistant,
                streaming: true,
                ..
            })
        ) {
            self.messages.push(Message::streaming_assistant());
        }
        if let Some(message) = self.messages.last_mut() {
            message.text.push_str(text);
        }
        // Streaming text must not pull the viewport down after the user
        // scrolled up; only the final result resumes following.
    }

    /// Append live reasoning text to the streaming thinking entry.
    pub fn thinking_delta(&mut self, text: &str) {
        self.close_assistant();
        self.drop_empty_assistant();
        if !matches!(
            self.messages.last(),
            Some(Message {
                role: Role::Thinking,
                streaming: true,
                ..
            })
        ) {
            self.messages.push(Message::streaming_thinking());
        }
        if let Some(message) = self.messages.last_mut() {
            message.text.push_str(text);
        }
    }

    /// Add or update one tool call before its result arrives.
    pub fn tool_call(
        &mut self,
        id: String,
        name: String,
        server: Option<String>,
        agent: Option<String>,
    ) {
        self.close_streaming();
        self.drop_empty_assistant();
        if self.messages.iter().any(|message| {
            message
                .tool
                .as_ref()
                .is_some_and(|tool| tool.id == id && tool.name == name)
        }) {
            return;
        }
        self.messages
            .push(Message::tool_call(id, name, server, agent));
        self.follow = true;
    }

    /// Pair one tool result with its call, or show an unpaired result.
    pub fn tool_result(
        &mut self,
        id: String,
        name: String,
        ok: bool,
        error_code: Option<String>,
        duration_ms: Option<u64>,
    ) {
        self.close_streaming();
        let state = if ok { ToolState::Ok } else { ToolState::Failed };
        if let Some(tool) = self.messages.iter_mut().find_map(|message| {
            message
                .tool
                .as_mut()
                .filter(|tool| tool.id == id && tool.name == name)
        }) {
            tool.state = state;
            tool.detail = error_code.unwrap_or_default();
            tool.duration_ms = duration_ms;
        } else {
            self.messages
                .push(Message::tool_result(id, name, ok, error_code, duration_ms));
        }
        self.follow = true;
    }

    /// Reconcile the live stream with the authoritative task projection.
    ///
    /// This mirrors the line-oriented fallback: text already shown is not
    /// repeated, a missing suffix is appended, and divergent output is added
    /// on its own line because it describes a different result.
    pub fn finish(&mut self, printed: &str, output: &str, error: Option<ApiError>) {
        self.close_streaming();
        self.fail_pending_tools("task finished before the tool returned a result");
        let remaining = if printed.is_empty() {
            output
        } else {
            output.strip_prefix(printed).unwrap_or_default()
        };
        let last_is_tool = self
            .messages
            .last()
            .is_some_and(|message| message.role == Role::Tool);
        if (!self
            .messages
            .iter()
            .any(|message| message.role == Role::Assistant)
            && (!printed.is_empty() || !output.is_empty()))
            || (last_is_tool && !remaining.is_empty())
        {
            self.messages
                .push(Message::new(Role::Assistant, String::new()));
        }
        if let Some(message) = self
            .messages
            .iter_mut()
            .rev()
            .find(|message| message.role == Role::Assistant)
        {
            if printed.is_empty() {
                message.text.push_str(output);
            } else if let Some(rest) = output.strip_prefix(printed) {
                message.text.push_str(rest);
            } else if !output.is_empty() {
                message.text.push('\n');
                message.text.push_str(output);
            }
        }
        self.busy = false;
        self.follow = true;
        if self.messages.last().is_some_and(empty_assistant) {
            self.messages.pop();
        }
        if let Some(error) = error {
            self.fail(&error);
        }
    }

    /// Clear the running state after a submission or stream failure.
    pub fn abort(&mut self, error: &ApiError) {
        self.close_streaming();
        self.fail_pending_tools("task ended before the tool returned a result");
        self.busy = false;
        self.follow = true;
        if self.messages.last().is_some_and(empty_assistant) {
            self.messages.pop();
        }
        self.fail(error);
    }

    /// Toggle reasoning visibility.
    pub fn toggle_thinking(&mut self) {
        self.show_thinking = !self.show_thinking;
        self.follow = true;
    }

    /// Toggle tool-call detail visibility.
    pub fn toggle_tool_detail(&mut self) {
        self.show_tool_detail = !self.show_tool_detail;
        self.follow = true;
    }

    /// Close any open streaming assistant block.
    fn close_assistant(&mut self) {
        if let Some(Message {
            role: Role::Assistant,
            streaming,
            ..
        }) = self.messages.last_mut()
        {
            *streaming = false;
        }
    }

    /// Close any open streaming reasoning block.
    fn close_thinking(&mut self) {
        if let Some(Message {
            role: Role::Thinking,
            streaming,
            ..
        }) = self.messages.last_mut()
        {
            *streaming = false;
        }
    }

    fn close_streaming(&mut self) {
        self.close_assistant();
        self.close_thinking();
    }

    fn drop_empty_assistant(&mut self) {
        if self.messages.last().is_some_and(empty_assistant) {
            self.messages.pop();
        }
    }

    fn fail_pending_tools(&mut self, detail: &str) {
        for message in &mut self.messages {
            if let Some(tool) = &mut message.tool {
                if tool.state == ToolState::Pending {
                    tool.state = ToolState::Failed;
                    tool.detail = detail.to_string();
                }
            }
        }
    }

    /// Start a new conversation session identifier.
    pub fn new_session(&mut self) {
        self.session = session::session_id();
        self.notice(format!("new session {}", self.session));
    }

    /// Switch Worker, preserving the conversation session.
    pub fn switch_worker(&mut self, worker: &str) {
        match session::first_agent(&self.workers, worker, None) {
            Ok(agent) => {
                self.worker = worker.to_string();
                self.agent = agent;
                self.notice(format!(
                    "worker {} · agent {} · session {}",
                    self.worker, self.agent, self.session
                ));
            }
            Err(error) => self.fail(&error),
        }
    }

    /// Switch agent on the current Worker.
    pub fn switch_agent(&mut self, agent: &str) {
        let agents = session::capabilities(&self.workers, &self.worker);
        if agents.iter().any(|id| id == agent) {
            self.agent = agent.to_string();
            self.notice(format!("agent {}", self.agent));
        } else {
            self.complain(format!(
                "Worker '{}' has no agent '{}'; type /agents",
                self.worker, agent
            ));
        }
    }

    /// Apply the highlighted picker item.
    pub fn confirm_picker(&mut self) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        let Some(item) = picker.selected_item().map(str::to_string) else {
            return;
        };
        match picker.kind {
            Pick::Worker => self.switch_worker(&item),
            Pick::Agent => self.switch_agent(&item),
        }
    }

    /// Commands matching the unfinished input line, if the popover applies.
    pub fn palette_matches(&self) -> Vec<&'static SlashCommand> {
        if self.picker.is_some() {
            return Vec::new();
        }
        palette_query(&self.input.text())
            .map(matching_commands)
            .unwrap_or_default()
    }

    /// Recompute the popover after the input text changed.
    pub fn sync_palette(&mut self) {
        let length = self.palette_matches().len();
        if length == 0 {
            self.palette = None;
            return;
        }
        let selected = self
            .palette
            .as_ref()
            .map_or(0, |palette| palette.selected.min(length - 1));
        self.palette = Some(Palette { selected });
    }

    /// Close the popover without touching the input line.
    pub fn reset_palette(&mut self) {
        self.palette = None;
    }

    /// Highlighted command in the current filter.
    pub fn selected_command(&self) -> Option<&'static SlashCommand> {
        let palette = self.palette.as_ref()?;
        self.palette_matches().get(palette.selected).copied()
    }

    /// Move the popover highlight up.
    pub fn palette_up(&mut self) {
        let length = self.palette_matches().len();
        if length == 0 {
            self.palette = None;
            return;
        }
        let selected = self
            .palette
            .as_ref()
            .map_or(0, |palette| palette.selected.saturating_sub(1));
        self.palette = Some(Palette { selected });
    }

    /// Move the popover highlight down.
    pub fn palette_down(&mut self) {
        let length = self.palette_matches().len();
        if length == 0 {
            self.palette = None;
            return;
        }
        let selected = self
            .palette
            .as_ref()
            .map_or(0, |palette| (palette.selected + 1).min(length - 1));
        self.palette = Some(Palette { selected });
    }

    /// Put the highlighted command in the input line and close the popover.
    pub fn complete_palette(&mut self) -> bool {
        let Some(command) = self.selected_command() else {
            return false;
        };
        self.input.set_text(&format!("{} ", command.name));
        self.palette = None;
        true
    }

    /// Listing for `/agents`.
    pub fn agent_listing(&self) -> String {
        let agents = session::capabilities(&self.workers, &self.worker);
        if agents.is_empty() {
            return format!("Worker '{}' advertises no agents", self.worker);
        }
        agents
            .iter()
            .map(|agent| {
                if agent == &self.agent {
                    format!("{agent} active")
                } else {
                    agent.clone()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Status label for the context pane.
    pub fn status_label(&self) -> String {
        let mut label = if self.busy {
            const SPINNER: [char; 8] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
            format!("running {}", SPINNER[self.spinner % SPINNER.len()])
        } else {
            "idle".to_string()
        };
        if self.show_thinking {
            label.push_str(" · thinking");
        }
        if self.show_tool_detail {
            label.push_str(" · tools");
        }
        label
    }

    /// Move the conversation viewport by `delta` lines.
    pub fn scroll_by(&mut self, delta: isize, maximum: usize) {
        let maximum = maximum as isize;
        let current = self.scroll.min(maximum.max(0) as usize) as isize;
        let next = (current + delta).clamp(0, maximum.max(0));
        self.scroll = next as usize;
        self.follow = self.scroll >= maximum.max(0) as usize;
    }

    /// Jump to the oldest visible line.
    pub fn scroll_to_start(&mut self) {
        self.scroll = 0;
        self.follow = false;
    }

    /// Jump back to the newest line and resume following.
    pub fn scroll_to_end(&mut self, maximum: usize) {
        self.scroll = maximum;
        self.follow = true;
    }
}

fn empty_assistant(message: &Message) -> bool {
    message.role == Role::Assistant && !message.streaming && message.text.is_empty()
}

/// A slash command entered in chat.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ChatCommand {
    /// Show command help.
    Help,
    /// List, switch, or intentionally add a Worker.
    Worker(Option<String>),
    /// Open the Workers management page.
    Workers,
    /// List agents on the current Worker.
    Agents,
    /// List or switch agent.
    Agent(Option<String>),
    /// Open the Providers management page.
    Providers,
    /// Open the Models management page.
    Models,
    /// Open management on the current page.
    Manage,
    /// Open management with the provider form.
    ProviderAdd,
    /// Open management with the Worker form.
    WorkerAdd,
    /// Show the active Gateway.
    Gateway,
    /// Show or hide model reasoning traces.
    Thinking(Option<bool>),
    /// Show compact or detailed tool calls.
    Tools(Option<bool>),
    /// Start a new session identifier.
    New,
    /// Leave the chat.
    Exit,
    /// Unknown slash command.
    Unknown(String),
}

/// Parse one slash command. Plain chat text is not a command.
pub fn command(input: &str) -> Option<ChatCommand> {
    let mut parts = input.split_whitespace();
    let head = parts.next()?;
    if !head.starts_with('/') {
        return None;
    }
    let arguments = parts.collect::<Vec<_>>();
    let first = arguments.first().copied();
    let thinking_flag = match first {
        Some("on" | "show" | "expand") => Some(true),
        Some("off" | "hide" | "collapse") => Some(false),
        _ => None,
    };
    let tools_flag = match first {
        Some("detail" | "details" | "expand" | "on") => Some(true),
        Some("compact" | "collapse" | "off") => Some(false),
        _ => None,
    };
    Some(match head {
        "/help" => ChatCommand::Help,
        "/worker" if first == Some("add") => ChatCommand::WorkerAdd,
        "/worker" => ChatCommand::Worker(first.map(str::to_string)),
        "/provider" if first == Some("add") => ChatCommand::ProviderAdd,
        "/provider" | "/providers" => ChatCommand::Providers,
        "/workers" => ChatCommand::Workers,
        "/models" | "/model" => ChatCommand::Models,
        "/manage" => ChatCommand::Manage,
        "/agents" => ChatCommand::Agents,
        "/agent" => ChatCommand::Agent(first.map(str::to_string)),
        "/gateway" => ChatCommand::Gateway,
        "/thinking" => ChatCommand::Thinking(thinking_flag),
        "/tools" => ChatCommand::Tools(tools_flag),
        "/new" => ChatCommand::New,
        "/exit" | "/quit" => ChatCommand::Exit,
        other => ChatCommand::Unknown(other.to_string()),
    })
}

/// Help text shown by `/help`.
pub const HELP: &str = "\
/worker [id]    list or switch the Worker
/worker add     open the Worker form
/workers        open the Workers management page
/providers      open the Providers management page
/provider add   open the provider form
/models         open the Models management page
/manage         open management
/agents         list agents on the current Worker
/agent [name]   list or switch the agent
/gateway        show the active Gateway
/thinking [on|off]  show or hide model reasoning
/tools [detail|compact]  show tool-call details
/new            start a new session id
/exit           leave the chat

Up/Down         recall history when input is empty, otherwise scroll
PgUp/PgDn       scroll one page
Home/End        scroll to start or end
Ctrl+U/Ctrl+W   delete to start or previous word
Ctrl+T          toggle model reasoning
Ctrl+O          toggle tool-call details
Ctrl+P          toggle management
Ctrl+L          redraw
Esc/Ctrl+C      leave";

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ChatState {
        let mut state = ChatState::new(
            "local",
            "http://127.0.0.1:8088",
            Vec::new(),
            "w".into(),
            "a".into(),
        );
        state.session = "session-1".to_string();
        state.messages.clear();
        state
    }

    #[test]
    fn commands_parse_with_optional_arguments() {
        assert_eq!(command("/help"), Some(ChatCommand::Help));
        assert_eq!(
            command("/worker echo"),
            Some(ChatCommand::Worker(Some("echo".to_string())))
        );
        assert_eq!(command("/worker"), Some(ChatCommand::Worker(None)));
        assert_eq!(command("/workers"), Some(ChatCommand::Workers));
        assert_eq!(command("/provider add"), Some(ChatCommand::ProviderAdd));
        assert_eq!(command("/providers"), Some(ChatCommand::Providers));
        assert_eq!(command("/models"), Some(ChatCommand::Models));
        assert_eq!(command("/manage"), Some(ChatCommand::Manage));
        assert_eq!(
            command("/thinking on"),
            Some(ChatCommand::Thinking(Some(true)))
        );
        assert_eq!(
            command("/thinking off"),
            Some(ChatCommand::Thinking(Some(false)))
        );
        assert_eq!(command("/thinking"), Some(ChatCommand::Thinking(None)));
        assert_eq!(
            command("/tools detail"),
            Some(ChatCommand::Tools(Some(true)))
        );
        assert_eq!(
            command("/tools compact"),
            Some(ChatCommand::Tools(Some(false)))
        );
        assert_eq!(command("/worker add"), Some(ChatCommand::WorkerAdd));
        assert_eq!(command("/agents"), Some(ChatCommand::Agents));
        assert_eq!(command("/agents now"), Some(ChatCommand::Agents));
        assert_eq!(command("/quit"), Some(ChatCommand::Exit));
        assert_eq!(
            command("/nope"),
            Some(ChatCommand::Unknown("/nope".to_string()))
        );
        assert_eq!(command("hello"), None);
        assert_eq!(command(""), None);
    }

    #[test]
    fn palette_opens_only_for_an_unfinished_command_token() {
        assert_eq!(palette_query("/"), Some("/"));
        assert_eq!(palette_query("/wor"), Some("/wor"));
        assert_eq!(palette_query("/worker "), None);
        assert_eq!(palette_query("/worker echo"), None);
        assert_eq!(palette_query("hello"), None);
        assert_eq!(palette_query(" /help"), None);
    }

    #[test]
    fn palette_filters_and_moves_the_highlight() {
        let mut state = state();
        state.input.set_text("/wor");
        state.sync_palette();
        let names = state
            .palette_matches()
            .iter()
            .map(|command| command.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["/worker", "/workers"]);
        assert_eq!(state.selected_command().unwrap().name, "/worker");

        state.palette_down();
        assert_eq!(state.selected_command().unwrap().name, "/workers");
        state.palette_down();
        assert_eq!(state.selected_command().unwrap().name, "/workers");
        state.palette_up();
        assert_eq!(state.selected_command().unwrap().name, "/worker");

        state.input.set_text("/help");
        state.sync_palette();
        assert_eq!(state.selected_command().unwrap().name, "/help");
    }

    #[test]
    fn palette_typing_updates_matches_and_no_match_closes_it() {
        let mut state = state();
        state.input.set_text("/");
        state.sync_palette();
        assert_eq!(state.palette_matches().len(), SLASH_COMMANDS.len());
        assert!(state.palette.is_some());

        state.input.set_text("/zzz");
        state.sync_palette();
        assert!(state.palette.is_none());
        assert!(state.selected_command().is_none());
    }

    #[test]
    fn palette_completion_extracts_the_highlighted_command() {
        let mut state = state();
        state.input.set_text("/wor");
        state.sync_palette();
        state.palette_down();
        let selected = *state.selected_command().unwrap();
        assert_eq!(selected.name, "/workers");
        assert_eq!(command(selected.name), Some(ChatCommand::Workers));

        assert!(state.complete_palette());
        assert_eq!(state.input.text(), "/workers ");
        assert!(state.palette.is_none());
        assert_eq!(
            command(state.input.text().trim()),
            Some(ChatCommand::Workers)
        );
    }

    #[test]
    fn final_output_does_not_repeat_streamed_text() {
        let mut state = state();
        state.begin("hello".to_string());
        state.delta("hel");
        state.finish("hel", "hello", None);
        assert!(!state.busy);
        let message = state.messages.last().unwrap();
        assert_eq!(message.text, "hello");
        assert!(!message.streaming);
    }

    #[test]
    fn missing_stream_text_comes_from_final_output() {
        let mut state = state();
        state.begin("hello".to_string());
        state.finish("", "complete answer", None);
        assert_eq!(state.messages.last().unwrap().text, "complete answer");
    }

    #[test]
    fn divergent_output_is_appended_once() {
        let mut state = state();
        state.begin("hello".to_string());
        state.delta("partial");
        state.finish("partial", "authoritative", None);
        assert_eq!(
            state.messages.last().unwrap().text,
            "partial\nauthoritative"
        );
    }

    #[test]
    fn streaming_deltas_keep_a_scrolled_up_viewport() {
        let mut state = state();
        state.begin("hello".to_string());
        state.input.clear();
        state.scroll = 0;
        state.follow = false;
        state.delta("par");
        assert!(!state.follow);
        assert_eq!(state.messages.last().unwrap().text, "par");
        state.finish("par", "partial", None);
        assert!(state.follow);
        assert_eq!(state.messages.last().unwrap().text, "partial");
    }

    #[test]
    fn empty_results_leave_no_blank_assistant_entry() {
        let mut state = state();
        state.begin("hello".to_string());
        state.finish("", "", None);
        assert!(state
            .messages
            .iter()
            .all(|message| message.role != Role::Assistant));
        assert!(!state.busy);
    }

    #[test]
    fn thinking_then_answer_are_separate_blocks() {
        let mut state = state();
        state.begin("inspect".to_string());
        state.thinking_delta("Need the file list.");
        state.delta("I will inspect it.");
        state.finish("I will inspect it.", "I will inspect it.", None);

        assert_eq!(state.messages[1].role, Role::Thinking);
        assert_eq!(state.messages[1].text, "Need the file list.");
        assert!(!state.messages[1].streaming);
        assert_eq!(state.messages[2].role, Role::Assistant);
        assert_eq!(state.messages[2].text, "I will inspect it.");
    }

    #[test]
    fn tool_call_pairs_with_result_and_reopens_the_answer_block() {
        let mut state = state();
        state.begin("run tests".to_string());
        state.delta("Checking the tree.");
        state.tool_call(
            "call-1".to_string(),
            "exec_command".to_string(),
            Some("local".to_string()),
            Some("general".to_string()),
        );
        state.tool_result(
            "call-1".to_string(),
            "exec_command".to_string(),
            true,
            None,
            Some(42),
        );
        state.delta("Tests pass.");

        let tool = state.messages[2].tool.as_ref().expect("tool entry");
        assert_eq!(tool.state, ToolState::Ok);
        assert_eq!(tool.duration_ms, Some(42));
        assert_eq!(state.messages[3].role, Role::Assistant);
        assert_eq!(state.messages[3].text, "Tests pass.");
    }

    #[test]
    fn pending_tools_fail_when_the_task_ends() {
        let mut state = state();
        state.begin("run".to_string());
        state.tool_call("call-1".to_string(), "exec_command".to_string(), None, None);
        state.finish("", "", None);
        let tool = state.messages[1].tool.as_ref().expect("tool entry");
        assert_eq!(tool.state, ToolState::Failed);
        assert!(tool.detail.contains("finished"));
    }

    #[test]
    fn view_modes_are_toggleable_and_visible_in_status() {
        let mut state = state();
        assert_eq!(state.status_label(), "idle");
        state.toggle_thinking();
        state.toggle_tool_detail();
        assert!(state.status_label().contains("thinking"));
        assert!(state.status_label().contains("tools"));
    }

    #[test]
    fn new_session_changes_the_id_and_worker_switches_keep_it() {
        let mut state = state();
        state.new_session();
        let session = state.session.clone();
        assert!(session.starts_with("cli-"));
        assert_eq!(state.messages.last().unwrap().role, Role::Notice);
        let workers = vec![worker("w", &["general"]), worker("x", &["echo"])];
        state.workers = workers;
        state.switch_worker("x");
        assert_eq!(state.worker, "x");
        assert_eq!(state.agent, "echo");
        assert_eq!(state.session, session);
    }

    #[test]
    fn scrolling_follows_only_at_the_bottom() {
        let mut state = state();
        state.scroll_by(-1, 5);
        assert_eq!(state.scroll, 0);
        assert!(!state.follow);
        state.scroll_by(2, 5);
        assert_eq!(state.scroll, 2);
        assert!(!state.follow);
        state.scroll_by(10, 5);
        assert_eq!(state.scroll, 5);
        assert!(state.follow);
        state.scroll_to_start();
        assert_eq!(state.scroll, 0);
        assert!(!state.follow);
        state.scroll_to_end(5);
        assert!(state.follow);
    }

    fn worker(id: &str, agents: &[&str]) -> WorkerSummary {
        WorkerSummary {
            id: id.to_string(),
            endpoint: format!("http://{id}"),
            instance: id.to_string(),
            package: "general".to_string(),
            version: "0.1.0".to_string(),
            capabilities: agents
                .iter()
                .map(|id| common::CapabilityDescriptor {
                    id: id.to_string(),
                    kind: "agent".to_string(),
                    description: String::new(),
                    permissions: Vec::new(),
                })
                .collect(),
            declared_capabilities: Vec::new(),
            approved_capabilities: Vec::new(),
            protocol: String::new(),
            incarnation: "1".to_string(),
            incarnations: Vec::new(),
            capacity: common::Capacity {
                active: 0,
                limit: 1,
                queued: 0,
                queue_limit: 1,
            },
            lease_expires_at: 0,
            renewed_at: 0,
            locality: "local".to_string(),
            status: "healthy".to_string(),
            reachable: true,
        }
    }
}
