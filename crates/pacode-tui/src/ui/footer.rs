//! Footer rows (spec §3) and width degradation (spec §8).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::{RenderOptions, display_width, truncate_to_width};
use pacode_types::TranscriptKind;
use pacode_types::time::{format_duration_ms, now_ms};

use crate::binding::Action as KeyAction;
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

/// Slot badges with per-status colors: `NeedsInput` blinks on the shared
/// pulse phase (accent while lit, dimmed otherwise), `Working` is green,
/// `Done` cyan, `Idle` faint. The active slot keeps its `[N]` accent except
/// under `NeedsInput`, where it blinks like the others.
pub fn render_slots_spans(state: &AppState, opts: &RenderOptions) -> Vec<Span<'static>> {
    use pacode_types::SessionStatusKind as Kind;
    let lit = state.pulse_lit(std::time::Instant::now());
    let blinking =
        |opts: &RenderOptions| crate::ui::anim::pulse(&opts.theme, opts.theme.accent, lit);
    let mut spans = Vec::with_capacity(9);
    for i in 0..9 {
        let num = i + 1;
        if i == state.active_slot {
            let style = match state.active_slot_status() {
                Kind::NeedsInput => blinking(opts),
                _ => opts.theme.accent,
            };
            spans.push(Span::styled(format!("[{num}]"), style));
        } else if let Some(slot) = &state.slots[i] {
            let style = match slot.status {
                Kind::NeedsInput => blinking(opts),
                Kind::Working => opts.theme.green,
                Kind::Done => opts.theme.cyan,
                Kind::Idle => opts.theme.faint,
            };
            spans.push(Span::styled(format!("{num}"), style));
        } else {
            spans.push(Span::styled("·", opts.theme.faint));
        }
    }
    spans
}

/// `slots · tokens · ctrl+p · menu hint` — the footer's right side, shared by
/// the plain row and every degradation step so all of them stay colored.
fn context_right_spans(state: &AppState, opts: &RenderOptions) -> Vec<Span<'static>> {
    let tokens = state.rail.usage.context_tokens as u64;
    let mut spans = render_slots_spans(state, opts);
    // The notice lock leads the right side, before the slot digits: a held
    // notice changes what the numbers below it mean, and it is the only piece
    // here that is not chrome. Empty when the lock is off and nothing is
    // parked, so the ordinary case costs nothing.
    let lock = state.notice_lock.footer_spans(&opts.theme);
    if !lock.is_empty() {
        spans.extend(lock);
        spans.push(Span::styled(" · ", opts.theme.faint));
    }
    if tokens > 0 {
        spans.push(Span::styled(" · ", opts.theme.faint));
        spans.push(Span::styled(format_tokens_upper(tokens), opts.theme.faint));
    }
    let keys = [
        state.keymap.hint(KeyAction::SessionPicker),
        menu_hint(state),
    ];
    for key in keys.into_iter().flatten() {
        spans.push(Span::styled(" · ", opts.theme.faint));
        spans.push(Span::styled(key, opts.theme.faint));
    }
    spans
}

/// `key verb · key verb` for the actions that are bound, in the keymap's own
/// spelling — a rebound key shows its new chord, an unbound action drops out
/// rather than advertise a key that does nothing (B27).
pub(crate) fn key_hints(state: &AppState, items: &[(KeyAction, &str)]) -> String {
    items
        .iter()
        .filter_map(|(action, verb)| {
            state
                .keymap
                .hint(*action)
                .map(|key| format!("{key} {verb}"))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let row1 = super::footer_status::render(area.width as usize, state, opts);
    let row2 = if area.height > 1 {
        render_row2(area.width as usize, state, opts)
    } else {
        Line::default()
    };

    frame.render_widget(Paragraph::new(vec![row1, row2]), area);
}

fn render_row2_with_right(
    left_spans: Vec<Span<'static>>,
    right_spans: Vec<Span<'static>>,
    width: usize,
) -> Line<'static> {
    let left_len: usize = left_spans.iter().map(|s| display_width(&s.content)).sum();
    let right_len: usize = right_spans.iter().map(|s| display_width(&s.content)).sum();
    let spaces = width.saturating_sub(left_len + right_len);
    let mut out = left_spans;
    out.push(Span::raw(" ".repeat(spaces)));
    out.extend(right_spans);
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
        Connection::Connecting => (
            vec![Span::styled(
                format!(
                    "connecting{}",
                    crate::state::activity::trailing_dots(state.anim_frame, &opts.glyphs)
                ),
                opts.theme.dim,
            )],
            false,
        ),
        Connection::Reconnecting { attempt } => (
            vec![Span::styled(
                format!(
                    "reconnecting{} (attempt {attempt})",
                    crate::state::activity::trailing_dots(state.anim_frame, &opts.glyphs)
                ),
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
                // A pending permission is activity: the chevrons and title
                // pulse accent <-> yellow on the shared blink phase.
                let lit = state.pulse_lit(std::time::Instant::now());
                let pulse = if lit {
                    opts.theme.accent
                } else {
                    opts.theme.yellow
                };
                (
                    vec![
                        Span::styled(format!("{} permission: ", opts.glyphs.chevrons), pulse),
                        Span::styled(format!("{} · ", req.title), opts.theme.bold.patch(pulse)),
                        Span::styled("y allow · a session · n deny · e explain", opts.theme.cyan),
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
                                    key_hints(
                                        state,
                                        &[
                                            (KeyAction::Submit, "open"),
                                            (KeyAction::FollowAgent, "follow"),
                                            (KeyAction::Cancel, "clear"),
                                        ],
                                    ),
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
                            PanelTarget::McpServer(name) => name.as_str(),
                            PanelTarget::FileDiff { path } => path.as_str(),
                            PanelTarget::CommitDiff { hash } => state
                                .git
                                .commits
                                .iter()
                                .find(|c| c.hash == *hash)
                                .map(|c| c.short.as_str())
                                .unwrap_or(hash.as_str()),
                        };
                        if *follow {
                            let main_id = pacode_types::AgentId::main();
                            let agent_id = match target {
                                PanelTarget::Agent(id) => id,
                                PanelTarget::Task(_)
                                | PanelTarget::McpServer(_)
                                | PanelTarget::FileDiff { .. }
                                | PanelTarget::CommitDiff { .. } => &main_id,
                            };
                            let dur = state
                                .rail
                                .agent(agent_id)
                                .map(|a| format_duration_ms(a.duration_ms(now_ms())))
                                .unwrap_or_default();
                            (
                                vec![
                                    crate::ui::chrome::badge("FOLLOW", opts.theme.accent, opts),
                                    Span::raw(" "),
                                    Span::styled(format!("{name} {dur} · "), opts.theme.bold),
                                    Span::styled(
                                        key_hints(
                                            state,
                                            &[
                                                (KeyAction::ScrollUp, "pause"),
                                                (KeyAction::FollowAgent, "release"),
                                            ],
                                        ),
                                        opts.theme.dim,
                                    ),
                                ],
                                false,
                            )
                        } else {
                            // A log view has no agent to follow or stop —
                            // it only scrolls.
                            let items: &[(KeyAction, &str)] = match target {
                                PanelTarget::McpServer(_)
                                | PanelTarget::FileDiff { .. }
                                | PanelTarget::CommitDiff { .. } => {
                                    &[(KeyAction::Cancel, "back"), (KeyAction::ScrollUp, "scroll")]
                                }
                                PanelTarget::Agent(_) => &[
                                    (KeyAction::Cancel, "back"),
                                    (KeyAction::FollowAgent, "follow"),
                                    (KeyAction::StopAgent, "stop"),
                                ],
                                PanelTarget::Task(_) => &[
                                    (KeyAction::Cancel, "back"),
                                    (KeyAction::ScrollUp, "scroll"),
                                    (KeyAction::KillTask, "kill"),
                                ],
                            };
                            let hints = key_hints(state, items);
                            (
                                vec![
                                    Span::styled(
                                        format!("{} ", opts.glyphs.chevrons),
                                        opts.theme.cyan,
                                    ),
                                    Span::styled(format!("{name} · "), opts.theme.bold),
                                    Span::styled(hints, opts.theme.dim),
                                ],
                                false,
                            )
                        }
                    }
                    _ => {
                        // Without a session the mode is unknown: saying the
                        // default mode's rules would be a guess (B10). Row 1
                        // already says "no session"; this row keeps only the
                        // slots and the keys.
                        let Some(meta) = state.meta.as_ref() else {
                            return render_row2_with_right(
                                Vec::new(),
                                context_right_spans(state, opts),
                                width,
                            );
                        };
                        let perm_line = meta.mode.permission_line();
                        let perm_style = crate::ui::chrome::mode_tone(meta.mode, &opts.theme);
                        let hint = state
                            .keymap
                            .hint(KeyAction::CycleMode)
                            .map(|key| format!(" ({key} to cycle)"))
                            .unwrap_or_default();

                        let right_spans = context_right_spans(state, opts);
                        let right_len: usize =
                            right_spans.iter().map(|s| display_width(&s.content)).sum();

                        let perm_base = format!("{} {perm_line}", opts.glyphs.chevrons);
                        let plugin_status_str = state
                            .plugin_status
                            .as_ref()
                            .map(|(_, t)| format!(" · {t}"))
                            .unwrap_or_default();
                        let counts_or_idle =
                            if state.rail.show_session_stats && state.rail.usage.turns > 0 {
                                let done_count = state
                                    .rail
                                    .agents
                                    .iter()
                                    .filter(|a| !a.id.is_main() && !a.status.is_live())
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
                        let hint_len = display_width(&hint);
                        let counts_len = display_width(&counts_or_idle);

                        let make_spans = |with_status: bool, with_hint: bool, with_counts: bool| {
                            let mut spans = vec![
                                Span::styled(format!("{} ", opts.glyphs.chevrons), perm_style),
                                Span::styled(perm_line, perm_style),
                            ];
                            if with_status && !plugin_status_str.is_empty() {
                                spans.push(Span::styled(plugin_status_str.clone(), opts.theme.dim));
                            }
                            if with_hint {
                                spans.push(Span::styled(hint.clone(), opts.theme.faint));
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
                                right_spans,
                                width,
                            );
                        }

                        // 2. Drop hint (keep status)
                        let no_hint_len = perm_base_len + status_len + counts_len;
                        if no_hint_len + 2 + right_len <= width {
                            return render_row2_with_right(
                                make_spans(true, false, true),
                                right_spans,
                                width,
                            );
                        }

                        // 3. Drop counts (keep status)
                        let base_status_len = perm_base_len + status_len;
                        if base_status_len + 2 + right_len <= width {
                            return render_row2_with_right(
                                make_spans(true, false, false),
                                right_spans,
                                width,
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
        context_right_spans(state, opts)
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

/// The hub entry hint (§4.2): `shift shift` once the terminal has proven it
/// reports the gesture, the `Menu` binding (`ctrl+,` by default) while it
/// has not or while the detector is switched off. `None` when neither works.
fn menu_hint(state: &AppState) -> Option<String> {
    if state.config.keys.double_shift_enabled() && state.double_shift.available() {
        Some("shift shift".to_string())
    } else {
        state.keymap.hint(KeyAction::Menu)
    }
}
