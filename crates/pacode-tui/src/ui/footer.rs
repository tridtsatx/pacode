//! Footer rows (spec §3) and width degradation (spec §8).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::{RenderOptions, display_width, truncate_to_width};
use pacode_types::TranscriptKind;
use pacode_types::state::Mode;
use pacode_types::time::{format_duration_ms, now_ms};

use crate::keys::selectable_agents;
use crate::state::transcript::CellKind;
use crate::state::{AppState, Connection, Focus, PanelTarget};

#[cfg(test)]
#[path = "footer_tests.rs"]
mod footer_tests;

pub fn format_tokens_upper(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub fn format_slots_summary(state: &AppState) -> String {
    let mut s = String::new();
    for i in 0..9 {
        let num = i + 1;
        if i == state.active_slot {
            s.push_str(&format!("[{num}]"));
        } else if state.slots[i].is_some() {
            s.push_str(&format!("{num}"));
        } else {
            s.push('·');
        }
    }
    s
}

pub fn render_slots_spans(state: &AppState, opts: &RenderOptions) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(9);
    for i in 0..9 {
        let num = i + 1;
        if i == state.active_slot {
            spans.push(Span::styled(format!("[{num}]"), opts.theme.accent));
        } else if state.slots[i].is_some() {
            spans.push(Span::styled(format!("{num}"), opts.theme.fg));
        } else {
            spans.push(Span::styled("·", opts.theme.faint));
        }
    }
    spans
}

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let row1 = render_row1(area.width as usize, state, opts);
    let row2 = if area.height > 1 {
        render_row2(area.width as usize, state, opts)
    } else {
        Line::default()
    };

    frame.render_widget(Paragraph::new(vec![row1, row2]), area);
}

fn render_row1(width: usize, state: &AppState, opts: &RenderOptions) -> Line<'static> {
    let mode = state.mode();
    let model_name = state
        .model()
        .map(|m| m.display_name())
        .unwrap_or_else(|| "Gemini 3.8 Flash".to_string());
    let effort_str = state.effort().to_string();

    let show_model_hint = state.config.ui.hints.model;
    let show_effort_hint = state.config.ui.hints.effort;

    let right_hint = if show_model_hint { "/model" } else { "" };
    let right_len = display_width(right_hint);

    let mode_style = match mode {
        Mode::Bypass => opts.theme.red,
        Mode::Plan => opts.theme.yellow,
        Mode::Build | Mode::Auto => opts.theme.cyan,
    };

    let mut left_spans = vec![Span::styled(mode.label(), mode_style)];

    if state.config.ui.vim {
        let (vim_badge, vim_style) = match state.vim.mode {
            crate::state::vim::VimMode::Normal => ("NOR", opts.theme.cyan),
            crate::state::vim::VimMode::Insert => ("INS", opts.theme.green),
            crate::state::vim::VimMode::Visual => ("VIS", opts.theme.yellow),
        };
        left_spans.push(Span::styled(" · ", opts.theme.faint));
        left_spans.push(Span::styled(vim_badge, vim_style));
    }

    if let Some((_, text)) = &state.plugin_status {
        left_spans.push(Span::styled(format!(" · {text}"), opts.theme.dim));
    }

    left_spans.extend([
        Span::styled(" · ", opts.theme.faint),
        Span::styled(model_name, opts.theme.dim),
        Span::styled(" | ", opts.theme.faint),
        Span::styled(effort_str, opts.theme.accent),
    ]);

    if show_effort_hint {
        left_spans.push(Span::styled(" /effort", opts.theme.cyan));
    }

    let left_len: usize = left_spans.iter().map(|s| display_width(&s.content)).sum();

    if left_len + 2 + right_len <= width {
        let spaces = width - left_len - right_len;
        left_spans.push(Span::raw(" ".repeat(spaces)));
        if !right_hint.is_empty() {
            left_spans.push(Span::styled(right_hint, opts.theme.cyan));
        }
        Line::from(left_spans)
    } else {
        let vim_part = if state.config.ui.vim {
            match state.vim.mode {
                crate::state::vim::VimMode::Normal => " · NOR",
                crate::state::vim::VimMode::Insert => " · INS",
                crate::state::vim::VimMode::Visual => " · VIS",
            }
        } else {
            ""
        };
        let mode_label = mode.label();
        let display_name = state.model().map(|m| m.display_name()).unwrap_or_default();
        let effort = state.effort();
        let plain = if let Some((_, text)) = &state.plugin_status {
            format!("{mode_label}{vim_part} · {text} · {display_name} | {effort}")
        } else {
            format!("{mode_label}{vim_part} · {display_name} | {effort}")
        };
        let trunc = truncate_to_width(&plain, width, true);
        Line::from(Span::styled(trunc, opts.theme.dim))
    }
}

fn render_row2_with_right(
    left_spans: Vec<Span<'static>>,
    right_text: String,
    width: usize,
    opts: &RenderOptions,
) -> Line<'static> {
    let left_len: usize = left_spans.iter().map(|s| display_width(&s.content)).sum();
    let right_len = display_width(&right_text);
    let spaces = width.saturating_sub(left_len + right_len);
    let mut out = left_spans;
    out.push(Span::raw(" ".repeat(spaces)));
    out.push(Span::styled(right_text, opts.theme.faint));
    Line::from(out)
}

fn render_row2(width: usize, state: &AppState, opts: &RenderOptions) -> Line<'static> {
    if let Some(t) = state.ctrl_c_at
        && std::time::Instant::now().saturating_duration_since(t)
            <= std::time::Duration::from_secs(2)
    {
        let hint = "ctrl+c again to exit · ctrl+d exit";
        let trunc = truncate_to_width(hint, width, true);
        return Line::from(Span::styled(trunc, opts.theme.accent));
    }

    let pending_perm = state.transcript.cells.iter().find_map(|c| {
        if let CellKind::Item(TranscriptKind::Permission(ref req)) = c.kind {
            Some(req.clone())
        } else {
            None
        }
    });

    let (left_spans, show_context) = match &state.connection {
        Connection::Reconnecting { attempt } => (
            vec![Span::styled(
                format!("reconnecting… (attempt {attempt})"),
                opts.theme.red,
            )],
            true,
        ),
        Connection::Disconnected { reason } => (
            vec![Span::styled(
                format!("disconnected: {reason}"),
                opts.theme.red,
            )],
            false,
        ),
        Connection::Connected => {
            if let Some(req) = pending_perm {
                (
                    vec![
                        Span::styled(
                            format!("{} permission: ", opts.glyphs.chevrons),
                            opts.theme.accent,
                        ),
                        Span::styled(format!("{} · ", req.title), opts.theme.bold),
                        Span::styled("y allow · a session · n deny", opts.theme.cyan),
                    ],
                    true,
                )
            } else {
                match &state.focus {
                    Focus::SelectAgent { index } => {
                        let agents = selectable_agents(state);
                        let total = agents.len();
                        let display_idx = index + 1;
                        let name = agents
                            .get(*index)
                            .and_then(|id| {
                                if id.is_main() {
                                    Some("main")
                                } else {
                                    state.rail.agent(id).map(|a| a.name.as_str())
                                }
                            })
                            .unwrap_or("agent");
                        (
                            vec![
                                Span::styled(format!("{} ", opts.glyphs.chevrons), opts.theme.cyan),
                                Span::styled(
                                    format!("{name} {display_idx}/{total} · "),
                                    opts.theme.bold,
                                ),
                                Span::styled(
                                    "enter open · alt+b follow · esc clear",
                                    opts.theme.dim,
                                ),
                            ],
                            false,
                        )
                    }
                    Focus::Panel { target, follow, .. } => {
                        let name = match target {
                            PanelTarget::Agent(id) => state
                                .rail
                                .agent(id)
                                .map(|a| a.name.as_str())
                                .unwrap_or("agent"),
                            PanelTarget::Task(id) => state
                                .rail
                                .task(id)
                                .map(|t| t.label.as_str())
                                .unwrap_or("task"),
                        };
                        if *follow {
                            let main_id = pacode_types::AgentId::main();
                            let agent_id = match target {
                                PanelTarget::Agent(id) => id,
                                _ => &main_id,
                            };
                            let dur = state
                                .rail
                                .agent(agent_id)
                                .map(|a| format_duration_ms(a.duration_ms(now_ms())))
                                .unwrap_or_default();
                            (
                                vec![
                                    Span::styled("FOLLOW", opts.theme.selected_bg),
                                    Span::raw(" "),
                                    Span::styled(format!("{name} {dur} · "), opts.theme.bold),
                                    Span::styled("pgup pause · alt+b release", opts.theme.dim),
                                ],
                                false,
                            )
                        } else {
                            (
                                vec![
                                    Span::styled(
                                        format!("{} ", opts.glyphs.chevrons),
                                        opts.theme.cyan,
                                    ),
                                    Span::styled(format!("{name} · "), opts.theme.bold),
                                    Span::styled(
                                        "esc back · alt+b follow · s stop",
                                        opts.theme.dim,
                                    ),
                                ],
                                false,
                            )
                        }
                    }
                    Focus::BgList { .. } => {
                        let running = state.rail.running_task_count();
                        let failed = state.rail.failed_unacked_count();
                        (
                            vec![
                                Span::styled(format!("{} ", opts.glyphs.chevrons), opts.theme.cyan),
                                Span::styled(
                                    format!("{running} bg running · {failed} failed · "),
                                    opts.theme.bold,
                                ),
                                Span::styled("enter output · k kill · esc", opts.theme.dim),
                            ],
                            false,
                        )
                    }
                    _ => {
                        let mode = state.mode();
                        let perm_style = match mode {
                            Mode::Bypass => opts.theme.red,
                            Mode::Plan => opts.theme.yellow,
                            Mode::Build | Mode::Auto => opts.theme.cyan,
                        };

                        let tok_str = format_tokens_upper(state.rail.usage.context_tokens as u64);
                        let slots_str = format_slots_summary(state);
                        let right_text = format!("{slots_str} · {tok_str} · ctrl+p");
                        let right_len = display_width(&right_text);

                        let perm_base =
                            format!("{} {}", opts.glyphs.chevrons, mode.permission_line());
                        let plugin_status_str = state
                            .plugin_status
                            .as_ref()
                            .map(|(_, t)| format!(" · {t}"))
                            .unwrap_or_default();
                        let hint = " (shift+tab to cycle)";
                        let counts_or_idle =
                            if state.rail.show_session_stats && state.rail.usage.turns > 0 {
                                let done_count = state
                                    .rail
                                    .agents
                                    .iter()
                                    .filter(|a| !a.status.is_live())
                                    .count();
                                let dur = format_duration_ms(
                                    state
                                        .rail
                                        .usage
                                        .last_activity_ms
                                        .saturating_sub(state.rail.usage.started_at_ms),
                                );
                                format!(" · {done_count} agents done · {dur}")
                            } else {
                                let agents =
                                    state.rail.agents.iter().filter(|a| !a.id.is_main()).count();
                                let bg = state.rail.running_task_count();
                                let mut parts = Vec::new();
                                if agents > 0 {
                                    parts.push(format!("← {agents} agents"));
                                }
                                if bg > 0 {
                                    parts.push(format!("{bg} bg"));
                                }
                                if parts.is_empty() {
                                    String::new()
                                } else {
                                    let joined = parts.join(" · ");
                                    format!(" · {joined}")
                                }
                            };

                        let perm_base_len = display_width(&perm_base);
                        let status_len = display_width(&plugin_status_str);
                        let hint_len = display_width(hint);
                        let counts_len = display_width(&counts_or_idle);

                        let make_spans = |with_status: bool, with_hint: bool, with_counts: bool| {
                            let mut spans = vec![
                                Span::styled(format!("{} ", opts.glyphs.chevrons), perm_style),
                                Span::styled(mode.permission_line(), perm_style),
                            ];
                            if with_status && !plugin_status_str.is_empty() {
                                spans.push(Span::styled(plugin_status_str.clone(), opts.theme.dim));
                            }
                            if with_hint {
                                spans.push(Span::styled(hint, opts.theme.faint));
                            }
                            if with_counts {
                                spans.push(Span::styled(counts_or_idle.clone(), opts.theme.faint));
                            }
                            spans
                        };

                        // Degradation steps:
                        // 1. Full with status + hint + counts + context
                        let full_left_len = perm_base_len + status_len + hint_len + counts_len;
                        if full_left_len + 2 + right_len <= width {
                            return render_row2_with_right(
                                make_spans(true, true, true),
                                right_text,
                                width,
                                opts,
                            );
                        }

                        // 2. Drop hint (keep status)
                        let no_hint_len = perm_base_len + status_len + counts_len;
                        if no_hint_len + 2 + right_len <= width {
                            return render_row2_with_right(
                                make_spans(true, false, true),
                                right_text,
                                width,
                                opts,
                            );
                        }

                        // 3. Drop counts (keep status)
                        let base_status_len = perm_base_len + status_len;
                        if base_status_len + 2 + right_len <= width {
                            return render_row2_with_right(
                                make_spans(true, false, false),
                                right_text,
                                width,
                                opts,
                            );
                        }

                        // 4. Drop context (right)
                        if full_left_len <= width {
                            return Line::from(make_spans(true, true, true));
                        }

                        if no_hint_len <= width {
                            return Line::from(make_spans(true, false, true));
                        }

                        if base_status_len <= width {
                            return Line::from(make_spans(true, false, false));
                        }

                        let plain = format!("{perm_base}{plugin_status_str}");
                        let trunc = truncate_to_width(&plain, width, true);
                        return Line::from(Span::styled(trunc, perm_style));
                    }
                }
            }
        }
    };

    let right_spans = if show_context {
        let tokens = state.rail.usage.context_tokens as u64;
        let mut spans = render_slots_spans(state, opts);
        spans.push(Span::styled(" · ", opts.theme.faint));
        if tokens > 0 {
            let tok_str = format_tokens_upper(tokens);
            spans.push(Span::styled(tok_str, opts.theme.faint));
            spans.push(Span::styled(" · ", opts.theme.faint));
        }
        spans.push(Span::styled("ctrl+p", opts.theme.faint));
        spans
    } else {
        Vec::new()
    };

    let right_len: usize = right_spans.iter().map(|s| display_width(&s.content)).sum();
    let left_len: usize = left_spans.iter().map(|s| display_width(&s.content)).sum();

    if left_len + 2 + right_len <= width {
        let spaces = width - left_len - right_len;
        let mut out = left_spans;
        out.push(Span::raw(" ".repeat(spaces)));
        out.extend(right_spans);
        Line::from(out)
    } else if left_len <= width {
        Line::from(left_spans)
    } else {
        let plain: String = left_spans.iter().map(|s| s.content.as_ref()).collect();
        let trunc = truncate_to_width(&plain, width, true);
        Line::from(Span::styled(trunc, opts.theme.dim))
    }
}
