//! Terminal ownership and frame painting.

use crate::tui::frame::{Frame, Tone};
use crossterm::cursor::{Hide, MoveTo, SetCursorStyle, Show};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{
    self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{execute, queue};
use std::io::{self, Write};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};

/// Current terminal size, with a safe fallback.
pub fn size() -> (usize, usize) {
    match terminal::size() {
        Ok((width, height)) if width > 0 && height > 0 => (width as usize, height as usize),
        _ => (80, 24),
    }
}

/// Paint one frame.
pub fn draw(frame: &Frame) -> io::Result<()> {
    let mut out = io::stdout().lock();
    queue!(out, BeginSynchronizedUpdate, Clear(ClearType::All))?;
    for (index, row) in frame.rows.iter().enumerate() {
        queue!(out, MoveTo(0, index as u16))?;
        for (tone, text) in &row.spans {
            queue!(
                out,
                SetForegroundColor(color(*tone)),
                SetBackgroundColor(background(*tone)),
                Print(text)
            )?;
        }
        queue!(out, ResetColor, Clear(ClearType::UntilNewLine))?;
    }
    if let Some((x, y)) = frame.cursor {
        queue!(out, MoveTo(x, y), Show)?;
    } else {
        queue!(out, Hide)?;
    }
    queue!(out, EndSynchronizedUpdate)?;
    out.flush()
}

fn color(tone: Tone) -> Color {
    match tone {
        Tone::Border => Color::DarkGrey,
        Tone::Title => Color::Cyan,
        Tone::Status => Color::Yellow,
        Tone::UserRow => Color::White,
        Tone::User => Color::Green,
        Tone::Assistant => Color::White,
        Tone::Thinking => Color::DarkGrey,
        Tone::Tool => Color::Cyan,
        Tone::ToolError => Color::Red,
        Tone::Notice => Color::Blue,
        Tone::Error => Color::Red,
        Tone::Dim => Color::DarkGrey,
        Tone::Prompt => Color::Cyan,
        Tone::Input => Color::White,
    }
}

fn background(tone: Tone) -> Color {
    match tone {
        Tone::UserRow => Color::DarkGrey,
        _ => Color::Reset,
    }
}

/// Terminal ownership that restores the screen on every exit path.
pub struct Terminal;

static RESTORED: AtomicBool = AtomicBool::new(true);

impl Terminal {
    /// Enter the alternate screen and raw mode.
    pub fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut out = io::stdout();
        if let Err(error) = execute!(out, EnterAlternateScreen, EnableBracketedPaste, Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        RESTORED.store(false, Ordering::SeqCst);
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |information| {
            restore();
            previous(information);
        }));
        Ok(Self)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    if RESTORED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    let _ = queue!(
        out,
        Show,
        SetCursorStyle::DefaultUserShape,
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
    let _ = out.flush();
    let _ = terminal::disable_raw_mode();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_user_rows_paint_a_background() {
        assert_eq!(background(Tone::UserRow), Color::DarkGrey);
        assert_eq!(color(Tone::UserRow), Color::White);
        for tone in [
            Tone::Border,
            Tone::Title,
            Tone::Status,
            Tone::User,
            Tone::Assistant,
            Tone::Notice,
            Tone::Error,
            Tone::Dim,
            Tone::Prompt,
            Tone::Input,
        ] {
            assert_eq!(background(tone), Color::Reset);
        }
    }
}
