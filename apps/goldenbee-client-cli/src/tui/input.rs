//! Single-line chat input: editing, history, and horizontal scrolling.

use unicode_width::UnicodeWidthChar;

/// Editable single-line input buffer with recall history.
#[derive(Default)]
pub struct Input {
    characters: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    browsing: Option<usize>,
    draft: String,
}

/// Visible slice of the input line for one terminal width.
pub struct Visible {
    /// Text that fits in the available columns.
    pub text: String,
    /// Cursor column in display cells.
    pub cursor: usize,
}

impl Input {
    /// Current text.
    pub fn text(&self) -> String {
        self.characters.iter().collect()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.characters.is_empty()
    }

    /// Masked representation that never reveals the contents.
    pub fn masked(&self) -> String {
        "•".repeat(self.characters.len())
    }

    /// Remove all text and leave history browsing.
    pub fn clear(&mut self) {
        self.characters.clear();
        self.cursor = 0;
        self.browsing = None;
    }

    /// Insert one printable character at the cursor.
    pub fn insert(&mut self, character: char) {
        if character.is_control() {
            return;
        }
        self.characters.insert(self.cursor, character);
        self.cursor += 1;
        self.browsing = None;
    }

    /// Insert pasted text, flattening line breaks and dropping controls.
    pub fn insert_text(&mut self, text: &str) {
        for character in text.chars() {
            match character {
                '\r' => {}
                '\n' | '\t' => self.insert(' '),
                other if other.is_control() => {}
                other => self.insert(other),
            }
        }
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.characters.remove(self.cursor);
            self.browsing = None;
        }
    }

    /// Delete the character under the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.characters.len() {
            self.characters.remove(self.cursor);
            self.browsing = None;
        }
    }

    /// Move the cursor one character left.
    pub fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    /// Move the cursor one character right.
    pub fn right(&mut self) {
        if self.cursor < self.characters.len() {
            self.cursor += 1;
        }
    }

    /// Move the cursor to the start of the line.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Move the cursor past the last character.
    pub fn end(&mut self) {
        self.cursor = self.characters.len();
    }

    /// Delete everything before the cursor (Ctrl+U).
    pub fn kill_to_start(&mut self) {
        self.characters.drain(..self.cursor);
        self.cursor = 0;
        self.browsing = None;
    }

    /// Delete the word before the cursor (Ctrl+W).
    pub fn kill_word(&mut self) {
        let mut start = self.cursor;
        while start > 0 && self.characters[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !self.characters[start - 1].is_whitespace() {
            start -= 1;
        }
        self.characters.drain(start..self.cursor);
        self.cursor = start;
        self.browsing = None;
    }

    /// Store the current line for recall, then clear the buffer.
    pub fn remember(&mut self) {
        let text = self.text();
        if !text.trim().is_empty() && self.history.last() != Some(&text) {
            self.history.push(text);
        }
        self.clear();
    }

    /// Recall an older submitted line. Only starts when the buffer is empty.
    ///
    /// Returns false when there is no history to recall or the user is editing
    /// a line that was not recalled.
    pub fn previous(&mut self) -> bool {
        if self.history.is_empty() || (self.browsing.is_none() && !self.is_empty()) {
            return false;
        }
        let index = match self.browsing {
            Some(0) => return false,
            Some(index) => index - 1,
            None => {
                self.draft = self.text();
                self.history.len() - 1
            }
        };
        self.browsing = Some(index);
        let text = self.history[index].clone();
        self.set_text(&text);
        true
    }

    /// Recall a newer submitted line, or restore the draft at the end.
    pub fn next(&mut self) -> bool {
        let Some(index) = self.browsing else {
            return false;
        };
        if index + 1 < self.history.len() {
            self.browsing = Some(index + 1);
            let text = self.history[index + 1].clone();
            self.set_text(&text);
        } else {
            self.browsing = None;
            let draft = std::mem::take(&mut self.draft);
            self.set_text(&draft);
        }
        true
    }

    /// Slice of the line that fits `width` columns, keeping the cursor visible.
    pub fn visible(&self, width: usize) -> Visible {
        let width = width.max(1);
        let mut start = 0;
        while start < self.cursor && self.cells(start, self.cursor) >= width {
            start += 1;
        }
        let mut text = String::new();
        let mut used = 0;
        let mut cursor = 0;
        for (offset, character) in self.characters[start..].iter().enumerate() {
            if start + offset == self.cursor {
                cursor = used;
            }
            let cells = character.width().unwrap_or(0);
            if used + cells > width {
                break;
            }
            text.push(*character);
            used += cells;
        }
        if self.cursor == self.characters.len() {
            cursor = used;
        }
        Visible {
            text,
            cursor: cursor.min(width - 1),
        }
    }

    fn cells(&self, from: usize, to: usize) -> usize {
        self.characters[from..to]
            .iter()
            .map(|character| character.width().unwrap_or(0))
            .sum()
    }

    /// Replace the buffer and leave the cursor at the end.
    pub fn set_text(&mut self, text: &str) {
        self.characters = text.chars().collect();
        self.cursor = self.characters.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Input {
        let mut input = Input::default();
        input.insert_text(text);
        input
    }

    #[test]
    fn edits_at_the_cursor() {
        let mut input = typed("helo");
        input.left();
        input.left();
        input.insert('l');
        assert_eq!(input.text(), "hello");
        input.backspace();
        assert_eq!(input.text(), "helo");
        input.delete();
        assert_eq!(input.text(), "heo");
        input.end();
        input.insert('!');
        assert_eq!(input.text(), "heo!");
        input.home();
        input.insert('>');
        assert_eq!(input.text(), ">heo!");
        input.left();
        assert_eq!(input.text(), ">heo!");
    }

    #[test]
    fn control_edits_remove_words_and_prefixes() {
        let mut input = typed("one two three");
        input.kill_word();
        assert_eq!(input.text(), "one two ");
        input.kill_word();
        assert_eq!(input.text(), "one ");
        input.insert_text("two three");
        input.home();
        input.right();
        input.right();
        input.right();
        input.kill_to_start();
        assert_eq!(input.text(), " two three");
    }

    #[test]
    fn paste_flattens_lines_and_drops_controls() {
        let mut input = typed("a");
        input.insert_text("b\nc\td\r\u{7}e");
        assert_eq!(input.text(), "ab c de");
    }

    #[test]
    fn history_recalls_and_restores_the_draft() {
        let mut input = Input::default();
        input.insert_text("first");
        input.remember();
        input.insert_text("second");
        input.remember();
        assert!(input.is_empty());

        assert!(input.previous());
        assert_eq!(input.text(), "second");
        assert!(input.previous());
        assert_eq!(input.text(), "first");
        assert!(!input.previous());
        assert_eq!(input.text(), "first");
        assert!(input.next());
        assert_eq!(input.text(), "second");
        assert!(input.next());
        assert_eq!(input.text(), "");
        assert!(!input.next());
    }

    #[test]
    fn history_does_not_start_while_editing_a_draft() {
        let mut input = Input::default();
        input.insert_text("saved");
        input.remember();
        input.insert_text("draft");
        assert!(!input.previous());
        assert_eq!(input.text(), "draft");
    }

    #[test]
    fn history_skips_blank_and_repeated_lines() {
        let mut input = Input::default();
        input.remember();
        input.insert_text("same");
        input.remember();
        input.insert_text("same");
        input.remember();
        assert!(input.previous());
        assert_eq!(input.text(), "same");
        assert!(!input.previous());
    }

    #[test]
    fn visible_keeps_the_cursor_inside_the_window() {
        let mut input = typed("abcdefghij");
        let window = input.visible(4);
        assert_eq!(window.text, "hij");
        assert!(window.cursor < 4);
        input.home();
        let window = input.visible(4);
        assert_eq!(window.text, "abcd");
        assert_eq!(window.cursor, 0);
        input.end();
        input.insert('k');
        let window = input.visible(4);
        assert_eq!(window.text, "ijk");
        assert_eq!(window.cursor, 3);
    }

    #[test]
    fn wide_characters_count_two_cells() {
        let input = typed("日本");
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(input.text().as_str()),
            4
        );
        let window = input.visible(3);
        assert_eq!(window.text, "本");
        assert_eq!(window.cursor, 2);
    }
}
