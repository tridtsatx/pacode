//! Overlays drawn over the dialog: background task list (`.`), session picker,
//! rail overlay (Tiny tier), help. Width 78% of the dialog column, top offset 2,
//! rounded border, bold title on the top edge, dim key hints on the bottom edge.
//! Pickers are fuzzy-filtered by the typed query.
//!
//! Shared chrome lives here: `menu_block` draws the border, `selected_row`
//! paints the highlighted row as a full-width bar, `empty_lines` gives every
//! empty list the same two faint lines, and `window_groups` scrolls
//! variable-height rows so the selected one stays fully visible.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::SessionMeta;
use pacode_types::state::TaskStatus;
use pacode_types::time::{format_duration_ms, now_ms};

use crate::commands;
use crate::state::{AppState, Focus, Overlay, PluginsTab};

pub fn draw(frame: &mut Frame, dialog_area: Rect, state: &mut AppState, opts: &RenderOptions) {
    if dialog_area.width < 20 || dialog_area.height < 6 {
        return;
    }

    let is_bg_list = matches!(state.focus, Focus::BgList { .. });
    let is_overlay = matches!(state.focus, Focus::Overlay(_));
    // Bottom pickers (effort/mode/model/config) are drawn by `picker`, not here.
    if (!is_bg_list && !is_overlay) || state.is_bottom_picker() {
        return;
    }

    let w = ((dialog_area.width as u32 * 78) / 100).max(20) as u16;
    let max_h = dialog_area.height.saturating_sub(4).max(6);
    // Cap at the content: an empty picker should not draw a tall bordered void.
    let h = wanted_height(state).min(max_h);
    let x = dialog_area.x + (dialog_area.width.saturating_sub(w)) / 2;
    let y = dialog_area.y + 2;
    let area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, area);

    match &state.focus {
        Focus::BgList { index } => {
            draw_bg_list(frame, area, *index, state, opts);
        }
        Focus::Overlay(overlay) => match overlay {
            Overlay::SessionPicker { query, index } => {
                draw_session_picker(frame, area, query, *index, state, opts);
            }
            Overlay::Files { index } => {
                crate::ui::files::draw(frame, area, *index, state, opts);
            }
            Overlay::McpPicker {
                index,
                servers,
                loading,
            } => {
                crate::ui::mcp::draw(frame, area, *index, servers, *loading, opts);
            }
            Overlay::PluginsPicker {
                index,
                plugins,
                tab,
                market,
                query,
                loading,
                stale,
            } => {
                let view = crate::ui::plugins::PluginsView {
                    index: *index,
                    plugins,
                    tab: *tab,
                    market,
                    query,
                    loading: *loading,
                    stale: *stale,
                    source: &state.config.plugins.marketplace,
                };
                crate::ui::plugins::draw(frame, area, &view, opts);
            }
            Overlay::Import(import_state) => {
                crate::ui::import::draw(frame, area, import_state, opts);
            }
            Overlay::RailOverlay => {
                draw_rail_overlay(frame, area, state, opts);
            }
            Overlay::Help => {
                draw_help(frame, area, opts);
            }
            Overlay::KeysPicker { index, capturing } => {
                crate::ui::keys_overlay::draw(frame, area, *index, *capturing, state, opts);
            }
            Overlay::ConfigPicker => {
                crate::ui::config_view::draw(frame, area, state, opts);
            }
            // Drawn as a bottom picker, not as a centred overlay.
            Overlay::QuestionPicker { .. }
            | Overlay::ModelPicker { .. }
            | Overlay::EffortPicker { .. }
            | Overlay::ModePicker { .. }
            | Overlay::ThemePicker { .. } => {}
        },
        Focus::Normal | Focus::SelectAgent { .. } | Focus::Panel { .. } => {}
    }
}

/// Rows the overlay wants, borders included. Lists cap at a handful of rows so
/// a short picker stays small; help, settings and the rail keep the full height.
fn wanted_height(state: &AppState) -> u16 {
    // `items` rows of `per` lines each, capped at `cap` items, or `empty` lines
    // for the empty state; plus the two border rows.
    fn capped(items: usize, per: usize, empty: usize, cap: usize) -> u16 {
        let rows = if items == 0 {
            empty
        } else {
            items.min(cap) * per
        };
        rows as u16 + 2
    }

    match &state.focus {
        Focus::BgList { .. } => capped(state.rail.tasks.len(), 2, 2, 6),
        Focus::Overlay(overlay) => match overlay {
            Overlay::SessionPicker { query, .. } => {
                // Query line, blank line, then two lines per session.
                2 + capped(filtered_sessions(&state.sessions, query).len(), 2, 2, 5)
            }
            Overlay::Files { .. } => capped(state.files.len(), 1, 2, 12),
            Overlay::McpPicker { servers, .. } => capped(servers.len(), 2, 2, 6),
            Overlay::PluginsPicker {
                tab,
                plugins,
                market,
                ..
            } => {
                // Tab bar, a query line on the Discover tab, then the rows
                // (installed rows are up to three lines, discover rows two).
                let (items, per, cap) = match tab {
                    PluginsTab::Installed => (plugins.len(), 3, 4),
                    PluginsTab::Discover => (market.len(), 2, 6),
                };
                let rows = if items == 0 { 2 } else { items.min(cap) * per };
                let status = match tab {
                    PluginsTab::Installed => 0,
                    PluginsTab::Discover => 1,
                };
                2 + 1 + status + rows as u16
            }
            Overlay::KeysPicker { .. } => {
                capped(crate::binding::Keymap::action_names().len(), 1, 1, 14)
            }
            Overlay::Import(import_state) => {
                // Rows plus a header per present source and a blank separator
                // between groups — exactly what `import::draw` emits.
                let sources = pacode_import::ImportSource::all()
                    .iter()
                    .filter(|s| import_state.rows.iter().any(|r| r.source == **s))
                    .count();
                let lines = if import_state.rows.is_empty() {
                    2
                } else {
                    import_state.rows.len() + sources * 2 - 1
                };
                (lines.min(15) as u16) + 2
            }
            // Settings, the rail and help fill the space they are given.
            Overlay::ConfigPicker | Overlay::RailOverlay | Overlay::Help => u16::MAX,
            Overlay::QuestionPicker { .. }
            | Overlay::ModelPicker { .. }
            | Overlay::EffortPicker { .. }
            | Overlay::ModePicker { .. }
            | Overlay::ThemePicker { .. } => 0,
        },
        Focus::Normal | Focus::SelectAgent { .. } | Focus::Panel { .. } => 0,
    }
}

/// The shared overlay frame: rounded border, bold title on the top edge, dim
/// `key action · key action` hints on the bottom edge.
pub(crate) fn menu_block(title: &str, hint: &str, opts: &RenderOptions) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(Span::styled(format!(" {title} "), opts.theme.bold))
        .title_bottom(Span::styled(hint.to_string(), opts.theme.dim))
        .border_style(opts.theme.dim)
}

/// The selected row: every span keeps its colour over `selected_bg`, padded to
/// the row's full width so the highlight is a bar, not a word. The pointer span
/// stays accent on top of the bar.
pub(crate) fn selected_row(line: Line<'static>, width: u16, opts: &RenderOptions) -> Line<'static> {
    let width = width as usize;
    let used: usize = line
        .spans
        .iter()
        .map(|s| pacode_render::display_width(&s.content))
        .sum();
    let mut spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|s| {
            Span::styled(
                s.content.into_owned(),
                opts.theme.selected_bg.patch(s.style),
            )
        })
        .collect();
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            opts.theme.selected_bg,
        ));
    }
    Line::from(spans)
}

/// Every empty list gets the same two faint lines: what is missing, and what
/// the reader can do about it.
pub(crate) fn empty_lines(primary: &str, hint: &str, opts: &RenderOptions) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(primary.to_string(), opts.theme.dim)),
        Line::from(Span::styled(hint.to_string(), opts.theme.faint)),
    ]
}

/// First index of a window over variable-height row groups such that the
/// selected group is the last fully visible one and the whole window fits the
/// line budget — the same rule `window` uses for fixed-height rows.
pub(crate) fn window_groups(heights: &[usize], sel: usize, budget: usize) -> usize {
    if heights.is_empty() || budget == 0 {
        return 0;
    }
    let sel = sel.min(heights.len() - 1);
    let mut start = sel;
    let mut used = heights[sel];
    while start > 0 {
        let next = heights[start - 1];
        if used + next > budget {
            break;
        }
        used += next;
        start -= 1;
    }
    start
}

/// `2h ago`-style age of a timestamp; `just now` under a minute.
pub(crate) fn age_ago(ts_ms: u64, now: u64) -> String {
    let mins = now.saturating_sub(ts_ms) / 60_000;
    if mins < 1 {
        "just now".to_string()
    } else if mins < 60 {
        format!("{mins}m ago")
    } else if mins < 60 * 24 {
        format!("{}h ago", mins / 60)
    } else {
        format!("{}d ago", mins / (60 * 24))
    }
}

/// `~/rel/path` when under HOME, else the path as written.
fn short_path(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy();
    if let Ok(home) = std::env::var("HOME") {
        let home_path = std::path::Path::new(&home);
        if let Ok(rel) = path.strip_prefix(home_path) {
            return format!("~/{}", rel.display());
        }
    }
    raw.into_owned()
}

fn filtered_sessions<'a>(sessions: &'a [SessionMeta], query: &str) -> Vec<&'a SessionMeta> {
    let q_lower = query.to_lowercase();
    sessions
        .iter()
        .filter(|s| {
            q_lower.is_empty()
                || s.title().to_lowercase().contains(&q_lower)
                || s.id.as_str().to_lowercase().contains(&q_lower)
        })
        .collect()
}

fn draw_bg_list(
    frame: &mut Frame,
    area: Rect,
    selected: usize,
    state: &AppState,
    opts: &RenderOptions,
) {
    let block = menu_block(
        "Background Tasks",
        " enter open · k kill · esc close ",
        opts,
    );

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let tasks = &state.rail.tasks;
    if tasks.is_empty() {
        let lines = empty_lines(
            "No background tasks",
            "jobs the agent runs in the background land here",
            opts,
        );
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let now = now_ms();
    let sep = opts.glyphs.dot_sep;
    // Each task is two rows: the label, then status · progress · duration.
    let heights: Vec<usize> = tasks.iter().map(|_| 2).collect();
    let sel = selected.min(tasks.len() - 1);
    let budget = inner.height as usize;
    let start = window_groups(&heights, sel, budget);

    let mut lines = Vec::new();
    for (i, task) in tasks.iter().enumerate().skip(start) {
        if lines.len() + 2 > budget {
            break;
        }
        let is_sel = i == sel;
        let (sym, sym_style) = match task.status {
            TaskStatus::Running => (opts.glyphs.running, opts.theme.violet),
            TaskStatus::Completed => (opts.glyphs.ok, opts.theme.green),
            TaskStatus::Failed | TaskStatus::Killed => (opts.glyphs.fail, opts.theme.red),
        };
        let label_style = if is_sel {
            opts.theme.bold
        } else {
            opts.theme.fg
        };

        let head = Line::from(vec![
            Span::styled(
                format!("{} ", if is_sel { opts.glyphs.pointer } else { " " }),
                opts.theme.accent,
            ),
            Span::styled(format!("{sym} "), sym_style),
            Span::styled(
                truncate_to_width(&task.label, (inner.width as usize).saturating_sub(8), true),
                label_style,
            ),
        ]);

        let (word, word_style) = match task.status {
            TaskStatus::Running => ("running", opts.theme.violet),
            TaskStatus::Completed => ("done", opts.theme.green),
            TaskStatus::Failed => ("failed", opts.theme.red),
            TaskStatus::Killed => ("killed", opts.theme.red),
        };
        let dur = format_duration_ms(task.duration_ms(now));
        let mut sub = vec![
            Span::raw("    "),
            Span::styled(word, word_style),
            Span::styled(format!("{sep}{dur}"), opts.theme.faint),
        ];
        if let Some(ref p) = task.progress
            && let Some(label) = p.short_label()
        {
            sub.push(Span::styled(format!("{sep}{label}"), opts.theme.faint));
        }
        if task.errors > 0 {
            sub.push(Span::styled(
                format!("{sep}{} err", task.errors),
                opts.theme.red,
            ));
        }
        // "exit 0" after "done" is noise; a non-zero code is the interesting one.
        if let Some(code) = task.exit_code
            && code != 0
        {
            sub.push(Span::styled(format!("{sep}exit {code}"), opts.theme.faint));
        }
        let sub = Line::from(sub);

        if is_sel {
            lines.push(selected_row(head, inner.width, opts));
            lines.push(selected_row(sub, inner.width, opts));
        } else {
            lines.push(head);
            lines.push(sub);
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_session_picker(
    frame: &mut Frame,
    area: Rect,
    query: &str,
    selected: usize,
    state: &AppState,
    opts: &RenderOptions,
) {
    let block = menu_block("Pick Session", " enter resume · esc cancel ", opts);

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let filtered = filtered_sessions(&state.sessions, query);

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("> ", opts.theme.accent),
        Span::styled(query.to_string(), opts.theme.fg),
    ]));
    lines.push(Line::default());

    let budget = (inner.height as usize).saturating_sub(2);
    if filtered.is_empty() {
        let empty = if state.sessions.is_empty() {
            empty_lines("No sessions yet", "this is the first one", opts)
        } else {
            empty_lines(
                "No sessions match",
                "keep typing to filter differently",
                opts,
            )
        };
        lines.extend(empty);
    } else {
        let now = now_ms();
        let sep = opts.glyphs.dot_sep;
        let sel = selected.min(filtered.len() - 1);
        let heights: Vec<usize> = filtered.iter().map(|_| 2).collect();
        let start = window_groups(&heights, sel, budget);

        for (i, s) in filtered.iter().enumerate().skip(start) {
            if lines.len() + 2 > inner.height as usize {
                break;
            }
            let is_sel = i == sel;
            let title_style = if is_sel {
                opts.theme.bold
            } else {
                opts.theme.fg
            };
            let head = Line::from(vec![
                Span::styled(
                    format!("{} ", if is_sel { opts.glyphs.pointer } else { " " }),
                    opts.theme.accent,
                ),
                Span::styled(
                    truncate_to_width(&s.title(), (inner.width as usize).saturating_sub(4), true),
                    title_style,
                ),
            ]);

            let meta = format!(
                "{}{sep}{}{sep}{}",
                s.id,
                short_path(&s.cwd),
                age_ago(s.updated_at_ms, now)
            );
            let sub = Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    truncate_to_width(&meta, (inner.width as usize).saturating_sub(4), true),
                    opts.theme.faint,
                ),
            ]);

            if is_sel {
                lines.push(selected_row(head, inner.width, opts));
                lines.push(selected_row(sub, inner.width, opts));
            } else {
                lines.push(head);
                lines.push(sub);
            }
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_rail_overlay(frame: &mut Frame, area: Rect, state: &mut AppState, opts: &RenderOptions) {
    let block = menu_block("Plan & Agents", " esc close ", opts);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    crate::ui::rail::draw(frame, inner, state, opts);
}

fn draw_help(frame: &mut Frame, area: Rect, opts: &RenderOptions) {
    let block = menu_block("Help & Keybindings", " esc close ", opts);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![
        Line::from(Span::styled("Navigation & Focus:", opts.theme.accent)),
        Line::from("  alt+↓ / alt+↑     Select agents in rail (or ctrl+j/k)"),
        Line::from("  enter             Open panel on agent/task / submit"),
        Line::from("  alt+b             Follow agent (auto-scroll)"),
        Line::from("  alt+f             Show touched files"),
        Line::from("  alt+r             Show plan and agents"),
        Line::from("  .                 Show background tasks (empty prompt)"),
        Line::from("  shift+tab         Cycle permission mode"),
        Line::from("  ctrl+p            Pick / resume session"),
        Line::from("  ctrl+c            Interrupt turn (twice: exit)"),
        Line::from("  r                 Restart MCP server (in /mcp)"),
        Line::from("  enter             Toggle MCP server enabled (in /mcp)"),
        Line::from("  esc               Peel focus layer"),
        Line::default(),
        Line::from(Span::styled("Commands:", opts.theme.accent)),
    ];
    for cmd in commands::all_commands() {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<18}", cmd.usage), opts.theme.cyan),
            Span::styled(cmd.help.to_string(), opts.theme.dim),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_groups_keeps_the_selected_group_fully_visible() {
        // Two-line groups, budget for three groups.
        let heights = vec![2, 2, 2, 2, 2];
        assert_eq!(window_groups(&heights, 0, 6), 0);
        assert_eq!(window_groups(&heights, 2, 6), 0);
        assert_eq!(window_groups(&heights, 3, 6), 1);
        // A taller selected group pulls the window further down.
        let heights = vec![2, 3, 2, 4, 2];
        assert_eq!(window_groups(&heights, 3, 6), 2);
        // A group that alone fills the budget starts at itself.
        let heights = vec![2, 2, 2, 5, 2];
        assert_eq!(window_groups(&heights, 3, 5), 3);
        // Degenerate inputs do not panic.
        assert_eq!(window_groups(&[], 0, 5), 0);
        assert_eq!(window_groups(&heights, 0, 0), 0);
        assert_eq!(window_groups(&heights, 99, 6), 4);
    }

    #[test]
    fn selected_row_pads_to_the_full_width() {
        let opts = RenderOptions::new(40, false);
        let line = Line::from(vec![
            Span::styled("▸ ".to_string(), opts.theme.accent),
            Span::styled("name".to_string(), opts.theme.bold),
        ]);
        let row = selected_row(line, 20, &opts);
        let width: usize = row
            .spans
            .iter()
            .map(|s| pacode_render::display_width(&s.content))
            .sum();
        assert_eq!(width, 20);
        // The pointer keeps the accent colour under the selection bar.
        assert_eq!(row.spans[0].style.fg, opts.theme.accent.fg);
    }

    #[test]
    fn age_ago_reads_like_a_clock() {
        let now = 1_000_000_000_000u64;
        assert_eq!(age_ago(now, now), "just now");
        assert_eq!(age_ago(now - 5 * 60_000, now), "5m ago");
        assert_eq!(age_ago(now - 3 * 3_600_000, now), "3h ago");
        assert_eq!(age_ago(now - 2 * 86_400_000, now), "2d ago");
    }
}
