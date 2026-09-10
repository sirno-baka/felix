//! Small UTF-8 aware command-line editor used by the GUI shell.

use alloc::string::String;

#[derive(Default)]
pub struct LineEditor {
    text: String,
    /// Byte offset, always kept on a UTF-8 boundary.
    cursor: usize,
}

impl LineEditor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn cursor_chars(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    pub fn chars_after_cursor(&self) -> usize {
        self.text[self.cursor..].chars().count()
    }

    pub fn set(&mut self, s: String) {
        self.text = s;
        self.cursor = self.text.len();
    }

    pub fn set_with_cursor(&mut self, s: String, cursor: usize) {
        self.text = s;
        self.cursor = cursor.min(self.text.len());
        while self.cursor > 0 && !self.text.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        core::mem::take(&mut self.text)
    }

    pub fn insert(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn backspace(&mut self) -> bool {
        let Some(prev) = self.prev_boundary() else { return false };
        self.text.drain(prev..self.cursor);
        self.cursor = prev;
        true
    }

    pub fn delete(&mut self) -> bool {
        let Some(next) = self.next_boundary() else { return false };
        self.text.drain(self.cursor..next);
        true
    }

    pub fn left(&mut self) -> bool {
        let Some(prev) = self.prev_boundary() else { return false };
        self.cursor = prev;
        true
    }

    pub fn right(&mut self) -> bool {
        let Some(next) = self.next_boundary() else { return false };
        self.cursor = next;
        true
    }

    pub fn home(&mut self) -> bool {
        if self.cursor == 0 {
            false
        } else {
            self.cursor = 0;
            true
        }
    }

    pub fn end(&mut self) -> bool {
        if self.cursor == self.text.len() {
            false
        } else {
            self.cursor = self.text.len();
            true
        }
    }

    /// Ctrl+U: remove everything before the cursor.
    pub fn kill_before(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.text.drain(..self.cursor);
        self.cursor = 0;
        true
    }

    /// Ctrl+K: remove everything after the cursor.
    pub fn kill_after(&mut self) -> bool {
        if self.cursor == self.text.len() {
            return false;
        }
        self.text.truncate(self.cursor);
        true
    }

    /// Ctrl+W: delete whitespace and the word immediately before the cursor.
    pub fn delete_prev_word(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let original = self.cursor;

        while let Some(prev) = self.prev_boundary() {
            let ch = self.text[prev..self.cursor].chars().next().unwrap_or(' ');
            if !ch.is_whitespace() {
                break;
            }
            self.cursor = prev;
        }
        while let Some(prev) = self.prev_boundary() {
            let ch = self.text[prev..self.cursor].chars().next().unwrap_or(' ');
            if ch.is_whitespace() {
                break;
            }
            self.cursor = prev;
        }
        self.text.drain(self.cursor..original);
        true
    }

    fn prev_boundary(&self) -> Option<usize> {
        if self.cursor == 0 {
            return None;
        }
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        if self.cursor >= self.text.len() {
            return None;
        }
        let ch = self.text[self.cursor..].chars().next()?;
        Some(self.cursor + ch.len_utf8())
    }
}
