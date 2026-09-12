//! Prompt input: text with a cursor, history, slash-command popup.
//!
//! Completion state for the `@` and `!` popups lives here as *data*, not
//! calls: the draw path records which candidates it wants (`want`), the event
//! loop runs the filesystem scan on a blocking thread, and the reply lands in
//! `ready`. Draw and key handlers only ever read `ready` — a miss shows no
//! popup for a frame instead of blocking the frame on a directory walk
//! (§8.1 clean-frame audit).

use std::collections::VecDeque;
use std::path::PathBuf;

use pacode_render::display_width;
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
#[path = "input_tests.rs"]
mod input_tests;

#[cfg(test)]
#[path = "input_delete_tests.rs"]
mod input_delete_tests;

pub const PROMPT_QUEUE_CAP: usize = 16;

/// What an `@` scan was requested for — the candidates are only meaningful
/// for exactly this (query, cursor, cwd).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtKey {
    /// The partially typed path after `@`.
    pub query: String,
    /// Byte cursor into `InputState::text`.
    pub cursor_byte: usize,
    /// Session cwd the scan ran against.
    pub cwd: PathBuf,
}

/// What a `!` scan was requested for — input, cursor, cwd and how much
/// history it consulted (a new recorded command invalidates candidates).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BashKey {
    /// `InputState::text` without the leading `!`.
    pub text: String,
    /// Char cursor minus the `!`.
    pub cursor: usize,
    pub cwd: PathBuf,
    /// `bash_history.len()` at scan time.
    pub history_len: usize,
}

/// Candidates a popup can draw from, plus which scan produced them and which
/// scan is outstanding. `K` is the full scan context; `C` one candidate row.
///
/// Cap: `ready`/`want`/`in_flight` hold one key each, and every scan result
/// overwrites `ready` wholesale — the previous generation is dropped.
/// Owner: `InputState`. Freed with `AppState`.
///
/// `Default` is hand-implemented: the derive would demand `K: Default` +
/// `C: Default`, which the key/candidate types have no business satisfying.
#[derive(Debug)]
pub struct CompletionState<K, C> {
    /// The context of the scan in `ready` — `None` means nothing completed yet.
    pub ready_key: Option<K>,
    /// The last completed scan's candidates (may be legitimately empty).
    pub ready: Vec<C>,
    /// What the draw path needs — drained by the event loop into `in_flight`.
    pub want: Option<K>,
    /// The context of the scan running on a blocking thread.
    pub in_flight: Option<K>,
}

impl<K, C> Default for CompletionState<K, C> {
    fn default() -> Self {
        Self {
            ready_key: None,
            ready: Vec::new(),
            want: None,
            in_flight: None,
        }
    }
}

impl<K: PartialEq + Clone, C> CompletionState<K, C> {
    /// Candidates for `key` when the last completed scan covered exactly it.
    pub fn candidates_for(&self, key: &K) -> Option<&[C]> {
        if self.ready_key.as_ref() == Some(key) {
            Some(&self.ready)
        } else {
            None
        }
    }

    /// Record what the popup wants now. Called from draw — it only mutates
    /// this cache, never runs the scan.
    pub fn set_want(&mut self, want: Option<K>) {
        self.want = want;
    }

    /// Claim `want` for a background scan: returns the key to scan when it is
    /// neither already answered nor already in flight. The caller dispatches
    /// the job and reports back through [`Self::finish_scan`].
    pub fn claim_want(&mut self) -> Option<K> {
        let want = self.want.clone()?;
        if self.in_flight.as_ref() == Some(&want) || self.ready_key.as_ref() == Some(&want) {
            return None;
        }
        self.in_flight = Some(want.clone());
        Some(want)
    }

    /// Fold a finished scan in. `key` is the context it ran for — a stale
    /// reply simply becomes the new `ready`; `candidates_for` keeps it from
    /// being misused for a different context.
    pub fn finish_scan(&mut self, key: K, candidates: Vec<C>) {
        if self.in_flight.as_ref() == Some(&key) {
            self.in_flight = None;
        }
        self.ready_key = Some(key);
        self.ready = candidates;
    }
}

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
    /// Queued prompts submitted while a turn was running (cap `PROMPT_QUEUE_CAP`).
    pub prompt_queue: VecDeque<String>,
    /// `@` path-completion cache — filled by the event loop, read by draw/keys.
    pub at_comp: CompletionState<AtKey, crate::at_complete::AtCandidate>,
    /// `!` bash-completion cache — same contract.
    pub bash_comp: CompletionState<BashKey, crate::bash::complete::Candidate>,
}

pub fn char_to_byte_index(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len())
}

/// Wrap text into lines preserving all characters without trimming.
/// Trailing whitespace (spaces, etc.) occupies terminal cells.
///
/// Word-aware (spec §4): graphemes group into words separated by whitespace
/// runs, and a line wraps at the last word boundary that still fits `width`.
/// Whitespace keeps occupying cells — a run that overflows continues at the
/// next line's start. A word longer than `width` hard-breaks mid-word.
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
        // Non-whitespace run accumulated until the next whitespace grapheme
        // (or the end of the raw line) marks it a complete word.
        let mut word = String::new();
        let mut word_w = 0usize;

        for g in raw_line.graphemes(true) {
            if g.chars().all(char::is_whitespace) {
                flush_word(
                    &mut lines,
                    &mut cur_line,
                    &mut cur_w,
                    &mut word,
                    &mut word_w,
                    width,
                );
                let gw = display_width(g);
                if cur_w + gw > width && cur_w > 0 {
                    lines.push(std::mem::take(&mut cur_line));
                    cur_w = 0;
                }
                cur_line.push_str(g);
                cur_w += gw;
            } else {
                word.push_str(g);
                word_w += display_width(g);
            }
        }
        flush_word(
            &mut lines,
            &mut cur_line,
            &mut cur_w,
            &mut word,
            &mut word_w,
            width,
        );
        lines.push(cur_line);
    }

    lines
}

/// Place a completed word on the current line: whole when it still fits, on a
/// fresh line when it only fits at a boundary, chunked mid-word when it is
/// longer than a whole line.
fn flush_word(
    lines: &mut Vec<String>,
    cur_line: &mut String,
    cur_w: &mut usize,
    word: &mut String,
    word_w: &mut usize,
    width: usize,
) {
    if word.is_empty() {
        return;
    }
    // Wrap at the word boundary first: the word starts on a fresh line.
    if *cur_w + *word_w > width && *cur_w > 0 {
        lines.push(std::mem::take(cur_line));
        *cur_w = 0;
    }
    let taken = std::mem::take(word);
    *word_w = 0;
    for g in taken.graphemes(true) {
        let gw = display_width(g);
        if *cur_w + gw > width && *cur_w > 0 {
            lines.push(std::mem::take(cur_line));
            *cur_w = 0;
        }
        cur_line.push_str(g);
        *cur_w += gw;
    }
}

impl InputState {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn queue_is_full(&self) -> bool {
        self.prompt_queue.len() >= PROMPT_QUEUE_CAP
    }

    pub fn queue_push(&mut self, text: String) -> bool {
        if self.prompt_queue.len() < PROMPT_QUEUE_CAP {
            self.prompt_queue.push_back(text);
            true
        } else {
            false
        }
    }

    pub fn queue_pop_back(&mut self) -> Option<String> {
        self.prompt_queue.pop_back()
    }

    pub fn queue_clear(&mut self) {
        self.prompt_queue.clear();
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
            self.at_closed = false;
            self.bash_complete_closed = false;
        }
    }

    /// Delete the word after the cursor (alt+delete).
    pub fn delete_word_forward(&mut self) {
        let total_chars = self.text.chars().count();
        if self.cursor < total_chars {
            let chars: Vec<char> = self.text.chars().collect();
            let mut i = self.cursor;
            if !chars[i].is_whitespace() {
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
            } else {
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
            }
            let start_byte = char_to_byte_index(&self.text, self.cursor);
            let end_byte = char_to_byte_index(&self.text, i);
            self.text.replace_range(start_byte..end_byte, "");
            self.at_closed = false;
            self.bash_complete_closed = false;
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
