//! Management shell, tabs, page tables, and status line.

use crate::tui::app::App;
use crate::tui::frame::components::{
    fill, panel, separator, table, tabs, titled_border, window, Frame, Row, Tone,
};
use crate::tui::frame::forms;
use crate::tui::pages::{Page, LABELS};

/// Narrowest management terminal.
pub const MIN_WIDTH: usize = 60;
/// Shortest management terminal.
pub const MIN_HEIGHT: usize = 12;

const CHROME_ROWS: usize = 6;

/// Build one management frame.
pub fn render(app: &App, width: usize, height: usize) -> Frame {
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return too_small(width, height);
    }
    let content = width.saturating_sub(4);
    let area = height.saturating_sub(CHROME_ROWS);
    let mut rows = Vec::with_capacity(height);
    rows.push(titled_border(
        &format!("goldenbee · {}", app.profile),
        width,
    ));
    rows.push(panel(tabs(&LABELS, app.page.index(), content), width));
    rows.push(separator(width));

    if let Some(dialog) = &app.dialog {
        rows.extend(forms::render(dialog, width, area));
    } else {
        let mut lines = page_lines(app, content, area);
        lines.resize(area, Row::default());
        for line in lines {
            rows.push(panel(line, width));
        }
    }

    rows.push(separator(width));
    rows.push(status_row(app, content));
    rows.push(crate::tui::frame::components::round_border('╰', '╯', width));
    debug_assert_eq!(rows.len(), height);
    Frame { rows, cursor: None }
}

fn page_lines(app: &App, width: usize, area: usize) -> Vec<Row> {
    let capacity = area.saturating_sub(1);
    match app.page {
        Page::Workers => {
            let rows = app.workers.rows();
            let selected = Some(app.workers.selected);
            let (start, _) = window(rows.len(), app.workers.selected, capacity);
            table(
                &crate::tui::pages::workers::HEADERS,
                &rows,
                selected,
                start,
                capacity,
                width,
            )
        }
        Page::Providers => {
            let rows = app.providers.rows();
            let selected = Some(app.providers.selected);
            let (start, _) = window(rows.len(), app.providers.selected, capacity);
            table(
                &crate::tui::pages::providers::HEADERS,
                &rows,
                selected,
                start,
                capacity,
                width,
            )
        }
        Page::Models => {
            let rows = app.models.table_rows(&app.providers.providers);
            let selected = Some(app.models.selected);
            let (start, _) = window(rows.len(), app.models.selected, capacity);
            table(
                &crate::tui::pages::models::HEADERS,
                &rows,
                selected,
                start,
                capacity,
                width,
            )
        }
    }
}

fn status_row(app: &App, width: usize) -> Row {
    let hints = if let Some(dialog) = &app.dialog {
        format!("dialog · {}", dialog.title())
    } else if app.workers.filtering && app.page == Page::Workers {
        format!(
            "filter: {}  ·  enter done  ·  esc clear",
            app.workers.filter
        )
    } else if app.providers.filtering && app.page == Page::Providers {
        format!(
            "filter: {}  ·  enter done  ·  esc clear",
            app.providers.filter
        )
    } else if app.models.filtering && app.page == Page::Models {
        format!("filter: {}  ·  enter done  ·  esc clear", app.models.filter)
    } else {
        match app.page {
            Page::Workers => {
                "ctrl+p chat  enter inspect  a add  e edit  p probe  d disable  E enable  x remove  l audit  / filter  r refresh".to_string()
            }
            Page::Providers => {
                "a add  e edit  s credential  R revoke  d discover  x toggle  w workers  m models  p pair  / filter  r refresh".to_string()
            }
            Page::Models => {
                "space allow  enter preferred  c clear  r rediscover  / filter  esc providers".to_string()
            }
        }
    };
    let left = if app.status.is_empty() {
        "ready".to_string()
    } else {
        app.status.clone()
    };
    let right = crate::tui::frame::components::clip(&hints, width / 2);
    let left = crate::tui::frame::components::clip(
        &left,
        width.saturating_sub(crate::tui::frame::components::clip(&right, width / 2).len() + 1),
    );
    use unicode_width::UnicodeWidthStr;
    let gap = width.saturating_sub(left.width() + right.width());
    fill(
        vec![
            (Tone::Status, left),
            (Tone::Border, " ".repeat(gap)),
            (Tone::Dim, right),
        ],
        width,
    )
}

fn too_small(width: usize, height: usize) -> Frame {
    let message = format!("terminal too small: need {MIN_WIDTH}x{MIN_HEIGHT}");
    let mut rows = vec![Row::single(
        Tone::Error,
        crate::tui::frame::components::clip(&message, width),
    )];
    rows.resize(height.max(1), Row::default());
    Frame { rows, cursor: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use common::AdminWorkerSummary;

    fn worker() -> AdminWorkerSummary {
        AdminWorkerSummary {
            id: "worker-a".into(),
            endpoint: "http://127.0.0.1:9090".into(),
            instance: "one".into(),
            package: "echo".into(),
            capabilities: Some(vec!["general".into()]),
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
    fn shell_has_tabs_page_content_and_status() {
        let mut app = App::new(
            "local",
            "http://127.0.0.1:8088",
            Vec::new(),
            "worker-a".into(),
            "general".into(),
        );
        app.open_manage(Page::Workers, None);
        app.workers.set(vec![worker()]);
        let frame = render(&app, 90, 20);
        assert_eq!(frame.rows.len(), 20);
        let text = frame
            .rows
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("goldenbee · local"));
        assert!(text.contains("Workers"));
        assert!(text.contains("Providers"));
        assert!(text.contains("Models"));
        assert!(text.contains("worker-a"));
        assert!(text.contains("CAPABILITIES"));
        assert!(text.contains("ctrl+p chat"));
    }
}
