//! Dialog rendering.

use crate::tui::forms::Dialog;
use crate::tui::frame::components::{clip, fill, Row, Tone};

/// Render one dialog into the management content area.
pub fn render(dialog: &Dialog, width: usize, area: usize) -> Vec<Row> {
    let mut lines: Vec<(Tone, String)> = Vec::new();
    match dialog {
        Dialog::Confirm { body, .. } => {
            for line in body.lines() {
                lines.push((Tone::Assistant, line.to_string()));
            }
            lines.push((Tone::Dim, String::new()));
            lines.push((Tone::Dim, "enter/y confirm · esc cancel".to_string()));
        }
        Dialog::ProviderForm(form) => {
            for (index, (label, value, read_only)) in form.fields().iter().enumerate() {
                let marker = if index == form.focus { ">" } else { " " };
                let tone = if index == form.focus {
                    Tone::User
                } else {
                    Tone::Assistant
                };
                let suffix = if *read_only { "  (read-only)" } else { "" };
                lines.push((tone, format!("{marker} {label:<10} {value}{suffix}")));
            }
            lines.push((Tone::Dim, String::new()));
            lines.push((Tone::Dim, "tab next · enter save · esc cancel".to_string()));
        }
        Dialog::WorkerForm(form) => {
            for (index, (label, value, read_only)) in form.fields().iter().enumerate() {
                let marker = if index == form.focus { ">" } else { " " };
                let tone = if index == form.focus {
                    Tone::User
                } else {
                    Tone::Assistant
                };
                let suffix = if *read_only { "  (read-only)" } else { "" };
                lines.push((tone, format!("{marker} {label:<12} {value}{suffix}")));
            }
            lines.push((Tone::Dim, String::new()));
            lines.push((Tone::Dim, "tab next · enter save · esc cancel".to_string()));
        }
        Dialog::Secret { buffer, .. } => {
            lines.push((Tone::Dim, "credential value is never shown".to_string()));
            lines.push((Tone::User, format!("> {}", mask(buffer.masked(), width))));
            lines.push((Tone::Dim, String::new()));
            lines.push((Tone::Dim, "enter save · esc cancel".to_string()));
        }
        Dialog::Text { buffer, action, .. } => {
            let read_only = matches!(action, crate::tui::forms::TextAction::Dismiss);
            for line in buffer.text().lines() {
                lines.push((Tone::Assistant, line.to_string()));
            }
            if buffer.is_empty() {
                lines.push((Tone::Dim, String::new()));
            }
            lines.push((Tone::Dim, String::new()));
            lines.push((
                Tone::Dim,
                if read_only {
                    "enter close".to_string()
                } else {
                    "enter apply · esc cancel".to_string()
                },
            ));
        }
        Dialog::Select {
            items, selected, ..
        } => {
            if items.is_empty() {
                lines.push((Tone::Dim, "no items; press esc".to_string()));
            }
            let capacity = area.saturating_sub(8);
            let start = selected.saturating_sub(capacity.saturating_sub(1));
            for (index, item) in items.iter().enumerate().skip(start).take(capacity) {
                let marker = if index == *selected { "> " } else { "  " };
                let tone = if index == *selected {
                    Tone::User
                } else {
                    Tone::Assistant
                };
                lines.push((tone, format!("{marker}{item}")));
            }
            lines.push((Tone::Dim, String::new()));
            lines.push((Tone::Dim, "enter select · esc cancel".to_string()));
        }
    }
    boxed(dialog.title(), lines, width, area)
}

fn mask(masked: String, width: usize) -> String {
    clip(&masked, width.saturating_sub(4))
}

fn boxed(title: &str, lines: Vec<(Tone, String)>, width: usize, area: usize) -> Vec<Row> {
    let inner = width.saturating_sub(4);
    let label = format!("─ {title} ");
    let top = format!(
        "╭{label}{}╮",
        "─".repeat(inner.saturating_sub(label.len() + 1))
    );
    let mut rows = Vec::with_capacity(area);
    rows.push(fill(vec![(Tone::Border, top)], width));
    for (tone, text) in lines {
        let text = clip(&text, inner);
        let padding = inner.saturating_sub(text.len());
        rows.push(fill(
            vec![
                (Tone::Border, "│ ".to_string()),
                (tone, text),
                (Tone::Dim, " ".repeat(padding)),
                (Tone::Border, " │".to_string()),
            ],
            width,
        ));
    }
    rows.push(fill(
        vec![(
            Tone::Border,
            format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
        )],
        width,
    ));
    if rows.len() > area {
        rows.truncate(area);
        return rows;
    }
    let top_pad = area.saturating_sub(rows.len()) / 2;
    let bottom_pad = area.saturating_sub(rows.len()) - top_pad;
    let mut padded = Vec::with_capacity(area);
    padded.extend(std::iter::repeat_with(Row::default).take(top_pad));
    padded.extend(rows);
    padded.extend(std::iter::repeat_with(Row::default).take(bottom_pad));
    padded
}

/// A tiny frame wrapper for dialog-only renders used by tests.
#[cfg(test)]
pub fn frame(dialog: &Dialog, width: usize, area: usize) -> crate::tui::frame::components::Frame {
    crate::tui::frame::components::Frame {
        rows: render(dialog, width, area),
        cursor: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::forms::{ProviderForm, SecretAction};
    use crate::tui::input::Input;

    #[test]
    fn secret_dialogs_never_render_the_value() {
        let mut buffer = Input::default();
        buffer.set_text("super-secret-key");
        let dialog = Dialog::Secret {
            title: "Set credential".into(),
            buffer,
            action: SecretAction::ProviderCredential("local".into()),
        };
        let text = frame(&dialog, 60, 16)
            .rows
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("super-secret-key"));
        assert!(text.contains('•'));
    }

    #[test]
    fn provider_form_marks_the_focused_field() {
        let dialog = Dialog::ProviderForm(ProviderForm::add());
        let text = frame(&dialog, 60, 16)
            .rows
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("> id"));
        assert!(text.contains("adapter"));
        assert!(text.contains("workers"));
    }
}
