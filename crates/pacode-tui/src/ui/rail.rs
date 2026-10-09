//! The rail (spec §4): header, PLAN, AGENTS (or SESSION when idle), BACKGROUND, anchor.

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::AgentId;
use pacode_types::state::{AgentStatus, PlanStatus};
use pacode_types::time::{format_duration_ms, format_tokens, now_ms};

use crate::layout::{RailDemand, RailLayout, compute_rail};
use crate::state::{AppState, Focus, Overlay};
use crate::ui::overlays::selected_row;

enum AgentSelection {
    Index(usize),
    Id(AgentId),
}

#[derive(Clone, Copy)]
pub(crate) struct PreparedRail {
    pub(crate) layout: RailLayout,
    pub(crate) select_mode: bool,
    pub(crate) show_session: bool,
    pub(crate) has_subagents: bool,
    pub(crate) selected_main: bool,
    pub(crate) selected_subagent: Option<usize>,
    pub(crate) agents_scroll: usize,
}

pub(crate) fn resolve(state: &AppState, area: Rect) -> Option<PreparedRail> {
    resolve_from(state, area, state.rail.agents_scroll)
}

pub(crate) fn resolve_from(
    state: &AppState,
    area: Rect,
    stored_scroll: usize,
) -> Option<PreparedRail> {
    if area.width == 0 || area.height == 0 {
        return None;
    }

    let plan_lines = if state.rail.plan.is_empty() {
        0
    } else {
        let count = state.rail.plan.counted().count() as u16;
        let bar_lines = if let Some(active) = state.rail.plan.active() {
            if active.progress.is_some() { 1 } else { 0 }
        } else {
            0
        };
        1 + count + bar_lines
    };

    let bg_tasks_count = state.rail.background_tasks().count() as u16;
    let schedule_count = state.rail.schedule_row_count();
    let schedule_lines = if schedule_count > 0 {
        1 + (schedule_count as u16).min(4)
    } else {
        0
    };
    let bg_lines = if bg_tasks_count > 0 {
        1 + bg_tasks_count.min(4) + schedule_lines
    } else {
        schedule_lines
    };

    let selection = match &state.focus {
        Focus::SelectAgent { index } => Some(AgentSelection::Index(*index)),
        Focus::Overlay(Overlay::RailOverlay { selected }) => {
            Some(AgentSelection::Id(selected.clone()))
        }
        Focus::Normal | Focus::Panel { .. } | Focus::Overlay(_) => None,
    };
    let select_mode = selection.is_some();
    let show_session = state.rail.show_session_stats && state.rail.usage.turns > 0;
    let layout = compute_rail(
        area,
        RailDemand {
            plan_lines,
            background_lines: bg_lines,
            session_lines: 8,
            agent_select_mode: select_mode,
            idle: show_session,
            anchor_y: None,
        },
    );
    let subagent_count = state
        .rail
        .agents
        .iter()
        .filter(|agent| !agent.id.is_main())
        .count();
    let (selected_main, selected_subagent) = match &selection {
        Some(selection) => selected_positions(
            &crate::nav::selectable_agents(state),
            selection,
            subagent_count,
        ),
        None => (false, None),
    };
    let mut agents_scroll = stored_scroll;
    if let Some(selected_idx) = selected_subagent
        && layout.agents.height >= 4
    {
        let remaining = (layout.agents.height as usize).saturating_sub(2);
        let selected_height = 3.min(remaining);
        if selected_idx < agents_scroll {
            agents_scroll = selected_idx;
        } else if selected_idx + selected_height > agents_scroll + remaining {
            agents_scroll = selected_idx + selected_height - remaining;
        }
    }

    Some(PreparedRail {
        layout,
        select_mode,
        show_session,
        has_subagents: state.rail.agents.iter().any(|a| !a.id.is_main()),
        selected_main,
        selected_subagent,
        agents_scroll,
    })
}

pub(crate) fn persist(state: &mut AppState, prepared: &PreparedRail) {
    if prepared.selected_subagent.is_some() && prepared.layout.agents.height >= 4 {
        state.rail.agents_scroll = prepared.agents_scroll;
    }
}

pub(crate) fn draw(
    frame: &mut Frame,
    prepared: &PreparedRail,
    state: &AppState,
    opts: &RenderOptions,
) {
    let PreparedRail {
        layout,
        select_mode,
        show_session,
        has_subagents,
        selected_main,
        selected_subagent,
        agents_scroll,
    } = *prepared;

    draw_header(frame, layout.header, state, opts);
    draw_plan(frame, layout.plan, state, opts, select_mode);
    if select_mode {
        draw_agents(
            frame,
            layout.agents,
            state,
            opts,
            selected_main,
            selected_subagent,
            agents_scroll,
        );
    } else if show_session {
        crate::ui::rail_session::draw_session(frame, layout.agents, state, opts);
    } else if has_subagents {
        draw_agents(
            frame,
            layout.agents,
            state,
            opts,
            false,
            None,
            agents_scroll,
        );
    }
    if layout.background.height > 0 {
        crate::ui::rail_session::draw_background(frame, layout.background, state, opts);
    }
    crate::ui::rail_session::draw_anchor(frame, layout.anchor, state, opts);
}

fn draw_header(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    if area.height == 0 {
        return;
    }
    let title = state
        .meta
        .as_ref()
        .map(|m| m.title())
        .unwrap_or_else(|| "new session".to_string());
    let id_str = state
        .meta
        .as_ref()
        .map(|m| m.id.to_string())
        .unwrap_or_default();

    let title_line = Line::from(Span::styled(
        truncate_to_width(&title, area.width as usize, true),
        opts.theme.bold,
    ));
    let id_line = Line::from(Span::styled(
        truncate_to_width(&id_str, area.width as usize, true),
        opts.theme.faint,
    ));

    let lines = vec![title_line, id_line, Line::default()];
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_plan(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    opts: &RenderOptions,
    select_mode: bool,
) {
    if area.height == 0 {
        return;
    }
    let mut lines = Vec::new();
    let total = state.rail.plan.total();
    let done = state.rail.plan.done();
    let pct = state.rail.plan.percent();

    let right_text = if select_mode {
        format!("{done}/{total} · {pct}% {}", opts.glyphs.pointer)
    } else {
        format!("{done}/{total} · {pct}%")
    };

    let title_text = "PLAN";
    let w = area.width as usize;
    let spaces = w.saturating_sub(title_text.len() + right_text.chars().count());
    let sep = " ".repeat(spaces);
    lines.push(Line::from(vec![
        crate::ui::chrome::section(title_text, opts),
        Span::raw(sep),
        Span::styled(right_text, opts.theme.dim),
    ]));

    if !select_mode && area.height > 1 {
        for item in state.rail.plan.counted() {
            if lines.len() >= area.height as usize {
                break;
            }
            let (tag, tag_style, text_style) = match item.status {
                PlanStatus::Done => (opts.glyphs.check_done, opts.theme.green, opts.theme.dim),
                PlanStatus::Active => (opts.glyphs.check_active, opts.theme.accent, opts.theme.fg),
                PlanStatus::Pending => (
                    opts.glyphs.check_pending,
                    opts.theme.faint,
                    opts.theme.faint,
                ),
                PlanStatus::Cancelled => continue,
            };

            let tag_len = tag.chars().count() + 1;
            let avail = (area.width as usize).saturating_sub(tag_len);
            let trunc_content = truncate_to_width(&item.content, avail, true);

            lines.push(Line::from(vec![
                Span::styled(tag, tag_style),
                Span::raw(" "),
                Span::styled(trunc_content, text_style),
            ]));

            if item.status == PlanStatus::Active
                && let Some(progress) = item.progress
                && lines.len() < area.height as usize
            {
                let bar = opts.glyphs.progress_bar(progress, 8);
                let text = format!("    {bar} {progress}%");
                lines.push(Line::from(Span::styled(text, opts.theme.accent)));
            }
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// Symbol and style for an agent's rail dot. Live agents breathe on the shared
/// pulse phase (`AppState::pulse_lit`); terminal ones stay put — a stopped dot
/// keeps its static dim look rather than blinking.
fn agent_dot(
    agent: &pacode_types::AgentInfo,
    lit: bool,
    opts: &RenderOptions,
) -> (&'static str, ratatui::style::Style) {
    match agent.status {
        AgentStatus::Failed => (opts.glyphs.fail, opts.theme.red),
        AgentStatus::Finished => (opts.glyphs.ok, opts.theme.green),
        AgentStatus::Stopped => (opts.glyphs.agent_dot, opts.theme.dim),
        AgentStatus::Idle
        | AgentStatus::Thinking
        | AgentStatus::RunningTool
        | AgentStatus::WaitingApproval => (
            opts.glyphs.agent_dot,
            crate::ui::anim::pulse(&opts.theme, opts.theme.green, lit),
        ),
    }
}

fn draw_agents(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    opts: &RenderOptions,
    selected_main: bool,
    selected_subagent: Option<usize>,
    agents_scroll: usize,
) {
    if area.height == 0 {
        return;
    }
    let count = state.rail.agents.len();
    if area.height < 4 {
        let text = format!("{count} agents {}", opts.glyphs.arrow_down);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(text, opts.theme.dim))),
            area,
        );
        return;
    }

    // Live indicators pulse: on the anim frame while the fast tick is armed,
    // on the second parity when only background work keeps the 1 s tick alive.
    let lit = state.pulse_lit(Instant::now());
    let main_live = state.turn_active
        || state
            .rail
            .agents
            .iter()
            .any(|a| a.id.is_main() && a.status.is_live());
    let main_dot_style = if main_live {
        crate::ui::anim::pulse(&opts.theme, opts.theme.green, lit)
    } else {
        opts.theme.green
    };

    let mut lines = Vec::new();
    let title_text = "AGENTS";
    let right_text = format!("{count} ");
    let spaces = (area.width as usize).saturating_sub(title_text.len() + right_text.len() + 1);
    lines.push(Line::from(vec![
        crate::ui::chrome::section(title_text, opts),
        Span::raw(" ".repeat(spaces)),
        Span::styled(right_text, opts.theme.dim),
        Span::styled(opts.glyphs.main_dot, main_dot_style),
    ]));

    let subagents: Vec<_> = state
        .session
        .rail
        .agents
        .iter()
        .filter(|a| !a.id.is_main())
        .collect();

    let now = now_ms();
    // The rail says the same thing as the activity line: whatever the main agent
    // is really doing, aged from the same phase start, in whole seconds.
    let main_line = if let Some((phase, _)) = state.phase.as_ref().filter(|_| state.turn_active) {
        let elapsed = state.phase_elapsed_ms;
        let dur_str = crate::state::activity::format_activity_secs(
            crate::state::activity::displayed_secs(elapsed),
        );
        let label = phase.label(elapsed, state.anim_frame, &opts.glyphs);
        Line::from(vec![
            Span::styled(opts.glyphs.main_dot, main_dot_style),
            Span::raw(" "),
            Span::styled("main  ", opts.theme.bold),
            Span::styled(format!("{label} · {dur_str}"), opts.theme.faint),
        ])
    } else {
        Line::from(vec![
            Span::styled(opts.glyphs.main_dot, main_dot_style),
            Span::raw(" "),
            Span::styled("main", opts.theme.bold),
        ])
    };
    lines.push(if selected_main {
        selected_row(main_line, area.width, opts)
    } else {
        main_line
    });

    let rem_h = (area.height as usize).saturating_sub(lines.len());

    if let Some(selected_idx) = selected_subagent {
        for (i, agent) in subagents.iter().enumerate().skip(agents_scroll) {
            if lines.len() >= area.height as usize {
                break;
            }
            let is_sel = i == selected_idx;
            let (dot, dot_style) = agent_dot(agent, lit, opts);

            if is_sel {
                let dur = format_duration_ms(agent.duration_ms(now));
                let tok = format_tokens(agent.tokens_in);
                let line1 = Line::from(vec![
                    Span::styled(opts.glyphs.pointer, opts.theme.accent),
                    Span::raw(" "),
                    Span::styled(dot, dot_style),
                    Span::raw(" "),
                    Span::styled(&agent.name, opts.theme.selected_bg),
                ]);
                let act = agent.activity.as_deref().unwrap_or("active");
                let line2 = Line::from(Span::styled(
                    format!(
                        "    {}",
                        truncate_to_width(act, (area.width as usize).saturating_sub(4), true)
                    ),
                    opts.theme.faint,
                ));
                let line3 = Line::from(Span::styled(
                    format!("    {dur} · ↓ {tok}"),
                    opts.theme.faint,
                ));
                lines.push(line1);
                if lines.len() < area.height as usize {
                    lines.push(line2);
                }
                if lines.len() < area.height as usize {
                    lines.push(line3);
                }
            } else {
                let dur = format_duration_ms(agent.duration_ms(now));
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(dot, dot_style),
                    Span::raw(" "),
                    Span::styled(&agent.name, opts.theme.fg),
                    Span::raw("  "),
                    Span::styled(dur, opts.theme.faint),
                ]));
            }
        }
    } else {
        let all_fit_3 = subagents.len() * 3 <= rem_h;
        let all_fit_1 = subagents.len() <= rem_h;

        if all_fit_3 {
            for agent in subagents {
                let (dot, dot_style) = agent_dot(agent, lit, opts);

                let dur = format_duration_ms(agent.duration_ms(now));
                let tok = format_tokens(agent.tokens_in);
                lines.push(Line::from(vec![
                    Span::styled(dot, dot_style),
                    Span::raw(" "),
                    Span::styled(&agent.name, opts.theme.fg),
                ]));
                let act = agent.activity.as_deref().unwrap_or("");
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {}",
                        truncate_to_width(act, (area.width as usize).saturating_sub(2), true)
                    ),
                    opts.theme.faint,
                )));
                lines.push(Line::from(Span::styled(
                    format!("  {dur} · ↓ {tok}"),
                    opts.theme.faint,
                )));
            }
        } else if all_fit_1 {
            for agent in subagents {
                let (dot, dot_style) = agent_dot(agent, lit, opts);
                let dur = format_duration_ms(agent.duration_ms(now));
                lines.push(Line::from(vec![
                    Span::styled(dot, dot_style),
                    Span::raw(" "),
                    Span::styled(&agent.name, opts.theme.fg),
                    Span::raw("  "),
                    Span::styled(dur, opts.theme.faint),
                ]));
            }
        } else {
            let visible = rem_h.saturating_sub(1);
            for agent in subagents.iter().take(visible) {
                let (dot, dot_style) = agent_dot(agent, lit, opts);
                let dur = format_duration_ms(agent.duration_ms(now));
                lines.push(Line::from(vec![
                    Span::styled(dot, dot_style),
                    Span::raw(" "),
                    Span::styled(&agent.name, opts.theme.fg),
                    Span::raw("  "),
                    Span::styled(dur, opts.theme.faint),
                ]));
            }
            let remaining = subagents.len() - visible;
            lines.push(Line::from(Span::styled(
                format!("  and {remaining} more alt+↓"),
                opts.theme.faint,
            )));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn selected_positions(
    agents: &[AgentId],
    selection: &AgentSelection,
    subagent_count: usize,
) -> (bool, Option<usize>) {
    match selection {
        AgentSelection::Index(index) => (
            *index == 0,
            index.checked_sub(1).filter(|index| *index < subagent_count),
        ),
        AgentSelection::Id(selected) => {
            let Some(index) = agents.iter().position(|agent| agent == selected) else {
                return (false, None);
            };
            (
                selected.is_main(),
                index.checked_sub(1).filter(|index| *index < subagent_count),
            )
        }
    }
}

#[cfg(test)]
#[path = "rail_tests.rs"]
mod rail_tests;
