//! What the settings overlay is looking at, and what a key press asks the host
//! to do about it.
//!
//! The overlay is a *view* of [`registry::SETTINGS`]: there is no list to keep
//! in sync, so a setting the registry gains appears here by itself and a
//! setting it loses disappears. The model holds the selection (section,
//! highlighted row, search query) and the inline editor, and returns a
//! [`SettingsOutcome`] instead of writing anything — the config file and the
//! live `Config` are the host's to touch, and a value the registry rejects is
//! reported on the description line rather than silently reverted.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_config::registry::{self, SettingEntry, SettingKind};
use pacode_config::toml;
use pacode_types::Config;

#[cfg(test)]
#[path = "model_tests.rs"]
mod model_tests;

/// What one key press wants the host to carry out.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsOutcome {
    /// The key was the overlay's to handle; nothing leaves it.
    Consumed,
    /// Write `value` under `key` and apply it to the live config.
    Commit {
        key: &'static str,
        value: toml::Value,
    },
    /// Drop `key` from the config file, back to the registry default.
    Reset { key: &'static str },
    /// Run a slash command — an `Action` row holds a command, not a value.
    Run { command: &'static str },
    /// Leave the overlay. `Esc` only reaches it once the search is cleared.
    Close,
}

/// Section names in display order.
///
/// [`registry::SECTIONS`] fixes the order, but a section that only ever appears
/// in `SETTINGS` would then be unreachable in the overlay, so anything the
/// order list misses is appended — sorted, so the tail is stable rather than
/// dependent on the table's layout. Sections that declare no settings at all
/// are dropped: an empty tab is a dead end.
pub fn section_order() -> Vec<&'static str> {
    let mut order: Vec<&'static str> = Vec::with_capacity(registry::SECTIONS.len());
    for &section in registry::SECTIONS {
        if has_entries(section) {
            order.push(section);
        }
    }
    let mut extra: Vec<&'static str> = registry::SETTINGS
        .iter()
        .map(|entry| entry.section)
        .filter(|section| !order.contains(section))
        .collect();
    extra.sort_unstable();
    extra.dedup();
    order.extend(extra);
    order
}

fn has_entries(section: &str) -> bool {
    registry::SETTINGS.iter().any(|e| e.section == section)
}

/// The registry entries one section owns, in table order.
pub fn section_entries(section: &str) -> impl Iterator<Item = &'static SettingEntry> {
    registry::SETTINGS.iter().filter(move |e| e.section == section)
}

/// Registry entries matching `query`, across every section, in table order.
///
/// The label, the dotted key and the description all match, case-insensitively
/// and without allocating per entry: this runs on every keystroke of a search.
/// An empty query matches nothing — the caller shows the active section's own
/// rows instead, with the section names beside them.
pub fn search_matches(query: &str) -> Vec<&'static SettingEntry> {
    let needle = query.trim();
    if needle.is_empty() {
        return Vec::new();
    }
    registry::SETTINGS
        .iter()
        .filter(|entry| {
            entry.label.contains_ignore_ascii_case(needle)
                || entry.dotted_key.contains_ignore_ascii_case(needle)
                || entry.description.contains_ignore_ascii_case(needle)
        })
        .collect()
}

/// Where `section` sits in `order`; unknown names fall back to the first
/// section, which is also where a highlight with no section lands.
pub fn section_index(order: &[&'static str], section: &str) -> usize {
    order.iter().position(|&s| s == section).unwrap_or(0)
}

/// Selection, search query and inline editor for the settings overlay.
///
/// The scroll offset is sticky rather than derived: it survives a resize and
/// only moves on a page key, while [`SettingsState::window`] re-clamps it to
/// the space actually available on every frame.
pub struct SettingsState {
    section: usize,
    /// The section `section` names, resolved once per move: the row lookup runs
    /// per visible row per frame, and re-deriving the order list each time
    /// would allocate on every one of them.
    section_name: &'static str,
    cursor: usize,
    scroll: usize,
    query: String,
    matches: Vec<&'static SettingEntry>,
    edit: Option<Edit>,
    error: Option<String>,
}

impl SettingsState {
    pub fn new() -> Self {
        let order = section_order();
        Self {
            section: 0,
            section_name: order.first().copied().unwrap_or(""),
            cursor: 0,
            scroll: 0,
            query: String::new(),
            matches: Vec::new(),
            edit: None,
            error: None,
        }
    }
}

impl Default for SettingsState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct Edit {
    buffer: String,
    cursor: usize,
}

impl SettingsState {
    /// Index into [`section_order`] of the selected section.
    pub fn section(&self) -> usize {
        self.section
    }

    /// The highlighted row, counted within [`SettingsState::row_count`].
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The search query; empty when no search is running.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Why the last value was refused, if the last value was refused. Shown on
    /// the description line until the highlight moves.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The inline editor's buffer and cursor, when a value is being edited.
    pub fn editing(&self) -> Option<(&str, usize)> {
        self.edit.as_ref().map(|e| (e.buffer.as_str(), e.cursor))
    }

    /// Rows the right column holds: the search hits while a query is running,
    /// the selected section's own entries otherwise.
    pub fn row_count(&self) -> usize {
        if self.query.is_empty() {
            section_entries(self.section_name).count()
        } else {
            self.matches.len()
        }
    }

    /// The row at `index`, or `None` past the end.
    pub fn row(&self, index: usize) -> Option<&'static SettingEntry> {
        if self.query.is_empty() {
            section_entries(self.section_name).nth(index)
        } else {
            self.matches.get(index).copied()
        }
    }

    /// The section whose rows are on screen.
    pub fn section_name(&self) -> &'static str {
        self.section_name
    }


    /// The half-open range of section names a body `height` rows tall shows.
    ///
    /// The section list keeps no scroll offset of its own: it is short, and a
    /// window that starts at the top and grows with the highlight scrolls the
    /// way a list is expected to.
    pub fn section_window(&self, count: usize, height: usize) -> Range<usize> {
        visible_range(0, self.section, count, height)
    }

    /// The half-open range of rows a body `height` rows tall shows, with the
    /// highlight kept inside and `scroll` re-clamped to what fits.
    pub fn window(&self, total: usize, height: usize) -> Range<usize> {
        visible_range(self.scroll, self.cursor, total, height)
    }

    fn move_section(&mut self, delta: isize) {
        let count = section_order().len();
        if count == 0 {
            return;
        }
        let next = (self.section as isize + delta).rem_euclid(count as isize) as usize;
        self.goto_section(next);
        self.cursor = 0;
        self.scroll = 0;
        // A search spans sections, so a tab move has to land on one of this
        // section's hits — otherwise the section column would follow the tabs
        // while the rows stayed where they were.
        if let Some(first) = self.first_row_of(self.section_name) {
            self.cursor = first;
            self.scroll = first;
        }
        self.error = None;
    }

    /// Move the selection onto a section, keeping the index and the name it
    /// resolves to in step.
    fn goto_section(&mut self, index: usize) {
        let order = section_order();
        self.section = index.min(order.len().saturating_sub(1));
        self.section_name = order.get(self.section).copied().unwrap_or("");
    }

    fn move_cursor(&mut self, delta: isize, total: usize) {
        if total == 0 {
            return;
        }
        let last = total - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last as isize) as usize;
        self.error = None;
    }

    fn first_row_of(&self, section: &str) -> Option<usize> {
        if self.query.is_empty() {
            return None;
        }
        self.matches.iter().position(|e| e.section == section)
    }

    fn type_char(&mut self, c: char) {
        self.query.push(c);
        self.matches = search_matches(&self.query);
        self.cursor = 0;
        self.scroll = 0;
        if let Some(first) = self.matches.first() {
            self.goto_section(section_index(&section_order(), first.section));
        }
        self.error = None;
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.matches.clear();
        self.cursor = 0;
        self.scroll = 0;
        self.error = None;
    }

    fn pop_query(&mut self) {
        self.query.pop();
        self.matches.clear();
        if !self.query.is_empty() {
            self.matches = search_matches(&self.query);
        }
        self.cursor = 0;
        self.scroll = 0;
        if let Some(first) = self.matches.first() {
            self.goto_section(section_index(&section_order(), first.section));
        }
        self.error = None;
    }

    /// `ctrl+r` — back to the registry default. An `Action` row is a command,
    /// not a value; removing `mcp.servers` or `keys` would wipe a whole table,
    /// so the key refuses them.
    fn reset_selected(&mut self) -> SettingsOutcome {
        let Some(entry) = self.row(self.cursor) else {
            return SettingsOutcome::Consumed;
        };
        if matches!(entry.kind, SettingKind::Action { .. }) {
            return SettingsOutcome::Consumed;
        }
        self.error = None;
        SettingsOutcome::Reset {
            key: entry.dotted_key,
        }
    }

    /// `Enter`/`Space` on the highlighted row: a bool flips, an enum steps to
    /// its next option (wrapping past the last one), a number or a string opens
    /// the inline editor, an action runs its command.
    fn activate(&mut self, config: &Config) -> SettingsOutcome {
        let Some(entry) = self.row(self.cursor) else {
            return SettingsOutcome::Consumed;
        };
        self.error = None;
        match entry.kind {
            SettingKind::Bool => {
                let on = current_value(config, entry) == "true";
                SettingsOutcome::Commit {
                    key: entry.dotted_key,
                    value: toml::Value::Boolean(!on),
                }
            }
            SettingKind::Enum { options } => {
                if options.is_empty() {
                    return SettingsOutcome::Consumed;
                }
                let current = current_value(config, entry);
                let index = options.iter().position(|o| *o == current).unwrap_or(0);
                let next = options[(index + 1) % options.len()];
                SettingsOutcome::Commit {
                    key: entry.dotted_key,
                    value: toml::Value::String(next.to_string()),
                }
            }
            SettingKind::Integer { .. } | SettingKind::Float { .. } | SettingKind::String => {
                let buffer = current_value(config, entry);
                let cursor = buffer.chars().count();
                self.edit = Some(Edit { buffer, cursor });
                SettingsOutcome::Consumed
            }
            SettingKind::Action { command } => SettingsOutcome::Run { command },
        }
    }

    /// Keys while the inline editor is open. The editor owns the keyboard:
    /// arrows move inside the buffer and `Esc` abandons it, so a search can
    /// not start on top of a half-written value.
    fn on_edit_key(&mut self, key: KeyEvent) -> SettingsOutcome {
        if key.code == KeyCode::Enter {
            let Some(edit) = self.edit.take() else {
                return SettingsOutcome::Consumed;
            };
            return self.commit_buffer(edit.buffer);
        }
        if key.code == KeyCode::Esc {
            self.edit = None;
            self.error = None;
            return SettingsOutcome::Consumed;
        }
        let Some(edit) = self.edit.as_mut() else {
            return SettingsOutcome::Consumed;
        };
        let length = edit.buffer.chars().count();
        match key.code {
            KeyCode::Backspace if edit.cursor > 0 => {
                let byte = char_byte(&edit.buffer, edit.cursor - 1);
                edit.buffer.remove(byte);
                edit.cursor -= 1;
            }
            KeyCode::Delete if edit.cursor < length => {
                let byte = char_byte(&edit.buffer, edit.cursor);
                edit.buffer.remove(byte);
            }
            KeyCode::Left => edit.cursor = edit.cursor.saturating_sub(1),
            KeyCode::Right => edit.cursor = (edit.cursor + 1).min(length),
            KeyCode::Home => edit.cursor = 0,
            KeyCode::End => edit.cursor = length,
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                edit.buffer.clear();
                edit.cursor = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let byte = char_byte(&edit.buffer, edit.cursor);
                edit.buffer.insert(byte, c);
                edit.cursor += 1;
            }
            _ => {}
        }
        SettingsOutcome::Consumed
    }

    /// Validate the edited buffer through the registry. A refusal keeps the
    /// editor open with the reason on the description line and leaves the
    /// config untouched: silently reverting a number the user meant to change
    /// is how a setting gets abandoned instead of fixed.
    fn commit_buffer(&mut self, buffer: String) -> SettingsOutcome {
        let Some(entry) = self.row(self.cursor) else {
            return SettingsOutcome::Consumed;
        };
        match registry::validate_candidate(entry, &buffer) {
            Ok(value) => {
                self.error = None;
                SettingsOutcome::Commit {
                    key: entry.dotted_key,
                    value,
                }
            }
            Err(err) => {
                let cursor = buffer.chars().count();
                self.edit = Some(Edit { buffer, cursor });
                self.error = Some(err.to_string());
                SettingsOutcome::Consumed
            }
        }
    }
}

/// The half-open range of `total` rows a `height`-row body shows, with `cursor`
/// inside it and `scroll` re-clamped to what fits. `scroll` is sticky: the
/// window only moves when the highlight would fall outside it, so a terminal
/// that grew or shrank lands on the same rows.
pub fn visible_range(scroll: usize, cursor: usize, total: usize, height: usize) -> Range<usize> {
    if total == 0 || height == 0 {
        return 0..0;
    }
    let height = height.min(total);
    let start = scroll.min(total - height);
    let start = if cursor < start {
        cursor
    } else if cursor >= start + height {
        cursor + 1 - height
    } else {
        start
    };
    start..start + height
}

/// Byte offset of the `index`-th character of `s`.
fn char_byte(s: &str, index: usize) -> usize {
    s.char_indices().nth(index).map_or(s.len(), |(b, _)| b)
}

fn current_value(config: &Config, entry: &SettingEntry) -> String {
    registry::read_value(config, entry.dotted_key).unwrap_or_default()
}

/// One key press against the overlay.
///
/// `rows_visible` is the body height the host measured: it is what makes a
/// page key a page. `config` is the live one, so a toggle cycles from what the
/// user actually has rather than from the registry default.
pub fn on_key(
    state: &mut SettingsState,
    key: KeyEvent,
    config: &Config,
    rows_visible: usize,
) -> SettingsOutcome {
    if state.editing().is_some() {
        return state.on_edit_key(key);
    }
    let total = state.row_count();
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match key.code {
        // Two-stage `Esc`: a running search is cleared first, and only an
        // overlay with nothing left to clear closes. One `Esc` must never both
        // drop the filter and leave.
        KeyCode::Esc => {
            if state.query().is_empty() {
                SettingsOutcome::Close
            } else {
                state.clear_query();
                SettingsOutcome::Consumed
            }
        }
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.reset_selected()
        }
        // `Space` is the second half of "change this setting", so it only types
        // a space once a search is running.
        KeyCode::Char(' ') if plain && state.query().is_empty() => state.activate(config),
        KeyCode::Char(c) if plain => {
            state.type_char(c);
            SettingsOutcome::Consumed
        }
        KeyCode::Backspace => {
            if !state.query().is_empty() {
                state.pop_query();
            }
            SettingsOutcome::Consumed
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let delta = if key.code == KeyCode::BackTab { -1 } else { 1 };
            state.move_section(delta);
            SettingsOutcome::Consumed
        }
        KeyCode::Left => {
            state.move_section(-1);
            SettingsOutcome::Consumed
        }
        KeyCode::Right => {
            state.move_section(1);
            SettingsOutcome::Consumed
        }
        KeyCode::Up => {
            state.move_cursor(-1, total);
            SettingsOutcome::Consumed
        }
        KeyCode::Down => {
            state.move_cursor(1, total);
            SettingsOutcome::Consumed
        }
        KeyCode::PageUp => {
            let page = rows_visible.max(1);
            state.move_cursor(-(page as isize), total);
            state.scroll = state.scroll.saturating_sub(page);
            SettingsOutcome::Consumed
        }
        KeyCode::PageDown => {
            let page = rows_visible.max(1);
            state.move_cursor(page as isize, total);
            state.scroll += page;
            SettingsOutcome::Consumed
        }
        KeyCode::Home => {
            state.cursor = 0;
            state.scroll = 0;
            state.error = None;
            SettingsOutcome::Consumed
        }
        KeyCode::End => {
            state.cursor = total.saturating_sub(1);
            state.scroll = state.cursor;
            state.error = None;
            SettingsOutcome::Consumed
        }
        KeyCode::Enter => state.activate(config),
        _ => SettingsOutcome::Consumed,
    }
}
