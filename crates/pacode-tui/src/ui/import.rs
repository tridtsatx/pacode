//! Import MCP servers and skills overlay picker.

use std::path::Path;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use pacode_import::{
    ApplyTarget, Discovery, EntryStatus, ImportSource, PlanEntry, apply, check_status,
};
use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::ToastLevel;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::keys::Action;
use crate::state::AppState;

#[cfg(test)]
#[path = "import_tests.rs"]
mod import_tests;

/// A row in the import picker overlay.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportRow {
    pub source: ImportSource,
    pub entry: PlanEntry,
    pub checked: bool,
    pub already_imported: bool,
}

/// Mutable state for the import picker overlay.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportOverlayState {
    pub rows: Vec<ImportRow>,
    pub selected: usize,
    pub scroll: usize,
    /// Set by the key handler when the overlay should be dismissed; the caller
    /// owns `state.focus` and puts the overlay back only while this is false.
    pub closed: bool,
}

impl ImportOverlayState {
    /// Discover servers and skills and initialize the overlay state against current config.
    pub fn discover(home: &Path, cwd: &Path, paths: &pacode_config::Paths) -> Self {
        let discovery = pacode_import::discover(home, cwd, ImportSource::all());
        let config_dir = paths.config_file.parent().unwrap_or(Path::new("."));
        let target = ApplyTarget::new(&paths.config_file, config_dir.join("skills"), false);
        Self::new_from_discovery(discovery, &target)
    }

    /// Build overlay state from a discovery result and apply target.
    pub fn new_from_discovery(discovery: Discovery, target: &ApplyTarget) -> Self {
        let mut rows = Vec::new();
        for &source in ImportSource::all() {
            for mcp in discovery.mcp.iter().filter(|m| m.source == source) {
                let entry = PlanEntry::mcp(mcp.clone());
                let status = check_status(&entry, target);
                let already_imported = status != EntryStatus::New;
                rows.push(ImportRow {
                    source,
                    entry,
                    checked: true,
                    already_imported,
                });
            }
            for skill in discovery.skills.iter().filter(|s| s.source == source) {
                let entry = PlanEntry::skill(skill.clone());
                let status = check_status(&entry, target);
                let already_imported = status != EntryStatus::New;
                rows.push(ImportRow {
                    source,
                    entry,
                    checked: true,
                    already_imported,
                });
            }
        }
        Self {
            rows,
            selected: 0,
            scroll: 0,
            closed: false,
        }
    }

    /// Move selection up one row.
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Move selection down one row.
    pub fn move_down(&mut self) {
        if !self.rows.is_empty() && self.selected + 1 < self.rows.len() {
            self.selected += 1;
        }
    }

    /// Toggle the checkbox on the selected row (skipped if already imported).
    pub fn toggle(&mut self) {
        if let Some(row) = self.rows.get_mut(self.selected)
            && !row.already_imported
        {
            row.checked = !row.checked;
        }
    }

    /// Toggle all non-imported rows: if any unchecked -> check all, else uncheck all.
    pub fn toggle_all(&mut self) {
        let any_unchecked = self.rows.iter().any(|r| !r.already_imported && !r.checked);
        for row in &mut self.rows {
            if !row.already_imported {
                row.checked = any_unchecked;
            }
        }
    }

    /// Returns plan entries that are checked and not already imported.
    pub fn checked_entries(&self) -> Vec<PlanEntry> {
        self.rows
            .iter()
            .filter(|r| r.checked && !r.already_imported)
            .map(|r| r.entry.clone())
            .collect()
    }
}

/// Handle keyboard events for the import picker overlay.
pub fn handle_key(
    state: &mut AppState,
    import_state: &mut ImportOverlayState,
    key: KeyEvent,
    now: Instant,
) -> Vec<Action> {
    match key.code {
        KeyCode::Esc => {
            import_state.closed = true;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            import_state.move_up();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            import_state.move_down();
        }
        KeyCode::Char(' ') => {
            import_state.toggle();
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            import_state.toggle_all();
        }
        KeyCode::Enter => {
            let plan = import_state.checked_entries();
            let config_dir = state.paths.config_file.parent().unwrap_or(Path::new("."));
            let target =
                ApplyTarget::new(&state.paths.config_file, config_dir.join("skills"), false);
            let report = apply(&plan, &target);

            let s = report.imported_servers;
            let sk = report.imported_skills;
            state.push_toast(
                ToastLevel::Success,
                format!("imported {s} servers, {sk} skills"),
                None,
                now,
            );
            if let Ok(new_cfg) = pacode_config::load(&state.paths) {
                state.config = new_cfg;
            }
            import_state.closed = true;
        }
        _ => {}
    }
    vec![]
}

/// Draw the import overlay.
pub fn draw(frame: &mut Frame, area: Rect, state: &ImportOverlayState, opts: &RenderOptions) {
    let block = crate::ui::overlays::menu_block(
        "Import MCP Servers & Skills",
        " space toggle · a all · enter apply · esc close ",
        opts,
    );

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.rows.is_empty() {
        let lines = crate::ui::overlays::empty_lines(
            "No MCP servers or skills found to import",
            "nothing importable in the supported config locations",
            opts,
        );
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let mut lines = Vec::new();
    let mut row_line_indices = vec![0; state.rows.len()];

    for &source in ImportSource::all() {
        let source_rows: Vec<(usize, &ImportRow)> = state
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.source == source)
            .collect();

        if source_rows.is_empty() {
            continue;
        }

        if !lines.is_empty() {
            lines.push(Line::default());
        }

        let label = source.label();
        let header_str = format!("── {label} ──");
        lines.push(Line::from(Span::styled(header_str, opts.theme.accent)));

        for (idx, row) in source_rows {
            row_line_indices[idx] = lines.len();
            let is_sel = idx == state.selected;
            let pointer = if is_sel { opts.glyphs.pointer } else { " " };
            let pointer_style = if is_sel {
                opts.theme.accent
            } else {
                opts.theme.fg
            };

            let (box_str, box_style, name_style, kind_style) = if row.already_imported {
                ("[x]", opts.theme.dim, opts.theme.dim, opts.theme.dim)
            } else if row.checked {
                ("[x]", opts.theme.green, opts.theme.bold, opts.theme.cyan)
            } else {
                (
                    "[ ]",
                    opts.theme.dim,
                    if is_sel {
                        opts.theme.bold
                    } else {
                        opts.theme.fg
                    },
                    opts.theme.dim,
                )
            };

            let kind_tag = row.entry.kind_str();
            let mut row_spans = vec![
                Span::styled(pointer, pointer_style),
                Span::raw(" "),
                Span::styled(box_str, box_style),
                Span::raw(" "),
                Span::styled(format!("[{kind_tag}]"), kind_style),
                Span::raw(" "),
                Span::styled(row.entry.name().to_string(), name_style),
            ];

            if row.already_imported {
                row_spans.push(Span::styled(" (already imported)", opts.theme.dim));
            }

            let detail = row.entry.detail();
            if !detail.is_empty() {
                let used_w: usize = row_spans
                    .iter()
                    .map(|s| pacode_render::display_width(&s.content))
                    .sum();
                let avail_w = (inner.width as usize).saturating_sub(used_w + 3);
                if avail_w > 4 {
                    let truncated = truncate_to_width(&detail, avail_w, opts.glyphs.ascii);
                    row_spans.push(Span::raw("  "));
                    row_spans.push(Span::styled(truncated, opts.theme.dim));
                }
            }

            let line = Line::from(row_spans);
            if is_sel {
                lines.push(crate::ui::overlays::selected_row(line, inner.width, opts));
            } else {
                lines.push(line);
            }
        }
    }

    let max_rows = inner.height as usize;
    let start = if max_rows > 0 && !state.rows.is_empty() {
        let sel_line = row_line_indices.get(state.selected).copied().unwrap_or(0);
        if sel_line >= max_rows {
            sel_line + 1 - max_rows
        } else {
            0
        }
    } else {
        0
    };

    let visible_lines: Vec<Line> = lines.into_iter().skip(start).take(max_rows).collect();

    frame.render_widget(Paragraph::new(visible_lines), inner);
}
