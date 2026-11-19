//! Manual full-screen layout.
//!
//! Crossterm provides terminal control only, so the chat frame is drawn from
//! plain text rows: a context pane, a scrollable conversation pane of
//! full-width message rows, a command popover, and a bordered input box.
//! Rendering is deterministic for a given size so the layout and wrapping
//! helpers can be tested without a terminal.

use crate::tui::frame::components::{clip, fill, panel, round_border, separator, Frame, Row, Tone};
use crate::tui::pages::chat::{ChatState, Message, Pick, Picker, Role, ToolState};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Narrowest terminal that still shows every pane.
pub const MIN_WIDTH: usize = 30;
/// Shortest terminal that still shows every pane.
pub const MIN_HEIGHT: usize = 12;

const CONTEXT_ROWS: usize = 4;
const INPUT_ROWS: usize = 3;
const CHROME_ROWS: usize = 1 + CONTEXT_ROWS + 1 + INPUT_ROWS + 1;

/// Number of conversation rows for one terminal height.
pub fn conversation_height(height: usize) -> usize {
    height.saturating_sub(CHROME_ROWS)
}

/// Largest scroll offset that still fills the conversation pane.
pub fn max_scroll(state: &ChatState, width: usize, height: usize) -> usize {
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return 0;
    }
    conversation_with(
        &state.messages,
        width - 4,
        0,
        state.show_thinking,
        state.show_tool_detail,
    )
    .len()
    .saturating_sub(conversation_height(height))
}

/// Build one frame for a terminal size.
pub fn render(state: &ChatState, width: usize, height: usize) -> Frame {
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return too_small(width, height);
    }
    let content = width - 4;
    let mut rows = Vec::with_capacity(height);

    rows.push(round_border('╭', '╮', width));
    for index in 0..CONTEXT_ROWS {
        rows.push(panel(
            fill(context_row(state, index, content), content),
            width,
        ));
    }
    rows.push(separator(width));

    let area = conversation_height(height);
    let mut lines = if let Some(picker) = &state.picker {
        picker_lines(picker, area, content)
    } else {
        let lines = conversation_with(
            &state.messages,
            content,
            state.spinner,
            state.show_thinking,
            state.show_tool_detail,
        );
        let maximum = lines.len().saturating_sub(area);
        let scroll = if state.follow {
            maximum
        } else {
            state.scroll.min(maximum)
        };
        lines.into_iter().skip(scroll).take(area).collect()
    };
    lines.resize(area, Row::default());
    if state.picker.is_none() {
        overlay_palette(state, &mut lines, area, content);
    }
    for line in lines {
        rows.push(panel(line, width));
    }

    rows.push(input_top(width));
    let (input_row, cursor) = input_row(state, content, height - 3);
    rows.push(input_row);
    rows.push(input_bottom(width));
    rows.push(round_border('╰', '╯', width));

    debug_assert_eq!(rows.len(), height);
    Frame { rows, cursor }
}

/// Wrap text to `width` columns, preserving explicit line breaks.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut rest = paragraph;
        while !rest.is_empty() {
            let (line, remainder) = take_line(rest, width);
            lines.push(line);
            rest = remainder;
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn take_line(text: &str, width: usize) -> (String, &str) {
    let mut used = 0;
    let mut end = 0;
    let mut last_space = None;
    for (index, character) in text.char_indices() {
        let cells = character.width().unwrap_or(0);
        if used + cells > width {
            break;
        }
        used += cells;
        end = index + character.len_utf8();
        if character == ' ' {
            last_space = Some((index, end));
        }
    }
    if end == text.len() {
        return (text.to_string(), "");
    }
    if text[end..].starts_with(' ') {
        return (text[..end].to_string(), &text[end + 1..]);
    }
    if let Some((break_at, resume)) = last_space {
        if break_at > 0 {
            return (text[..break_at].to_string(), &text[resume..]);
        }
    }
    if end == 0 {
        let character = text.chars().next().unwrap_or(' ');
        end = character.len_utf8();
    }
    (text[..end].to_string(), &text[end..])
}

/// Conversation rows as styled spans, before pane borders.
///
/// Rows are exact `width` columns wide so alignment and truncation are cheap
/// to verify.
#[cfg(test)]
pub fn conversation(messages: &[Message], width: usize, spinner: usize) -> Vec<Row> {
    conversation_with(messages, width, spinner, false, false)
}

fn conversation_with(
    messages: &[Message],
    width: usize,
    spinner: usize,
    show_thinking: bool,
    show_tool_detail: bool,
) -> Vec<Row> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            lines.push(fill(Vec::new(), width));
        }
        match message.role {
            Role::User | Role::Assistant => {
                lines.extend(message_rows(message, width, spinner));
            }
            Role::Thinking => {
                lines.extend(thinking_rows(message, width, spinner, show_thinking));
            }
            Role::Tool => {
                lines.extend(tool_rows(message, width, spinner, show_tool_detail));
            }
            Role::Notice | Role::Error => lines.extend(status(message, width)),
        }
    }
    lines
}

/// Full-width message rows with one column of inner padding on each side.
///
/// User rows carry [`Tone::UserRow`] across the whole content width, so the
/// draw layer paints their background through the text and both padding
/// columns. Assistant rows stay transparent.
fn message_rows(message: &Message, width: usize, spinner: usize) -> Vec<Row> {
    let text = block_text(message, spinner);
    let tone = match message.role {
        Role::User => Tone::UserRow,
        _ => Tone::Assistant,
    };
    if width < 4 {
        return wrap(&text, width)
            .into_iter()
            .map(|line| Row::single(tone, clip(&line, width)))
            .collect();
    }
    let inner = width - 2;
    wrap(&text, inner)
        .into_iter()
        .map(|line| {
            let padding = inner.saturating_sub(line.width());
            Row::single(tone, format!(" {line}{} ", " ".repeat(padding)))
        })
        .collect()
}

fn block_text(message: &Message, spinner: usize) -> String {
    if !message.streaming {
        return message.text.clone();
    }
    if message.text.is_empty() {
        "·".repeat(1 + spinner % 3)
    } else {
        format!("{}▌", message.text)
    }
}

fn status(message: &Message, width: usize) -> Vec<Row> {
    let (marker, marker_tone, tone) = match message.role {
        Role::Error => ("! ", Tone::Error, Tone::Error),
        _ => ("· ", Tone::Dim, Tone::Notice),
    };
    wrap(&message.text, width.saturating_sub(2))
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let lead = if index == 0 { marker } else { "  " };
            fill(vec![(marker_tone, lead.to_string()), (tone, line)], width)
        })
        .collect()
}

fn thinking_rows(message: &Message, width: usize, spinner: usize, expanded: bool) -> Vec<Row> {
    let chars = message.text.chars().count();
    let label = if expanded {
        "▾ Thinking".to_string()
    } else if message.streaming && chars == 0 {
        format!("▸ Thinking {}", "·".repeat(1 + spinner % 3))
    } else if message.streaming {
        format!("▸ Thinking · {chars} chars …")
    } else {
        format!("▸ Thinking · {chars} chars")
    };
    let tone = Tone::Thinking;
    if !expanded {
        return vec![fill(vec![(tone, clip(&label, width))], width)];
    }
    let mut rows = vec![fill(vec![(tone, clip(&label, width))], width)];
    if message.text.is_empty() {
        return rows;
    }
    let inner = width.saturating_sub(2);
    let lines = wrap(&message.text, inner);
    let count = lines.len();
    for (index, line) in lines.into_iter().enumerate() {
        let mut text = format!("  {line}");
        if message.streaming && index + 1 == count {
            text.push('▌');
        }
        rows.push(fill(vec![(tone, clip(&text, width))], width));
    }
    rows
}

fn tool_rows(message: &Message, width: usize, spinner: usize, detail: bool) -> Vec<Row> {
    let Some(tool) = &message.tool else {
        return Vec::new();
    };
    let mut title = format!("→ {}", tool.name);
    if let Some(server) = &tool.server {
        title.push_str("  ");
        title.push_str(server);
    }
    let mut rows = vec![fill(vec![(Tone::Tool, clip(&title, width))], width)];
    let (result, tone) = match tool.state {
        ToolState::Pending => (
            format!("← running {}", "·".repeat(1 + spinner % 3)),
            Tone::Dim,
        ),
        ToolState::Ok => {
            let duration = tool
                .duration_ms
                .map(|millis| format!(" · {millis}ms"))
                .unwrap_or_default();
            (format!("← ok{duration}"), Tone::Tool)
        }
        ToolState::Failed => {
            let detail = if tool.detail.is_empty() {
                String::new()
            } else {
                format!(" · {}", tool.detail)
            };
            (format!("← error{detail}"), Tone::ToolError)
        }
    };
    rows.push(fill(vec![(tone, clip(&result, width))], width));
    if detail {
        if let Some(agent) = &tool.agent {
            rows.push(fill(
                vec![(Tone::Dim, clip(&format!("  agent {agent}"), width))],
                width,
            ));
        }
        if let Some(server) = &tool.server {
            rows.push(fill(
                vec![(Tone::Dim, clip(&format!("  server {server}"), width))],
                width,
            ));
        }
        if !tool.detail.is_empty() && tool.state != ToolState::Pending {
            for line in wrap(&tool.detail, width.saturating_sub(2)) {
                rows.push(fill(
                    vec![(Tone::Dim, clip(&format!("  {line}"), width))],
                    width,
                ));
            }
        }
    }
    rows
}

fn context_row(state: &ChatState, index: usize, width: usize) -> Vec<(Tone, String)> {
    let title = format!("goldenbee · {}", state.profile);
    match index {
        0 => spread(
            &title,
            Tone::Title,
            &state.status_label(),
            Tone::Status,
            width,
        ),
        1 => vec![
            (Tone::Dim, "gateway  ".to_string()),
            (Tone::Notice, clip(&state.gateway, width.saturating_sub(9))),
        ],
        2 => spread(
            &format!("worker   {}", state.worker),
            Tone::Notice,
            &format!("agent {}", state.agent),
            Tone::Notice,
            width,
        ),
        _ => vec![
            (Tone::Dim, "session  ".to_string()),
            (Tone::Dim, clip(&state.session, width.saturating_sub(9))),
        ],
    }
}

fn spread(
    left: &str,
    left_tone: Tone,
    right: &str,
    right_tone: Tone,
    width: usize,
) -> Vec<(Tone, String)> {
    let right = clip(right, width / 2);
    let left = clip(left, width.saturating_sub(right.width() + 1));
    let gap = width.saturating_sub(left.width() + right.width());
    vec![
        (left_tone, left),
        (Tone::Border, " ".repeat(gap)),
        (right_tone, right),
    ]
}

fn input_top(width: usize) -> Row {
    let mut row = Row::single(Tone::Border, "│ ");
    row.push(Tone::Border, "╭");
    row.push(Tone::Border, "─".repeat(width - 6));
    row.push(Tone::Border, "╮");
    row.push(Tone::Border, " │");
    row
}

fn input_bottom(width: usize) -> Row {
    let mut row = Row::single(Tone::Border, "│ ");
    row.push(Tone::Border, "╰");
    row.push(Tone::Border, "─".repeat(width - 6));
    row.push(Tone::Border, "╯");
    row.push(Tone::Border, " │");
    row
}

fn input_row(state: &ChatState, content: usize, y: usize) -> (Row, Option<(u16, u16)>) {
    let columns = content.saturating_sub(6);
    let visible = state.input.visible(columns);
    let padding = columns.saturating_sub(visible.text.width());
    let mut row = Row::single(Tone::Border, "│ ");
    row.push(Tone::Border, "│");
    row.push(Tone::Border, " ");
    row.push(Tone::Prompt, "> ");
    row.push(Tone::Input, visible.text);
    row.push(Tone::Input, " ".repeat(padding));
    row.push(Tone::Border, " ");
    row.push(Tone::Border, "│");
    row.push(Tone::Border, " │");
    let cursor = if state.picker.is_some() {
        None
    } else {
        Some((
            (6 + visible.cursor).min(u16::MAX as usize) as u16,
            y.min(u16::MAX as usize) as u16,
        ))
    };
    (row, cursor)
}

fn overlay_palette(state: &ChatState, lines: &mut [Row], area: usize, width: usize) {
    let Some(popover) = palette_popover(state, area, width) else {
        return;
    };
    let start = area.saturating_sub(popover.len());
    for (offset, row) in popover.into_iter().enumerate() {
        if let Some(line) = lines.get_mut(start + offset) {
            *line = row;
        }
    }
}

fn palette_popover(state: &ChatState, area: usize, width: usize) -> Option<Vec<Row>> {
    if area < 3 || width < 16 || state.palette.is_none() {
        return None;
    }
    let matches = state.palette_matches();
    if matches.is_empty() {
        return None;
    }
    let interior = area - 2;
    let selected = state
        .palette
        .as_ref()
        .map_or(0, |palette| palette.selected.min(matches.len() - 1));
    let start = selected.saturating_sub(interior.saturating_sub(1));
    let shown: Vec<_> = matches.iter().skip(start).take(interior).collect();
    let name_width = shown
        .iter()
        .map(|command| command.name.width())
        .max()
        .unwrap_or(0);
    let description_width = shown
        .iter()
        .map(|command| command.description.width())
        .max()
        .unwrap_or(0);
    let box_width = (name_width + description_width + 4).min(width);
    let usable = box_width.saturating_sub(4);

    let mut lines = Vec::with_capacity(shown.len() + 2);
    lines.push(fill(
        vec![
            (Tone::Border, "╭".to_string()),
            (Tone::Border, "─".repeat(box_width - 2)),
            (Tone::Border, "╮".to_string()),
        ],
        width,
    ));
    for (index, command) in shown.into_iter().enumerate() {
        let highlighted = start + index == selected;
        let (name_tone, description_tone) = if highlighted {
            (Tone::User, Tone::User)
        } else {
            (Tone::Title, Tone::Dim)
        };
        let name_column = name_width.min(usable);
        let name = clip(command.name, name_column);
        let mut spans = vec![(Tone::Border, "│ ".to_string()), (name_tone, name)];
        let mut used = name_column;
        let pad = name_column.saturating_sub(spans[1].1.width());
        spans.push((name_tone, " ".repeat(pad)));
        if usable > name_column + 2 {
            let room = usable - name_column - 2;
            spans.push((Tone::Dim, "  ".to_string()));
            spans.push((description_tone, clip(command.description, room)));
            used += 2 + room.min(command.description.width());
        }
        spans.push((Tone::Border, " ".repeat(usable.saturating_sub(used))));
        spans.push((Tone::Border, " │".to_string()));
        lines.push(fill(spans, width));
    }
    lines.push(fill(
        vec![
            (Tone::Border, "╰".to_string()),
            (Tone::Border, "─".repeat(box_width - 2)),
            (Tone::Border, "╯".to_string()),
        ],
        width,
    ));
    Some(lines)
}

fn picker_lines(picker: &Picker, area: usize, width: usize) -> Vec<Row> {
    let context = match picker.kind {
        Pick::Worker => "select Worker",
        Pick::Agent => "select agent",
    };
    let mut lines = vec![fill(
        vec![(Tone::Title, format!("{context} · {}", picker.title))],
        width,
    )];
    let room = area.saturating_sub(3);
    let start = if room == 0 {
        0
    } else {
        picker.selected.saturating_sub(room.saturating_sub(1))
    };
    for (index, item) in picker.items.iter().enumerate().skip(start).take(room) {
        let marker = if index == picker.selected { "> " } else { "  " };
        let tone = if index == picker.selected {
            Tone::User
        } else {
            Tone::Dim
        };
        lines.push(fill(vec![(tone, format!("{marker}{item}"))], width));
    }
    lines.push(fill(
        vec![(Tone::Dim, clip("enter selects · esc cancels", width))],
        width,
    ));
    lines
}

fn too_small(width: usize, height: usize) -> Frame {
    let message = format!("terminal too small: need {MIN_WIDTH}x{MIN_HEIGHT}");
    let mut rows = vec![Row::single(Tone::Error, clip(&message, width))];
    rows.resize(height.max(1), Row::default());
    Frame { rows, cursor: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::input::Input;
    use crate::tui::pages::chat::{ChatState, Role};
    use unicode_width::UnicodeWidthStr;

    fn typed_state() -> ChatState {
        let mut state = ChatState::new(
            "local",
            "http://127.0.0.1:8088",
            Vec::new(),
            "worker-a".to_string(),
            "echo".to_string(),
        );
        state.session = "session-1".to_string();
        state.messages.clear();
        state
    }

    fn push(state: &mut ChatState, role: Role, text: &str) {
        state.messages.push(Message {
            role,
            text: text.to_string(),
            streaming: false,
            tool: None,
        });
    }

    fn plain(frame: &Frame) -> String {
        frame
            .rows
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn wrapping_keeps_words_and_hard_breaks_long_tokens() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap("first\nsecond", 20), vec!["first", "second"]);
        assert_eq!(wrap("", 10), vec![""]);
        assert_eq!(wrap("a  b", 10), vec!["a  b"]);
    }

    #[test]
    fn user_rows_use_the_full_width_user_background() {
        let mut state = typed_state();
        push(&mut state, Role::User, "hi");
        push(&mut state, Role::Assistant, "hello back");
        let lines = conversation(&state.messages, 32, 0);

        let user = &lines[0];
        assert_eq!(user.text(), format!(" hi{} ", " ".repeat(28)));
        assert_eq!(user.text().width(), 32);
        assert!(user.spans.iter().all(|(tone, _)| *tone == Tone::UserRow));
        assert!(lines[1]
            .spans
            .iter()
            .all(|(tone, _)| *tone != Tone::UserRow));

        let assistant = &lines[2];
        assert_eq!(assistant.text().width(), 32);
        assert!(assistant
            .spans
            .iter()
            .all(|(tone, _)| *tone == Tone::Assistant));

        let text = lines.iter().map(Row::text).collect::<Vec<_>>().join("\n");
        assert!(!text.contains("you>"));
        assert!(!text.contains("ai>"));
        assert!(!text.contains('╭'));
    }

    #[test]
    fn wrapped_user_rows_keep_the_background_on_every_line() {
        let mut state = typed_state();
        push(
            &mut state,
            Role::User,
            "a full-width message with enough words to wrap several times",
        );
        let lines = conversation(&state.messages, 24, 0);
        assert!(lines.len() > 1);
        for row in &lines {
            assert_eq!(row.text().width(), 24);
            assert!(row.text().starts_with(' '));
            assert!(row.text().ends_with(' '));
            assert!(row.spans.iter().all(|(tone, _)| *tone == Tone::UserRow));
        }
    }

    #[test]
    fn assistant_rows_have_no_user_background() {
        let mut state = typed_state();
        push(&mut state, Role::Assistant, "plain answer");
        let lines = conversation(&state.messages, 32, 0);
        assert_eq!(lines[0].text().width(), 32);
        assert!(lines[0]
            .spans
            .iter()
            .all(|(tone, _)| *tone == Tone::Assistant));
    }

    #[test]
    fn unicode_message_rows_never_overflow_the_frame() {
        let mut state = typed_state();
        push(&mut state, Role::User, "こんにちは世界");
        push(
            &mut state,
            Role::Assistant,
            "日本語のとても長いメッセージです。",
        );
        state.input.insert_text("/wor");
        state.sync_palette();
        for width in [30, 31, 40, 57, 80, 121] {
            let rows = conversation(&state.messages, width - 4, 0);
            assert!(rows.iter().all(|row| row.text().width() == width - 4));
            for height in [12, 16, 24] {
                let frame = render(&state, width, height);
                assert_eq!(frame.rows.len(), height);
                for row in &frame.rows {
                    assert_eq!(row.text().width(), width);
                }
            }
        }
    }

    #[test]
    fn notices_and_errors_are_inline_status_lines() {
        let mut state = typed_state();
        push(&mut state, Role::Notice, "connected");
        push(&mut state, Role::Error, "gateway refused");
        let lines = conversation(&state.messages, 32, 0);
        let text = lines.iter().map(Row::text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("· connected"));
        assert!(text.contains("! gateway refused"));
        assert!(lines.iter().all(|row| !row.text().contains('╭')));
    }

    #[test]
    fn thinking_is_collapsed_by_default_and_expands() {
        let mut state = typed_state();
        state.thinking_delta("Need to inspect the provider profile first.");
        let collapsed = conversation(&state.messages, 40, 0)
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(collapsed.contains("▸ Thinking"));
        assert!(!collapsed.contains("provider profile"));
        let expanded = conversation_with(&state.messages, 40, 0, true, false)
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(expanded.contains("▾ Thinking"));
        assert!(expanded.contains("provider profile"));
    }

    #[test]
    fn tool_calls_show_pending_and_result_states() {
        let mut state = typed_state();
        state.tool_call(
            "call-1".to_string(),
            "exec_command".to_string(),
            Some("local".to_string()),
            None,
        );
        let pending = conversation(&state.messages, 40, 0)
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(pending.contains("exec_command"));
        assert!(pending.contains("running"));
        state.tool_result(
            "call-1".to_string(),
            "exec_command".to_string(),
            true,
            None,
            Some(12),
        );
        let detail = conversation_with(&state.messages, 40, 0, false, true)
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(detail.contains("ok"));
        assert!(detail.contains("12ms"));
        assert!(detail.contains("server local"));
    }

    #[test]
    fn streaming_marker_fills_an_empty_assistant_block() {
        let mut state = typed_state();
        state.messages.push(Message {
            role: Role::Assistant,
            text: String::new(),
            streaming: true,
            tool: None,
        });
        let text = conversation(&state.messages, 32, 0)
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains('·'));
        state.spinner = 2;
        let text = conversation(&state.messages, 32, 2)
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("···"));
    }

    #[test]
    fn streaming_indicator_is_visible_in_the_context_pane() {
        let mut state = typed_state();
        state.busy = true;
        assert!(state.status_label().starts_with("running "));
        state.busy = false;
        assert_eq!(state.status_label(), "idle");
    }

    #[test]
    fn palette_overlays_the_lower_conversation_rows() {
        let mut state = typed_state();
        push(&mut state, Role::Assistant, "answer");
        state.input.insert_text("/wor");
        state.sync_palette();
        let frame = render(&state, 40, 20);
        let text = plain(&frame);
        assert!(text.contains("/worker"));
        assert!(text.contains("/workers"));
        assert!(!text.contains("/gateway"));
        assert!(frame.cursor.is_some());
        let highlighted = frame
            .rows
            .iter()
            .find(|row| row.text().contains("/worker"))
            .expect("command row");
        assert!(highlighted
            .spans
            .iter()
            .any(|(tone, _)| *tone == Tone::User));
    }

    #[test]
    fn fixed_size_frame_is_stable() {
        let mut state = typed_state();
        push(&mut state, Role::User, "hi");
        push(&mut state, Role::Assistant, "hello back");
        state.input.insert_text("next");
        let frame = render(&state, 40, 18);
        assert_eq!(frame.rows.len(), 18);
        let text = plain(&frame);
        let expected = [
            "╭──────────────────────────────────────╮",
            "│ goldenbee · local               idle │",
            "│ gateway  http://127.0.0.1:8088       │",
            "│ worker   worker-a         agent echo │",
            "│ session  session-1                   │",
            "├──────────────────────────────────────┤",
            "│  hi                                  │",
            "│                                      │",
            "│  hello back                          │",
            "│                                      │",
            "│                                      │",
            "│                                      │",
            "│                                      │",
            "│                                      │",
            "│ ╭──────────────────────────────────╮ │",
            "│ │ > next                           │ │",
            "│ ╰──────────────────────────────────╯ │",
            "╰──────────────────────────────────────╯",
        ];
        assert_eq!(text, expected.join("\n"));
        assert!(frame.rows.iter().all(|row| row.text().width() == 40));
        let user = frame.rows[6]
            .spans
            .iter()
            .find(|(tone, _)| *tone == Tone::UserRow)
            .expect("user row carries the background tone");
        assert_eq!(user.1.width(), 36);
        assert!(!frame.rows[8]
            .spans
            .iter()
            .any(|(tone, _)| *tone == Tone::UserRow));
        assert_eq!(frame.cursor, Some((10, 15)));
    }

    #[test]
    fn scroll_bar_keeps_viewport_inside_content() {
        let mut state = typed_state();
        for index in 0..40 {
            push(&mut state, Role::Notice, &format!("line {index}"));
        }
        let maximum = max_scroll(&state, 40, 16);
        assert!(maximum > 0);
        state.scroll_by(-1, maximum);
        assert_eq!(state.scroll, 0);
        state.scroll_to_end(maximum);
        let frame = render(&state, 40, 16);
        let text = plain(&frame);
        assert!(text.contains("line 39"));
    }

    #[test]
    fn picker_replaces_the_conversation_pane() {
        let mut state = typed_state();
        state.picker = Some(Picker::new(
            Pick::Worker,
            "workers",
            vec!["worker-a".to_string(), "worker-b".to_string()],
        ));
        let frame = render(&state, 40, 16);
        assert!(frame.cursor.is_none());
        let text = plain(&frame);
        assert!(text.contains("select Worker"));
        assert!(text.contains("> worker-a"));
        assert!(text.contains("  worker-b"));
    }

    #[test]
    fn small_terminals_get_a_readable_message() {
        let state = typed_state();
        let frame = render(&state, 20, 5);
        assert_eq!(frame.rows.len(), 5);
        assert!(frame.rows[0].text().contains("terminal too small"));
        assert!(frame.cursor.is_none());
    }

    #[test]
    fn input_row_scrolls_with_a_long_line() {
        let mut state = typed_state();
        let mut input = Input::default();
        input.insert_text("abcdefghijklmnopqrstuvwxyz0123456789");
        state.input = input;
        let frame = render(&state, 30, 12);
        let text = plain(&frame);
        assert!(!text.contains("abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(text.contains("0123456789"));
    }
}
