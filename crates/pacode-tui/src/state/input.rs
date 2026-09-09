//! Prompt input: text with a cursor, history, slash-command popup.

use std::collections::VecDeque;

use pacode_render::display_width;
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
#[path = "input_tests.rs"]
mod input_tests;

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
    /// Vertical scroll offset for multi-line prompts (> 6 rows).
    pub input_scroll: usize,
    /// Bounded history of executed bash commands (`BASH_HISTORY_CAP` = 500).
    pub bash_history: VecDeque<String>,
    /// Index into bash history while browsing with ↑/↓ in bash mode.
    pub bash_history_index: Option<usize>,
    /// Draft saved while browsing bash history.
    pub bash_draft: String,
    /// Selected index in `@` reference popup.
    pub at_index: usize,
    /// Whether user explicitly dismissed the `@` reference popup with Esc.
    pub at_closed: bool,
    /// Selected index in bash completion popup.
    pub bash_complete_index: usize,
    /// Whether user explicitly dismissed the bash completion popup with Esc.
    pub bash_complete_closed: bool,
}

pub fn char_to_byte_index(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len())
}

/// Wrap text into lines preserving all characters without trimming.
/// Trailing whitespace (spaces, etc.) occupies terminal cells.
pub fn wrap_text_non_trimming(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let mut lines = Vec::new();

    for raw_line in text.split('\n') {
        let raw_line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if raw_line.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut cur_line = String::new();
        let mut cur_w = 0usize;

        for g in raw_line.graphemes(true) {
            let gw = display_width(g);
            if cur_w + gw > width && cur_w > 0 {
                lines.push(std::mem::take(&mut cur_line));
                cur_w = 0;
            }
            cur_line.push_str(g);
            cur_w += gw;
        }
        lines.push(cur_line);
    }

    lines
}

impl InputState {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn insert_str(&mut self, s: &str) {
        let byte_idx = char_to_byte_index(&self.text, self.cursor);
        self.text.insert_str(byte_idx, s);
        self.cursor += s.chars().count();
        self.at_closed = false;
        self.bash_complete_closed = false;
    }

    pub fn insert_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.insert_str(c.encode_utf8(&mut buf));
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let start_byte = char_to_byte_index(&self.text, self.cursor - 1);
            let end_byte = char_to_byte_index(&self.text, self.cursor);
            self.text.replace_range(start_byte..end_byte, "");
            self.cursor -= 1;
            self.at_closed = false;
            self.bash_complete_closed = false;
        }
    }

    pub fn delete(&mut self) {
        let len = self.text.chars().count();
        if self.cursor < len {
            let start_byte = char_to_byte_index(&self.text, self.cursor);
            let end_byte = char_to_byte_index(&self.text, self.cursor + 1);
            self.text.replace_range(start_byte..end_byte, "");
            self.at_closed = false;
            self.bash_complete_closed = false;
        }
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
        if self.cursor > 0 {
            let chars: Vec<char> = self.text.chars().take(self.cursor).collect();
            let mut i = chars.len();
            while i > 0 && chars[i - 1].is_whitespace() {
                i -= 1;
            }
            while i > 0 && !chars[i - 1].is_whitespace() {
                i -= 1;
            }
            let start_byte = char_to_byte_index(&self.text, i);
            let end_byte = char_to_byte_index(&self.text, self.cursor);
            self.text.replace_range(start_byte..end_byte, "");
            self.cursor = i;
        }
    }

    /// Take the text for submission, push to history, reset.
    pub fn take(&mut self) -> String {
        let taken = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
        self.slash_index = 0;
        self.input_scroll = 0;
        self.bash_history_index = None;
        self.bash_draft.clear();
        self.at_index = 0;
        self.at_closed = false;
        self.bash_complete_index = 0;
        self.bash_complete_closed = false;
        if !taken.trim().is_empty() {
            self.history.push_back(taken.clone());
            if self.history.len() > 100 {
                self.history.pop_front();
            }
        }
        taken
    }

    pub fn bash_history_up(&mut self) {
        if self.bash_history.is_empty() {
            return;
        }
        match self.bash_history_index {
            None => {
                self.bash_draft = self.text.clone();
                let idx = self.bash_history.len() - 1;
                self.bash_history_index = Some(idx);
                self.text = format!("!{}", self.bash_history[idx]);
                self.cursor = self.text.chars().count();
            }
            Some(idx) if idx > 0 => {
                let new_idx = idx - 1;
                self.bash_history_index = Some(new_idx);
                self.text = format!("!{}", self.bash_history[new_idx]);
                self.cursor = self.text.chars().count();
            }
            Some(_) => {}
        }
    }

    pub fn bash_history_down(&mut self) {
        if let Some(idx) = self.bash_history_index {
            if idx + 1 < self.bash_history.len() {
                let new_idx = idx + 1;
                self.bash_history_index = Some(new_idx);
                self.text = format!("!{}", self.bash_history[new_idx]);
                self.cursor = self.text.chars().count();
            } else {
                self.bash_history_index = None;
                self.text = std::mem::take(&mut self.bash_draft);
                self.cursor = self.text.chars().count();
            }
        }
    }

    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                self.draft = self.text.clone();
                let idx = self.history.len() - 1;
                self.history_index = Some(idx);
                self.text = self.history[idx].clone();
                self.cursor = self.text.chars().count();
            }
            Some(idx) if idx > 0 => {
                let new_idx = idx - 1;
                self.history_index = Some(new_idx);
                self.text = self.history[new_idx].clone();
                self.cursor = self.text.chars().count();
            }
            Some(_) => {}
        }
    }

    pub fn history_down(&mut self) {
        if let Some(idx) = self.history_index {
            if idx + 1 < self.history.len() {
                let new_idx = idx + 1;
                self.history_index = Some(new_idx);
                self.text = self.history[new_idx].clone();
                self.cursor = self.text.chars().count();
            } else {
                self.history_index = None;
                self.text = std::mem::take(&mut self.draft);
                self.cursor = self.text.chars().count();
            }
        }
    }

    /// Lines after wrapping at `width` (for the layout), 1..=6.
    pub fn wrapped_lines(&self, width: u16) -> u16 {
        if self.text.is_empty() {
            return 1;
        }
        let text_w = (width as usize).saturating_sub(2).max(1);
        let lines = wrap_text_non_trimming(&self.text, text_w);
        lines.len().clamp(1, 6) as u16
    }

    /// Wrapped lines without clamping, preserving all characters.
    pub fn wrap_lines(&self, width: u16) -> Vec<String> {
        let text_w = (width as usize).saturating_sub(2).max(1);
        wrap_text_non_trimming(&self.text, text_w)
    }

    /// Cursor position as (line, column) after wrapping at `width`.
    pub fn cursor_position(&self, width: u16) -> (u16, u16) {
        if self.cursor == 0 {
            return (0, 2.min(width.saturating_sub(1)));
        }
        let text_w = (width as usize).saturating_sub(2).max(1);
        let before_cursor: String = self.text.chars().take(self.cursor).collect();
        let lines = wrap_text_non_trimming(&before_cursor, text_w);
        let line_idx = lines.len().saturating_sub(1) as u16;
        let last_line = lines.last().map(|s| s.as_str()).unwrap_or("");
        let col = 2 + display_width(last_line);
        (line_idx, (col as u16).min(width.saturating_sub(1)))
    }
}
