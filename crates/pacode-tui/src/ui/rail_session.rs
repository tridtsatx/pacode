//! Rail zones: SESSION (idle stats), BACKGROUND tasks, and anchor.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::{RenderOptions, display_width, truncate_to_width};
use pacode_types::state::{AgentStatus, TaskStatus};
use pacode_types::time::{format_duration_ms, format_tokens, now_ms};

use crate::state::AppState;

pub fn draw_session(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    if area.height == 0 {
        return;
    }
    let usage = &state.rail.usage;
    let mut lines = Vec::new();

    let in_tok = format_tokens(usage.input);
    let out_tok = format_tokens(usage.output);
    let think_tok = format_tokens(usage.reasoning);
    lines.push(Line::from(Span::styled(
        format!("in {in_tok}  out {out_tok}  think {think_tok}"),
        opts.theme.dim,
    )));

    let hit = usage.cache_hit_percent().unwrap_or(0.0);
    let r_tok = format_tokens(usage.cache_read);
    let w_tok = format_tokens(usage.cache_write);
    lines.push(Line::from(vec![
        Span::styled(format!("cache hit {hit:.1}%"), opts.theme.green),
        Span::styled(format!("  r {r_tok} / w {w_tok}"), opts.theme.dim),
    ]));

    let turns = usage.turns;
    if let Some(cost) = usage.cost_usd {
        lines.push(Line::from(vec![
            Span::styled(format!("{turns} turns · "), opts.theme.dim),
            Span::styled(format!("${cost:.2}"), opts.theme.accent),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            format!("{turns} turns"),
            opts.theme.dim,
        )));
    }

    let ctx = usage.context_tokens;
    let ctx_pct = usage.context_percent().unwrap_or(0);
    lines.push(Line::from(Span::styled(
        format!("ctx {ctx} · {ctx_pct}% used"),
        opts.theme.dim,
    )));

    let finished_subagents: Vec<_> = state
        .rail
        .agents
        .iter()
        .filter(|a| {
            !a.id.is_main()
                && (a.status == AgentStatus::Finished || a.status == AgentStatus::Failed)
        })
        .collect();

    if !finished_subagents.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!("AGENTS  {} done", finished_subagents.len()),
            opts.theme.faint,
        )));

        let rem = (area.height as usize).saturating_sub(lines.len());
        let now = now_ms();
        if finished_subagents.len() <= rem {
            for a in finished_subagents {
                let dur = format_duration_ms(a.duration_ms(now));
                let tok = format_tokens(a.tokens_in);
                let dot = if a.status == AgentStatus::Failed {
                    opts.glyphs.fail
                } else {
                    opts.glyphs.ok
                };
                let style = if a.status == AgentStatus::Failed {
                    opts.theme.red
                } else {
                    opts.theme.green
                };
                lines.push(Line::from(vec![
                    Span::styled(dot, style),
                    Span::raw(" "),
                    Span::styled(&a.name, opts.theme.fg),
                    Span::raw(" "),
                    Span::styled(format!("{dur} · {tok}"), opts.theme.faint),
                ]));
            }
        } else if rem > 1 {
            for a in finished_subagents.iter().take(rem - 1) {
                let dur = format_duration_ms(a.duration_ms(now));
                let dot = opts.glyphs.ok;
                lines.push(Line::from(vec![
                    Span::styled(dot, opts.theme.green),
                    Span::raw(" "),
                    Span::styled(&a.name, opts.theme.fg),
                    Span::raw(" "),
                    Span::styled(dur, opts.theme.faint),
                ]));
            }
            let more = finished_subagents.len() - (rem - 1);
            lines.push(Line::from(Span::styled(
                format!("and {more} more"),
                opts.theme.faint,
            )));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

pub fn draw_background(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    if area.height == 0 {
        return;
    }
    let mut lines = Vec::new();
    let running = state.rail.running_task_count();
    let failed_unacked = state.rail.failed_unacked_count();
    let completed = state
        .rail
        .background_tasks()
        .filter(|t| t.status == TaskStatus::Completed)
        .count();

    let title_text = "BACKGROUND";
    let mut right_spans = Vec::new();
    if running > 0 {
        right_spans.push(Span::styled(
            format!("{running} {} ", opts.glyphs.running),
            opts.theme.violet,
        ));
    }
    if completed > 0 {
        right_spans.push(Span::styled(
            format!("{completed} {} ", opts.glyphs.ok),
            opts.theme.green,
        ));
    }
    if failed_unacked > 0 {
        right_spans.push(Span::styled(
            format!("{failed_unacked} {}", opts.glyphs.fail),
            opts.theme.bold.patch(opts.theme.red),
        ));
    }

    let right_len: usize = right_spans.iter().map(|s| display_width(&s.content)).sum();
    let spaces = (area.width as usize).saturating_sub(title_text.len() + right_len);

    let tasks: Vec<_> = state.rail.background_tasks().collect();
    // With nothing running, the zone belongs to the schedule rows alone and the
    // BACKGROUND header would name an empty list.
    if !tasks.is_empty() {
        let mut header_spans = vec![
            Span::styled(title_text, opts.theme.faint),
            Span::raw(" ".repeat(spaces)),
        ];
        header_spans.extend(right_spans);
        lines.push(Line::from(header_spans));
    }

    let rem = (area.height as usize).saturating_sub(1);
    let now = now_ms();

    if tasks.len() <= rem {
        for t in tasks {
            lines.push(render_task_line(t, area.width as usize, opts, now));
        }
    } else if rem > 1 {
        for t in tasks.iter().take(rem - 1) {
            lines.push(render_task_line(t, area.width as usize, opts, now));
        }
        let more = tasks.len() - (rem - 1);
        lines.push(Line::from(Span::styled(
            format!("  and {more} more"),
            opts.theme.faint,
        )));
    }

    // The schedule rows share this zone: a cron job or a monitor is background
    // work the reader watches the same way, and giving it its own zone would cost
    // a rail block that most sessions never use.
    let used = lines.len();
    let left = (area.height as usize).saturating_sub(used);
    if left > 1 && state.rail.has_schedule_rows() {
        lines.extend(schedule_lines(
            state,
            area.width as usize,
            opts,
            now,
            left - 1,
        ));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// `SCHEDULE` header plus one row per enabled job and live monitor, newest last.
fn schedule_lines(
    state: &AppState,
    width: usize,
    opts: &RenderOptions,
    now: u64,
    budget: usize,
) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(Span::styled("SCHEDULE", opts.theme.faint))];
    let mut rows = Vec::new();

    for job in state.rail.cron_jobs.iter().filter(|j| j.enabled) {
        let due = job
            .due_in_ms(now)
            .map(|ms| format!("in {}", format_duration_ms(ms)))
            .unwrap_or_else(|| "paused".to_string());
        rows.push((opts.glyphs.running, opts.theme.cyan, job.name.clone(), due));
    }
    for monitor in state.rail.monitors.iter().filter(|m| m.status.is_live()) {
        rows.push((
            opts.glyphs.running,
            opts.theme.violet,
            monitor.label.clone(),
            "watching".to_string(),
        ));
    }

    let budget = budget.saturating_sub(1);
    let shown = rows.len().min(budget);
    for (sym, style, label, tail) in rows.iter().take(shown) {
        let tail_w = display_width(tail);
        let label_w = width.saturating_sub(tail_w + 4);
        let label = truncate_to_width(label, label_w, true);
        let pad = width
            .saturating_sub(display_width(&label) + tail_w + 3)
            .max(1);
        out.push(Line::from(vec![
            Span::styled(format!("{sym} "), *style),
            Span::styled(label, opts.theme.fg),
            Span::raw(" ".repeat(pad)),
            Span::styled(tail.clone(), opts.theme.faint),
        ]));
    }
    if rows.len() > shown {
        let more = rows.len() - shown;
        out.push(Line::from(Span::styled(
            format!("  and {more} more"),
            opts.theme.faint,
        )));
    }
    out
}

fn render_task_line(
    task: &pacode_types::TaskInfo,
    width: usize,
    opts: &RenderOptions,
    now: u64,
) -> Line<'static> {
    let dur = format_duration_ms(task.duration_ms(now));
    let (sym, sym_style) = match task.status {
        TaskStatus::Running => (opts.glyphs.running, opts.theme.violet),
        TaskStatus::Completed => (opts.glyphs.ok, opts.theme.green),
        TaskStatus::Failed | TaskStatus::Killed => (opts.glyphs.fail, opts.theme.red),
    };

    let tail_str = match task.status {
        TaskStatus::Running => {
            if let Some(ref p) = task.progress {
                p.short_label().unwrap_or(dur)
            } else {
                dur
            }
        }
        TaskStatus::Completed => dur,
        TaskStatus::Failed => {
            if task.errors > 0 {
                format!("{} err", task.errors)
            } else {
                "failed".to_string()
            }
        }
        TaskStatus::Killed => "killed".to_string(),
    };

    let label_avail = width.saturating_sub(display_width(sym) + display_width(&tail_str) + 2);
    let trunc_label = truncate_to_width(&task.label, label_avail, true);

    Line::from(vec![
        Span::styled(sym, sym_style),
        Span::raw(" "),
        Span::styled(trunc_label, opts.theme.fg),
        Span::raw(" "),
        Span::styled(tail_str, opts.theme.faint),
    ])
}

pub fn draw_anchor(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    if area.height == 0 {
        return;
    }
    let cwd_str = state
        .meta
        .as_ref()
        .map(|m| {
            let p = m.cwd.to_string_lossy();
            if let Ok(home) = std::env::var("HOME") {
                let home_path = std::path::Path::new(&home);
                if let Ok(rel) = m.cwd.strip_prefix(home_path) {
                    format!("~/{}", rel.display())
                } else {
                    p.into_owned()
                }
            } else {
                p.into_owned()
            }
        })
        .unwrap_or_else(|| "~/pacode".to_string());

    let branch = state
        .meta
        .as_ref()
        .and_then(|m| m.git_branch.as_deref())
        .unwrap_or("master");

    let branch_str = format!(":{branch}");
    let branch_w = display_width(&branch_str);
    let cwd_w = (area.width as usize).saturating_sub(branch_w);
    let cwd_trunc = truncate_to_width(&cwd_str, cwd_w, true);

    let line1 = Line::from(vec![
        Span::styled(cwd_trunc, opts.theme.dim),
        Span::styled(branch_str, opts.theme.accent),
    ]);

    frame.render_widget(Paragraph::new(vec![line1]).centered(), area);
}
