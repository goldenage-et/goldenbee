//! Shared frame primitives for chat and management pages.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Visual weight of one run of text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    /// Frame and pane borders.
    Border,
    /// Session identity and table headers.
    Title,
    /// Running or idle status.
    Status,
    /// Submitted chat text on its full-row background.
    UserRow,
    /// Highlighted interactive label and selected table row.
    User,
    /// Assistant or ordinary data text.
    Assistant,
    /// Collapsed or expanded model reasoning.
    Thinking,
    /// Tool call and successful result.
    Tool,
    /// Failed tool result.
    ToolError,
    /// Local CLI status.
    Notice,
    /// Recoverable failure.
    Error,
    /// Secondary text such as hints.
    Dim,
    /// Input prompt marker.
    Prompt,
    /// Input text.
    Input,
}

/// One rendered row made of styled runs.
#[derive(Clone, Debug, Default)]
pub struct Row {
    /// Runs in display order.
    pub spans: Vec<(Tone, String)>,
}

impl Row {
    /// Build a row from one run.
    pub fn single(tone: Tone, text: impl Into<String>) -> Self {
        Self {
            spans: vec![(tone, text.into())],
        }
    }

    /// Append one run.
    pub fn push(&mut self, tone: Tone, text: impl Into<String>) {
        self.spans.push((tone, text.into()));
    }

    /// Concatenated plain text, for tests and diagnostics.
    #[cfg(test)]
    pub fn text(&self) -> String {
        self.spans
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<String>()
    }
}

/// One full frame and where to place the cursor.
pub struct Frame {
    /// Rows top to bottom.
    pub rows: Vec<Row>,
    /// Cursor position in terminal cells.
    pub cursor: Option<(u16, u16)>,
}

/// Truncate one line to `width` display columns.
pub fn clip(text: &str, width: usize) -> String {
    let mut clipped = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cells = character.width().unwrap_or(0);
        if used + cells > width {
            break;
        }
        clipped.push(character);
        used += cells;
    }
    clipped
}

/// Pad one line to exactly `width` display columns.
pub fn fit(text: &str, width: usize) -> String {
    let clipped = clip(text, width);
    let padding = width.saturating_sub(clipped.width());
    format!("{clipped}{}", " ".repeat(padding))
}

/// Fill the remaining width of a row with dim text.
pub fn fill(spans: Vec<(Tone, String)>, width: usize) -> Row {
    let mut spans = clipped(spans, width);
    let used: usize = spans.iter().map(|(_, text)| text.width()).sum();
    spans.push((Tone::Dim, " ".repeat(width.saturating_sub(used))));
    Row { spans }
}

/// Clip styled runs to `width` columns.
pub fn clipped(spans: Vec<(Tone, String)>, width: usize) -> Vec<(Tone, String)> {
    let mut used = 0;
    let mut clipped_spans = Vec::new();
    for (tone, text) in spans {
        if used >= width {
            break;
        }
        let text = clip(&text, width - used);
        used += text.width();
        if !text.is_empty() {
            clipped_spans.push((tone, text));
        }
    }
    clipped_spans
}

/// Add side borders and pad one content row to the full frame width.
pub fn panel(row: Row, width: usize) -> Row {
    let content = width.saturating_sub(4);
    let mut spans = clipped(row.spans, content);
    let used: usize = spans.iter().map(|(_, text)| text.width()).sum();
    let mut row = Row::single(Tone::Border, "│ ");
    row.spans.append(&mut spans);
    row.push(Tone::Border, " ".repeat(content.saturating_sub(used)));
    row.push(Tone::Border, " │");
    row
}

/// A plain horizontal separator with side borders.
pub fn separator(width: usize) -> Row {
    Row::single(Tone::Border, format!("├{}┤", "─".repeat(width - 2)))
}

/// A rounded top or bottom border.
pub fn round_border(left: char, right: char, width: usize) -> Row {
    Row::single(
        Tone::Border,
        format!("{left}{}{right}", "─".repeat(width - 2)),
    )
}

/// Build a bordered header row with a title embedded in the top border.
pub fn titled_border(title: &str, width: usize) -> Row {
    let inner = width.saturating_sub(2);
    let label = format!(" {title} ");
    let label = clip(&label, inner);
    let remaining = inner.saturating_sub(label.width());
    Row::single(Tone::Border, format!("╭{label}{}╮", "─".repeat(remaining)))
}

/// Number of rows visible for a selection and pane capacity.
pub fn window(length: usize, selected: usize, capacity: usize) -> (usize, usize) {
    if length <= capacity || capacity == 0 {
        return (0, length.min(capacity.max(1)));
    }
    let start = selected.saturating_sub(capacity.saturating_sub(1));
    (start, (start + capacity).min(length))
}

/// Render one table inside the management content pane.
#[allow(clippy::too_many_arguments)]
pub fn table(
    headers: &[&str],
    rows: &[Vec<String>],
    selected: Option<usize>,
    start: usize,
    capacity: usize,
    width: usize,
) -> Vec<Row> {
    let widths = fit_widths(headers, rows, width);
    let mut lines = Vec::with_capacity(capacity + 1);
    let header = headers
        .iter()
        .zip(&widths)
        .map(|(header, width)| fit(header, *width))
        .collect::<Vec<_>>()
        .join(" ");
    lines.push(fill(vec![(Tone::Title, header)], width));
    if rows.is_empty() {
        lines.push(fill(vec![(Tone::Dim, "no rows".to_string())], width));
        return lines;
    }
    for (offset, row) in rows.iter().enumerate().skip(start).take(capacity) {
        let text = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| fit(cell, *width))
            .collect::<Vec<_>>()
            .join(" ");
        let tone = if Some(offset) == selected {
            Tone::User
        } else {
            Tone::Assistant
        };
        lines.push(fill(vec![(tone, text)], width));
    }
    lines
}

/// Column widths fitted to the available content width.
pub fn fit_widths(headers: &[&str], rows: &[Vec<String>], available: usize) -> Vec<usize> {
    let columns = headers.len();
    if columns == 0 {
        return Vec::new();
    }
    let mut widths = vec![0usize; columns];
    for (index, header) in headers.iter().enumerate() {
        widths[index] = header.width().min(32);
    }
    for row in rows {
        for (index, cell) in row.iter().enumerate().take(columns) {
            widths[index] = widths[index].max(cell.width().min(32));
        }
    }
    let gaps = columns.saturating_sub(1);
    let mut total: usize = widths.iter().sum::<usize>() + gaps;
    while total > available {
        let Some(index) = widths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > 3)
            .max_by_key(|(_, width)| **width)
            .map(|(index, _)| index)
        else {
            break;
        };
        widths[index] -= 1;
        total -= 1;
    }
    widths
}

/// One management tab row.
pub fn tabs(labels: &[&str], active: usize, width: usize) -> Row {
    let mut spans = Vec::new();
    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            spans.push((Tone::Dim, "  ".to_string()));
        }
        spans.push((
            if index == active {
                Tone::User
            } else {
                Tone::Dim
            },
            label.to_string(),
        ));
    }
    fill(spans, width)
}
