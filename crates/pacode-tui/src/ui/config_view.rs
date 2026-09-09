//! Full settings view exposing all fields of `Config`.
//!
//! Replaces the old quick settings bottom picker with a filterable,
//! scrolling two-column view grouped by section.

#[cfg(test)]
#[path = "config_view_tests.rs"]
mod config_view_tests;

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use pacode_config::registry::{self, SettingEntry, SettingKind};
use pacode_render::RenderOptions;
use pacode_types::{Config, ToastLevel};

use crate::keys::Action;
use crate::state::{AppState, Focus};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConfigRow {
    Section(&'static str),
    Setting {
        entry: &'static SettingEntry,
        selectable_index: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineEdit {
    pub buffer: String,
    pub cursor: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigViewState {
    pub query: String,
    pub selected: usize,
    pub scroll_offset: usize,
    pub editing: Option<InlineEdit>,
    pub error: Option<String>,
}

impl ConfigViewState {
    pub fn new(_config: &Config) -> Self {
        Self::default()
    }
}

pub fn filter_entries(query: &str) -> Vec<&'static SettingEntry> {
    let q = query.trim().to_lowercase();
    registry::SETTINGS
        .iter()
        .filter(|entry| {
            if q.is_empty() {
                true
            } else {
                entry.label.to_lowercase().contains(&q)
                    || entry.dotted_key.to_lowercase().contains(&q)
                    || entry.description.to_lowercase().contains(&q)
            }
        })
        .collect()
}

pub fn build_rows(matching: &[&'static SettingEntry]) -> Vec<ConfigRow> {
    let mut rows = Vec::new();
    for &sec in registry::SECTIONS {
        let in_sec: Vec<(usize, &'static SettingEntry)> = matching
            .iter()
            .enumerate()
            .filter(|(_, e)| e.section == sec)
            .map(|(i, &e)| (i, e))
            .collect();
        if !in_sec.is_empty() {
            rows.push(ConfigRow::Section(sec));
            for (selectable_index, entry) in in_sec {
                rows.push(ConfigRow::Setting {
                    entry,
                    selectable_index,
                });
            }
        }
    }
    rows
}

pub fn adjust_scroll(
    selected_selectable: usize,
    rows: &[ConfigRow],
    viewport_height: usize,
    scroll_offset: usize,
) -> usize {
    if viewport_height == 0 || rows.is_empty() {
        return 0;
    }
    let target_row_idx = rows
        .iter()
        .position(|r| match r {
            ConfigRow::Setting {
                selectable_index, ..
            } => *selectable_index == selected_selectable,
            ConfigRow::Section(_) => false,
        })
        .unwrap_or(0);

    if target_row_idx < scroll_offset {
        target_row_idx
    } else if target_row_idx >= scroll_offset + viewport_height {
        target_row_idx + 1 - viewport_height
    } else {
        scroll_offset
    }
}

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    if area.width < 20 || area.height < 6 {
        return;
    }

    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Settings ", opts.theme.bold))
        .border_style(opts.theme.dim);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 10 || inner.height < 4 {
        return;
    }

    let cv = &state.config_view;
    let matching = filter_entries(&cv.query);
    let rows = build_rows(&matching);

    let header_rows = 2; // query + blank
    let footer_rows = 2; // error/more + hints
    let viewport_height = (inner.height as usize).saturating_sub(header_rows + footer_rows);

    let scroll = adjust_scroll(cv.selected, &rows, viewport_height, cv.scroll_offset);

    let mut lines = Vec::new();

    // 0: Filter box
    lines.push(Line::from(vec![
        Span::styled("> ", opts.theme.accent),
        Span::styled(&cv.query, opts.theme.fg),
    ]));
    // 1: Blank separator
    lines.push(Line::default());

    // Rows
    for row in rows.iter().skip(scroll).take(viewport_height) {
        match row {
            ConfigRow::Section(sec) => {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("[{sec}]"), opts.theme.accent.patch(opts.theme.bold)),
                ]));
            }
            ConfigRow::Setting {
                entry,
                selectable_index,
            } => {
                let is_sel = *selectable_index == cv.selected;
                let pointer = if is_sel { opts.glyphs.pointer } else { " " };
                let pointer_style = if is_sel {
                    opts.theme.accent
                } else {
                    opts.theme.fg
                };

                let is_overridden = registry::is_modified(&state.config, entry);
                let cur_val =
                    registry::read_value(&state.config, entry.dotted_key).unwrap_or_default();

                let (right_str, right_style) = if is_sel && cv.editing.is_some() {
                    let buf = cv.editing.as_ref().map(|e| e.buffer.as_str()).unwrap_or("");
                    (
                        format!("[ {buf} ]"),
                        opts.theme.accent.patch(opts.theme.bold),
                    )
                } else if is_overridden {
                    (format!("{cur_val} *"), opts.theme.cyan)
                } else {
                    (cur_val, opts.theme.dim)
                };

                let label_style = if is_sel {
                    opts.theme.selected_bg.patch(opts.theme.bold)
                } else {
                    opts.theme.fg
                };

                let right_w = pacode_render::display_width(&right_str);
                let avail_label_w = (inner.width as usize).saturating_sub(right_w + 4);
                let label_display =
                    pacode_render::truncate_to_width(entry.label, avail_label_w, opts.glyphs.ascii);
                let left_w = 2 + pacode_render::display_width(&label_display);
                let padding = (inner.width as usize).saturating_sub(left_w + right_w);

                lines.push(Line::from(vec![
                    Span::styled(pointer, pointer_style),
                    Span::raw(" "),
                    Span::styled(label_display, label_style),
                    Span::raw(" ".repeat(padding)),
                    Span::styled(right_str, right_style),
                ]));
            }
        }
    }

    // Pad until footer
    while lines.len() < (inner.height as usize).saturating_sub(footer_rows) {
        lines.push(Line::default());
    }

    // Notice / more below line
    let displayed_count = rows.len().saturating_sub(scroll).min(viewport_height);
    let hidden_below = rows.len().saturating_sub(scroll + displayed_count);

    if let Some(ref err) = cv.error {
        lines.push(Line::from(Span::styled(
            format!("error: {err}"),
            opts.theme.red.patch(opts.theme.bold),
        )));
    } else if hidden_below > 0 {
        let arrow = if opts.glyphs.ascii { "v" } else { "↓" };
        lines.push(Line::from(Span::styled(
            format!("{arrow} {hidden_below} more below"),
            opts.theme.faint,
        )));
    } else {
        lines.push(Line::default());
    }

    // Footer hint line
    let sep = if opts.glyphs.ascii { " . " } else { " · " };
    let hint = if cv.editing.is_some() {
        format!("enter confirm{sep}esc cancel")
    } else {
        let arrows = if opts.glyphs.ascii { "^/v" } else { "↑/↓" };
        format!("{arrows} select{sep}enter edit{sep}ctrl+r reset{sep}esc close")
    };
    lines.push(Line::from(Span::styled(hint, opts.theme.faint)));

    frame.render_widget(Paragraph::new(lines), inner);

    // Cursor positioning
    if let Some(ref edit) = cv.editing {
        let target_row_idx = rows
            .iter()
            .position(|r| match r {
                ConfigRow::Setting {
                    selectable_index, ..
                } => *selectable_index == cv.selected,
                ConfigRow::Section(_) => false,
            })
            .unwrap_or(0);
        let row_in_viewport = target_row_idx.saturating_sub(scroll);
        let cursor_y = inner.y + header_rows as u16 + row_in_viewport as u16;
        let buf_len = edit.buffer.chars().count();
        let edit_box_w = buf_len + 4; // "[  ]"
        let right_start = inner.x + inner.width.saturating_sub(edit_box_w as u16);
        let cursor_x = right_start + 2 + (edit.cursor as u16).min(edit.buffer.len() as u16);
        frame.set_cursor_position((cursor_x, cursor_y));
    } else {
        let cursor_x = inner.x + 2 + (cv.query.len() as u16).min(inner.width.saturating_sub(3));
        frame.set_cursor_position((cursor_x, inner.y));
    }
}

fn save_and_apply(state: &mut AppState, key: &str, val: pacode_config::toml::Value, now: Instant) {
    if let Err(e) = pacode_config::update_config_value(&state.paths, key, val.clone()) {
        log::warn!("failed to write config key '{key}': {e}");
        state.push_toast(
            ToastLevel::Error,
            "Failed to update config".to_string(),
            Some(e.to_string()),
            now,
        );
    }
    registry::apply_to_config(&mut state.config, key, &val);
    state.dirty = true;
}

pub fn handle_key(state: &mut AppState, key: KeyEvent) -> Vec<Action> {
    let now = Instant::now();

    // Inline edit active
    if state.config_view.editing.is_some() {
        match key.code {
            KeyCode::Esc => {
                state.config_view.editing = None;
                state.config_view.error = None;
                state.dirty = true;
            }
            KeyCode::Enter => {
                let matching = filter_entries(&state.config_view.query);
                if let Some(&entry) = matching.get(state.config_view.selected) {
                    let buf = state
                        .config_view
                        .editing
                        .as_ref()
                        .map(|e| e.buffer.clone())
                        .unwrap_or_default();
                    match registry::validate_candidate(entry, &buf) {
                        Ok(toml_val) => {
                            save_and_apply(state, entry.dotted_key, toml_val, now);
                            state.config_view.editing = None;
                            state.config_view.error = None;
                        }
                        Err(err) => {
                            state.config_view.error = Some(err.to_string());
                            state.dirty = true;
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(ref mut edit) = state.config_view.editing {
                    if edit.cursor > 0 && edit.cursor <= edit.buffer.len() {
                        edit.buffer.remove(edit.cursor - 1);
                        edit.cursor -= 1;
                    } else {
                        edit.buffer.pop();
                        edit.cursor = edit.buffer.len();
                    }
                    state.config_view.error = None;
                    state.dirty = true;
                }
            }
            KeyCode::Left => {
                if let Some(ref mut edit) = state.config_view.editing {
                    edit.cursor = edit.cursor.saturating_sub(1);
                    state.dirty = true;
                }
            }
            KeyCode::Right => {
                if let Some(ref mut edit) = state.config_view.editing {
                    if edit.cursor < edit.buffer.len() {
                        edit.cursor += 1;
                    }
                    state.dirty = true;
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(ref mut edit) = state.config_view.editing {
                    if edit.cursor <= edit.buffer.len() {
                        edit.buffer.insert(edit.cursor, c);
                        edit.cursor += 1;
                    } else {
                        edit.buffer.push(c);
                        edit.cursor = edit.buffer.len();
                    }
                    state.config_view.error = None;
                    state.dirty = true;
                }
            }
            _ => {}
        }
        return vec![];
    }

    // Normal navigation / query filtering
    let matching = filter_entries(&state.config_view.query);
    let count = matching.len();

    let is_prev = key.code == KeyCode::Up
        || (key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL))
        || (key.code == KeyCode::Char('k')
            && state.config_view.query.is_empty()
            && key.modifiers.is_empty());

    let is_next = key.code == KeyCode::Down
        || (key.code == KeyCode::Char('n') && key.modifiers.contains(KeyModifiers::CONTROL))
        || (key.code == KeyCode::Char('j')
            && state.config_view.query.is_empty()
            && key.modifiers.is_empty());

    if is_prev && state.config_view.selected > 0 {
        state.config_view.selected -= 1;
        state.dirty = true;
        return vec![];
    }

    if is_next && count > 0 && state.config_view.selected + 1 < count {
        state.config_view.selected += 1;
        state.dirty = true;
        return vec![];
    }

    match key.code {
        KeyCode::Esc => {
            state.focus = Focus::Normal;
            state.dirty = true;
        }
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(&entry) = matching.get(state.config_view.selected) {
                if let Err(e) = pacode_config::remove_config_value(&state.paths, entry.dotted_key) {
                    log::warn!("failed to remove config key '{}': {e}", entry.dotted_key);
                    state.push_toast(
                        ToastLevel::Error,
                        "Failed to remove config".to_string(),
                        Some(e.to_string()),
                        now,
                    );
                }
                registry::reset_in_config(&mut state.config, entry.dotted_key);
                state.config_view.error = None;
                state.dirty = true;
            }
        }
        KeyCode::Enter => {
            if let Some(&entry) = matching.get(state.config_view.selected) {
                match entry.kind {
                    SettingKind::Bool => {
                        let cur = registry::read_value(&state.config, entry.dotted_key)
                            .unwrap_or_default();
                        let next = cur != "true";
                        let toml_val = pacode_config::toml::Value::Boolean(next);
                        save_and_apply(state, entry.dotted_key, toml_val, now);
                    }
                    SettingKind::Enum { options } => {
                        let cur = registry::read_value(&state.config, entry.dotted_key)
                            .unwrap_or_default();
                        let cur_idx = options.iter().position(|&o| o == cur).unwrap_or(0);
                        let next_opt = options[(cur_idx + 1) % options.len()];
                        let toml_val = pacode_config::toml::Value::String(next_opt.to_string());
                        save_and_apply(state, entry.dotted_key, toml_val, now);
                    }
                    SettingKind::Integer { .. }
                    | SettingKind::Float { .. }
                    | SettingKind::String => {
                        let cur = registry::read_value(&state.config, entry.dotted_key)
                            .unwrap_or_default();
                        let len = cur.chars().count();
                        state.config_view.editing = Some(InlineEdit {
                            buffer: cur,
                            cursor: len,
                        });
                        state.config_view.error = None;
                        state.dirty = true;
                    }
                    SettingKind::Action { command } => {
                        return crate::commands::execute(state, command);
                    }
                }
            }
        }
        KeyCode::Backspace => {
            state.config_view.query.pop();
            state.config_view.selected = 0;
            state.config_view.scroll_offset = 0;
            state.config_view.error = None;
            state.dirty = true;
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            state.config_view.query.push(c);
            state.config_view.selected = 0;
            state.config_view.scroll_offset = 0;
            state.config_view.error = None;
            state.dirty = true;
        }
        _ => {}
    }

    vec![]
}
