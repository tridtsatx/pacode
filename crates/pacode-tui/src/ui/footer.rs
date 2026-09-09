//! Footer rows (spec §3) and width degradation (spec §8).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::{RenderOptions, display_width, truncate_to_width};
use pacode_types::TranscriptKind;
use pacode_types::state::Mode;
use pacode_types::time::{format_duration_ms, now_ms};

use crate::state::transcript::CellKind;
use crate::state::{AppState, Connection, Focus, PanelTarget};

pub fn format_tokens_upper(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
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

    let mut left_spans = vec![
        Span::styled(mode.label(), opts.theme.cyan),
        Span::styled(" · ", opts.theme.faint),
        Span::styled(model_name, opts.theme.dim),
        Span::styled(" | ", opts.theme.faint),
        Span::styled(effort_str, opts.theme.accent),
    ];

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
        let plain = format!(
            "{} · {} | {}",
            mode.label(),
            state.model().map(|m| m.display_name()).unwrap_or_default(),
            state.effort()
        );
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
                        let total = state.rail.agents.len();
                        let subagents: Vec<_> = state
                            .rail
                            .agents
                            .iter()
                            .filter(|a| !a.id.is_main())
                            .collect();
                        let name = subagents
                            .get(*index)
                            .map(|a| a.name.as_str())
                            .unwrap_or("agent");
                        (
                            vec![
                                Span::styled(format!("{} ", opts.glyphs.chevrons), opts.theme.cyan),
                                Span::styled(
                                    format!("{name} {}/{} · ", index + 1, total),
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
                        let perm_style = if mode == Mode::Bypass {
                            opts.theme.red
                        } else {
                            opts.theme.cyan
                        };

                        let tok_str = format_tokens_upper(state.rail.usage.context_tokens as u64);
                        let right_text = format!("{tok_str} · ctrl+p");
                        let right_len = display_width(&right_text);

                        let perm_base =
                            format!("{} {}", opts.glyphs.chevrons, mode.permission_line());
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
                                    format!(" · {}", parts.join(" · "))
                                }
                            };

                        let perm_base_len = display_width(&perm_base);
                        let hint_len = display_width(hint);
                        let counts_len = display_width(&counts_or_idle);

                        // Degradation steps:
                        // 1. Full with hint and counts and context
                        let full_left_len = perm_base_len + hint_len + counts_len;
                        if full_left_len + 2 + right_len <= width {
                            let spans = vec![
                                Span::styled(format!("{} ", opts.glyphs.chevrons), perm_style),
                                Span::styled(mode.permission_line(), perm_style),
                                Span::styled(hint, opts.theme.faint),
                                Span::styled(counts_or_idle, opts.theme.faint),
                            ];
                            return render_row2_with_right(spans, right_text, width, opts);
                        }

                        // 2. Drop hint
                        let no_hint_len = perm_base_len + counts_len;
                        if no_hint_len + 2 + right_len <= width {
                            let spans = vec![
                                Span::styled(format!("{} ", opts.glyphs.chevrons), perm_style),
                                Span::styled(mode.permission_line(), perm_style),
                                Span::styled(counts_or_idle, opts.theme.faint),
                            ];
                            return render_row2_with_right(spans, right_text, width, opts);
                        }

                        // 3. Drop counts
                        if perm_base_len + 2 + right_len <= width {
                            let spans = vec![
                                Span::styled(format!("{} ", opts.glyphs.chevrons), perm_style),
                                Span::styled(mode.permission_line(), perm_style),
                            ];
                            return render_row2_with_right(spans, right_text, width, opts);
                        }

                        // 4. Drop context (right)
                        if full_left_len <= width {
                            let spans = vec![
                                Span::styled(format!("{} ", opts.glyphs.chevrons), perm_style),
                                Span::styled(mode.permission_line(), perm_style),
                                Span::styled(hint, opts.theme.faint),
                                Span::styled(counts_or_idle, opts.theme.faint),
                            ];
                            return Line::from(spans);
                        }

                        if no_hint_len <= width {
                            let spans = vec![
                                Span::styled(format!("{} ", opts.glyphs.chevrons), perm_style),
                                Span::styled(mode.permission_line(), perm_style),
                                Span::styled(counts_or_idle, opts.theme.faint),
                            ];
                            return Line::from(spans);
                        }

                        let trunc = truncate_to_width(&perm_base, width, true);
                        return Line::from(Span::styled(trunc, perm_style));
                    }
                }
            }
        }
    };

    let right_spans = if show_context {
        let tokens = state.rail.usage.context_tokens as u64;
        if tokens == 0 {
            vec![Span::styled("ctrl+p", opts.theme.faint)]
        } else {
            let tok_str = format_tokens_upper(tokens);
            vec![
                Span::styled(tok_str, opts.theme.faint),
                Span::styled(" · ctrl+p", opts.theme.faint),
            ]
        }
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
