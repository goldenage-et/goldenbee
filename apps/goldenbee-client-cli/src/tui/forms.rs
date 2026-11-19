//! Typed dialogs used by management pages.
//!
//! Dialogs cover confirmations, small text, secrets, and selection. Lists and
//! editing live on dedicated pages instead of hidden modal flows; a form uses
//! one dialog because it edits a single record before returning to the list.

use crate::tui::input::Input;
use common::{AdminWorkerSummary, ProviderMetadata};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Confirmed destructive action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    /// Remove one Worker identity.
    WorkerRemove(String),
    /// Revoke one provider credential.
    ProviderRevoke(String),
}

/// Secret value destination. The buffer is never rendered or logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretAction {
    /// Write a provider credential.
    ProviderCredential(String),
}

/// Small text destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextAction {
    /// Edit one provider's Worker assignments.
    ProviderWorkers(String),
    /// Enter the instance for a provider pairing code.
    PairInstance {
        /// Selected Worker.
        worker: String,
    },
    /// Read-only notice or audit text.
    Dismiss,
}

/// Selection destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectAction {
    /// Choose the Worker to pair with a provider.
    PairWorker,
}

/// One open dialog.
pub enum Dialog {
    /// Destructive confirmation.
    Confirm {
        /// Heading.
        title: String,
        /// Body text.
        body: String,
        /// Action applied on confirmation.
        action: ConfirmAction,
    },
    /// Create or edit a provider.
    ProviderForm(ProviderForm),
    /// Create or edit an approved Worker.
    WorkerForm(WorkerForm),
    /// Masked secret input.
    Secret {
        /// Heading.
        title: String,
        /// Masked input buffer.
        buffer: Input,
        /// Credential destination.
        action: SecretAction,
    },
    /// Small text input or read-only notice.
    Text {
        /// Heading.
        title: String,
        /// Input or notice text.
        buffer: Input,
        /// Text destination.
        action: TextAction,
    },
    /// Small selection list.
    Select {
        /// Heading.
        title: String,
        /// Items.
        items: Vec<String>,
        /// Highlighted item.
        selected: usize,
        /// Selection destination.
        action: SelectAction,
    },
}

/// Result of one key press in a dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogOutcome {
    /// Keep editing.
    Continue,
    /// Close without applying.
    Cancel,
    /// Apply and close.
    Submit,
}

impl Dialog {
    /// Dialog heading.
    pub fn title(&self) -> &str {
        match self {
            Dialog::Confirm { title, .. }
            | Dialog::Secret { title, .. }
            | Dialog::Text { title, .. }
            | Dialog::Select { title, .. } => title,
            Dialog::ProviderForm(form) => {
                if form.editing {
                    "Edit provider"
                } else {
                    "Add provider"
                }
            }
            Dialog::WorkerForm(form) => {
                if form.editing {
                    "Edit Worker"
                } else {
                    "Add Worker"
                }
            }
        }
    }

    /// Route one key press.
    pub fn handle(&mut self, key: KeyEvent) -> DialogOutcome {
        if key.code == KeyCode::Esc {
            return DialogOutcome::Cancel;
        }
        match self {
            Dialog::Confirm { .. } => match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => DialogOutcome::Submit,
                KeyCode::Char('n') | KeyCode::Char('N') => DialogOutcome::Cancel,
                _ => DialogOutcome::Continue,
            },
            Dialog::Secret { buffer, .. } | Dialog::Text { buffer, .. } => {
                edit_line(buffer, key);
                if key.code == KeyCode::Enter {
                    DialogOutcome::Submit
                } else {
                    DialogOutcome::Continue
                }
            }
            Dialog::ProviderForm(form) => form.handle(key),
            Dialog::WorkerForm(form) => form.handle(key),
            Dialog::Select {
                items, selected, ..
            } => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        *selected = selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if *selected + 1 < items.len() {
                            *selected += 1;
                        }
                    }
                    KeyCode::Enter => return DialogOutcome::Submit,
                    _ => {}
                }
                DialogOutcome::Continue
            }
        }
    }
}

/// Provider create/edit form.
pub struct ProviderForm {
    /// Provider id.
    pub id: Input,
    /// Native adapter kind.
    pub adapter: Input,
    /// Preferred model.
    pub model: Input,
    /// Endpoint URL.
    pub endpoint: Input,
    /// Worker assignments.
    pub workers: Input,
    /// Focused field.
    pub focus: usize,
    /// Whether this form updates an existing provider.
    pub editing: bool,
    /// Observed provider revision when editing.
    pub revision: u64,
    /// Original provider id when editing.
    pub provider: String,
}

impl ProviderForm {
    /// Empty form for a new provider.
    pub fn add() -> Self {
        let mut adapter = Input::default();
        adapter.set_text("deterministic");
        let mut workers = Input::default();
        workers.set_text("*");
        Self {
            id: Input::default(),
            adapter,
            model: Input::default(),
            endpoint: Input::default(),
            workers,
            focus: 0,
            editing: false,
            revision: 0,
            provider: String::new(),
        }
    }

    /// Form prefilled from one provider.
    pub fn edit(provider: &ProviderMetadata) -> Self {
        let mut id = Input::default();
        id.set_text(&provider.id);
        let mut adapter = Input::default();
        adapter.set_text(&provider.adapter);
        let mut model = Input::default();
        model.set_text(&provider.model);
        let mut endpoint = Input::default();
        endpoint.set_text(provider.endpoint.as_deref().unwrap_or(""));
        let mut workers = Input::default();
        workers.set_text(&provider.allowed_workers.join(","));
        Self {
            id,
            adapter,
            model,
            endpoint,
            workers,
            focus: 0,
            editing: true,
            revision: provider.revision,
            provider: provider.id.clone(),
        }
    }

    fn handle(&mut self, key: KeyEvent) -> DialogOutcome {
        if key.code == KeyCode::Esc {
            return DialogOutcome::Cancel;
        }
        if key.code == KeyCode::Enter {
            return DialogOutcome::Submit;
        }
        if key.code == KeyCode::BackTab
            || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
        {
            self.focus = (self.focus + 4) % 5;
            return DialogOutcome::Continue;
        }
        if key.code == KeyCode::Tab || key.code == KeyCode::Down {
            self.focus = (self.focus + 1) % 5;
            return DialogOutcome::Continue;
        }
        if key.code == KeyCode::Up {
            self.focus = (self.focus + 4) % 5;
            return DialogOutcome::Continue;
        }
        let buffer = match self.focus {
            0 => &mut self.id,
            1 => &mut self.adapter,
            2 => &mut self.model,
            3 => &mut self.endpoint,
            _ => &mut self.workers,
        };
        edit_line(buffer, key);
        DialogOutcome::Continue
    }

    /// Field labels and values in display order. The id field is read-only when
    /// editing because renaming a provider would create a different identity.
    pub fn fields(&self) -> [(&'static str, String, bool); 5] {
        [
            ("id", self.id.text(), self.editing),
            ("adapter", self.adapter.text(), false),
            ("model", self.model.text(), false),
            ("endpoint", self.endpoint.text(), false),
            ("workers", self.workers.text(), false),
        ]
    }
}

/// Worker create/edit form.
pub struct WorkerForm {
    /// Worker id.
    pub id: Input,
    /// Endpoint URL.
    pub endpoint: Input,
    /// Expected instance.
    pub instance: Input,
    /// Expected package.
    pub package: Input,
    /// Approved capabilities.
    pub capabilities: Input,
    /// Focused field.
    pub focus: usize,
    /// Whether this form updates an existing Worker.
    pub editing: bool,
    /// Observed revision when editing.
    pub revision: u64,
    /// Original Worker id when editing.
    pub worker: String,
}

impl WorkerForm {
    /// Empty form for a new Worker.
    pub fn add() -> Self {
        let mut capabilities = Input::default();
        capabilities.set_text("general");
        Self {
            id: Input::default(),
            endpoint: Input::default(),
            instance: Input::default(),
            package: Input::default(),
            capabilities,
            focus: 0,
            editing: false,
            revision: 0,
            worker: String::new(),
        }
    }

    /// Form prefilled from one Worker.
    pub fn edit(worker: &AdminWorkerSummary) -> Self {
        let mut id = Input::default();
        id.set_text(&worker.id);
        let mut endpoint = Input::default();
        endpoint.set_text(&worker.endpoint);
        let mut instance = Input::default();
        instance.set_text(&worker.instance);
        let mut package = Input::default();
        package.set_text(&worker.package);
        let mut capabilities = Input::default();
        let capabilities_text = worker
            .capabilities
            .clone()
            .unwrap_or_else(|| worker.approved_capabilities.clone())
            .join(",");
        capabilities.set_text(&capabilities_text);
        Self {
            id,
            endpoint,
            instance,
            package,
            capabilities,
            focus: 0,
            editing: true,
            revision: worker.revision,
            worker: worker.id.clone(),
        }
    }

    fn handle(&mut self, key: KeyEvent) -> DialogOutcome {
        if key.code == KeyCode::Esc {
            return DialogOutcome::Cancel;
        }
        if key.code == KeyCode::Enter {
            return DialogOutcome::Submit;
        }
        if key.code == KeyCode::BackTab
            || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
        {
            self.focus = (self.focus + 4) % 5;
            return DialogOutcome::Continue;
        }
        if key.code == KeyCode::Tab || key.code == KeyCode::Down {
            self.focus = (self.focus + 1) % 5;
            return DialogOutcome::Continue;
        }
        if key.code == KeyCode::Up {
            self.focus = (self.focus + 4) % 5;
            return DialogOutcome::Continue;
        }
        let buffer = match self.focus {
            0 => &mut self.id,
            1 => &mut self.endpoint,
            2 => &mut self.instance,
            3 => &mut self.package,
            _ => &mut self.capabilities,
        };
        edit_line(buffer, key);
        DialogOutcome::Continue
    }

    /// Field labels and values in display order.
    pub fn fields(&self) -> [(&'static str, String, bool); 5] {
        [
            ("id", self.id.text(), self.editing),
            ("endpoint", self.endpoint.text(), false),
            ("instance", self.instance.text(), false),
            ("package", self.package.text(), false),
            ("capabilities", self.capabilities.text(), false),
        ]
    }
}

fn edit_line(buffer: &mut Input, key: KeyEvent) {
    match key.code {
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            buffer.insert(character);
        }
        KeyCode::Backspace => buffer.backspace(),
        KeyCode::Delete => buffer.delete(),
        KeyCode::Left => buffer.left(),
        KeyCode::Right => buffer.right(),
        KeyCode::Home => buffer.home(),
        KeyCode::End => buffer.end(),
        _ => {}
    }
}

/// Split a Worker assignment string. `*` assigns every approved Worker.
pub fn worker_list(raw: &str) -> Vec<String> {
    raw.split(|character: char| character == ',' || character.is_whitespace())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Split a capability string.
pub fn capability_list(raw: &str) -> Vec<String> {
    worker_list(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn provider_form_tabs_through_fields_and_submits() {
        let mut form = ProviderForm::add();
        assert_eq!(form.fields()[1].1, "deterministic");
        assert_eq!(form.handle(key(KeyCode::Tab)), DialogOutcome::Continue);
        assert_eq!(form.focus, 1);
        assert_eq!(form.handle(key(KeyCode::Enter)), DialogOutcome::Submit);
        assert_eq!(form.handle(key(KeyCode::Esc)), DialogOutcome::Cancel);
    }

    #[test]
    fn worker_assignments_accept_commas_and_star() {
        assert_eq!(worker_list("a,b c"), ["a", "b", "c"]);
        assert_eq!(worker_list("*"), ["*"]);
    }

    #[test]
    fn secret_editing_never_exposes_the_buffer_in_its_title() {
        let mut dialog = Dialog::Secret {
            title: "Set credential".into(),
            buffer: Input::default(),
            action: SecretAction::ProviderCredential("local".into()),
        };
        dialog.handle(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(dialog.title(), "Set credential");
        assert!(!dialog.title().contains('k'));
        assert_eq!(
            dialog.handle(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            DialogOutcome::Continue
        );
        assert_eq!(
            dialog.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            DialogOutcome::Submit
        );
    }
}
