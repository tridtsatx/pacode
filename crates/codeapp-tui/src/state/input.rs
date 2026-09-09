//! Prompt input: text with a cursor, history, slash-command popup.

use std::collections::VecDeque;

#[derive(Default)]
pub struct InputState {
    /// The text being edited (may contain newlines).
    pub text: String,
    /// Cursor as a char index into `text`.
    pub cursor: usize,
    /// Submitted prompts, newest last (cap 100).
    pub history: VecDeque<String>,
    /// Index into history while browsing with ↑/↓; `None` = editing a new prompt.
    pub history_index: Option<usize>,
    /// Draft saved while browsing history.
    pub draft: String,
    /// Slash popup: matching commands when `text` starts with `/`.
    pub slash_index: usize,
}

impl InputState {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn insert_str(&mut self, s: &str) {
        let _ = s;
        todo!("InputState::insert_str")
    }

    pub fn insert_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.insert_str(c.encode_utf8(&mut buf));
    }

    pub fn backspace(&mut self) {
        todo!("InputState::backspace")
    }

    pub fn delete(&mut self) {
        todo!("InputState::delete")
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        let len = self.text.chars().count();
        if self.cursor < len {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    /// Delete the word before the cursor (ctrl+w).
    pub fn delete_word(&mut self) {
        todo!("InputState::delete_word")
    }

    /// Take the text for submission, push to history, reset.
    pub fn take(&mut self) -> String {
        todo!("InputState::take")
    }

    pub fn history_up(&mut self) {
        todo!("InputState::history_up")
    }

    pub fn history_down(&mut self) {
        todo!("InputState::history_down")
    }

    /// Lines after wrapping at `width` (for the layout), 1..=6.
    pub fn wrapped_lines(&self, width: u16) -> u16 {
        let _ = width;
        todo!("InputState::wrapped_lines")
    }

    /// Cursor position as (line, column) after wrapping at `width`.
    pub fn cursor_position(&self, width: u16) -> (u16, u16) {
        let _ = width;
        todo!("InputState::cursor_position")
    }
}
