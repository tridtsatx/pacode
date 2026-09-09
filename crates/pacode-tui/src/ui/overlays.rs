//! Overlays drawn over the dialog: background task list (`.`), model picker, effort
//! picker, session picker, rail overlay (Tiny tier), help. Width 78% of the dialog
//! column, top offset 2, bordered with a single line, title in the top row, hint row
//! at the bottom. Pickers are fuzzy-filtered by the typed query.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::state::TaskStatus;
use pacode_types::time::{format_duration_ms, now_ms};

use crate::commands;
use crate::state::{AppState, Focus, Overlay};

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
    let h = dialog_area.height.saturating_sub(4).max(6);
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
            Overlay::PluginsPicker { index, plugins } => {
                crate::ui::plugins::draw(frame, area, *index, plugins, opts);
            }
            Overlay::RailOverlay => {
                draw_rail_overlay(frame, area, state, opts);
            }
            Overlay::Help => {
                draw_help(frame, area, opts);
            }
            Overlay::ModelPicker { .. }
            | Overlay::EffortPicker { .. }
            | Overlay::ModePicker { .. }
            | Overlay::ConfigPicker { .. } => {}
        },
        Focus::Normal | Focus::SelectAgent { .. } | Focus::Panel { .. } => {}
    }
}

fn draw_bg_list(
    frame: &mut Frame,
    area: Rect,
    selected: usize,
    state: &AppState,
    opts: &RenderOptions,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Background Tasks ", opts.theme.bold))
        .title_bottom(Span::styled(
            " enter open · k kill · esc close ",
            opts.theme.dim,
        ))
        .border_style(opts.theme.dim);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let tasks = &state.rail.tasks;
    if tasks.is_empty() {
        let msg = Line::from(Span::styled("No background tasks", opts.theme.faint));
        frame.render_widget(Paragraph::new(vec![msg]), inner);
        return;
    }

    let now = now_ms();
    let mut lines = Vec::new();

    for (i, task) in tasks.iter().enumerate().take(inner.height as usize) {
        let is_sel = i == selected;
        let style = if is_sel {
            opts.theme.selected_bg
        } else {
            opts.theme.fg
        };

        let (sym, sym_style) = match task.status {
            TaskStatus::Running => (opts.glyphs.running, opts.theme.violet),
            TaskStatus::Completed => (opts.glyphs.ok, opts.theme.green),
            TaskStatus::Failed | TaskStatus::Killed => (opts.glyphs.fail, opts.theme.red),
        };

        let dur = format_duration_ms(task.duration_ms(now));
        let tail_str = if let Some(ref p) = task.progress {
            p.short_label().unwrap_or(dur)
        } else {
            dur
        };

        let pointer = if is_sel { opts.glyphs.pointer } else { " " };
        lines.push(Line::from(vec![
            Span::styled(pointer, opts.theme.accent),
            Span::raw(" "),
            Span::styled(sym, sym_style),
            Span::raw(" "),
            Span::styled(
                truncate_to_width(&task.label, (inner.width as usize).saturating_sub(18), true),
                style,
            ),
            Span::raw(" "),
            Span::styled(tail_str, opts.theme.faint),
        ]));
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
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Pick Session ", opts.theme.bold))
        .title_bottom(Span::styled(" enter resume · esc cancel ", opts.theme.dim))
        .border_style(opts.theme.dim);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let q_lower = query.to_lowercase();
    let filtered: Vec<_> = state
        .sessions
        .iter()
        .filter(|s| {
            q_lower.is_empty()
                || s.title().to_lowercase().contains(&q_lower)
                || s.id.as_str().to_lowercase().contains(&q_lower)
        })
        .collect();

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("> ", opts.theme.cyan),
        Span::styled(query, opts.theme.bold),
    ]));
    lines.push(Line::default());

    let max_rows = (inner.height as usize).saturating_sub(2);
    for (i, s) in filtered.iter().enumerate().take(max_rows) {
        let is_sel = i == selected;
        let style = if is_sel {
            opts.theme.selected_bg
        } else {
            opts.theme.fg
        };
        let p = if is_sel { opts.glyphs.pointer } else { " " };
        lines.push(Line::from(vec![
            Span::styled(p, opts.theme.accent),
            Span::raw(" "),
            Span::styled(s.title(), style),
            Span::raw("  "),
            Span::styled(s.id.to_string(), opts.theme.faint),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_rail_overlay(frame: &mut Frame, area: Rect, state: &mut AppState, opts: &RenderOptions) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Plan & Agents ", opts.theme.bold))
        .title_bottom(Span::styled(" esc close ", opts.theme.dim))
        .border_style(opts.theme.dim);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    crate::ui::rail::draw(frame, inner, state, opts);
}

fn draw_help(frame: &mut Frame, area: Rect, opts: &RenderOptions) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Help & Keybindings ", opts.theme.bold))
        .title_bottom(Span::styled(" esc close ", opts.theme.dim))
        .border_style(opts.theme.dim);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![
        Line::from(Span::styled("Navigation & Focus:", opts.theme.accent)),
        Line::from("  alt+↓ / alt+↑     Select agents in rail (or ctrl+j/k)"),
        Line::from("  enter             Open panel on agent/task / submit"),
        Line::from("  alt+b             Follow agent (auto-scroll)"),
        Line::from("  alt+f             Show touched files"),
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
