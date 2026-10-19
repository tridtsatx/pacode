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
        lines.push(Line::from(vec![
            crate::ui::chrome::section("AGENTS", opts),
            Span::styled(
                format!("  {} done", finished_subagents.len()),
                opts.theme.faint,
            ),
        ]));

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

    // The running glyph breathes in its own hue: violet means "in progress"
    // everywhere in the rail, so the pulse dims it instead of borrowing green
    // (which already means "done").
    let lit = state.pulse_lit(std::time::Instant::now());
    let running_style = crate::ui::anim::pulse(&opts.theme, opts.theme.violet, lit);

    let title_text = "BACKGROUND";
    let mut right_spans = Vec::new();
    if running > 0 {
        right_spans.push(Span::styled(
            format!("{running} {} ", opts.glyphs.running),
            running_style,
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
            crate::ui::chrome::section(title_text, opts),
            Span::raw(" ".repeat(spaces)),
        ];
        header_spans.extend(right_spans);
        lines.push(Line::from(header_spans));
    }

    let now = now_ms();
    let fit = background_fit(
        area.height as usize,
        tasks.len(),
        state.rail.schedule_row_count(),
    );
    for t in tasks.iter().take(fit.tasks_shown) {
        lines.push(render_task_line(t, area.width as usize, opts, now, lit));
    }
    if fit.tasks_more {
        let more = tasks.len() - fit.tasks_shown;
        lines.push(Line::from(Span::styled(
            format!("  and {more} more"),
            opts.theme.faint,
        )));
    }

    // The schedule rows share this zone: a cron job or a monitor is background
    // work the reader watches the same way, and giving it its own zone would cost
    // a rail block that most sessions never use.
    if let Some(shown) = fit.schedule_shown {
        lines.extend(schedule_lines(state, area.width as usize, opts, now, shown));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// What the background zone fits: how many task rows (and whether an
/// `and N more` line follows them), then how many SCHEDULE rows under the
/// SCHEDULE header (`None`: no SCHEDULE block).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackgroundFit {
    pub(crate) tasks_shown: usize,
    pub(crate) tasks_more: bool,
    pub(crate) schedule_shown: Option<usize>,
}

pub(crate) fn background_fit(height: usize, tasks: usize, schedule_rows: usize) -> BackgroundFit {
    let header = usize::from(tasks > 0);
    let rem = height.saturating_sub(1);
    let (tasks_shown, tasks_more) = if tasks <= rem {
        (tasks, false)
    } else if rem > 1 {
        (rem - 1, true)
    } else {
        (0, false)
    };
    let used = header + tasks_shown + usize::from(tasks_more);
    let left = height.saturating_sub(used);
    // The header row, then every row when they fit; otherwise one row gives
    // way to an `and N more` line (the task list's rule). Reserving that line
    // unconditionally hid the last job of a zone sized exactly for its rows.
    let schedule_shown = (left > 1 && schedule_rows > 0).then(|| {
        if schedule_rows < left {
            schedule_rows
        } else {
            left - 2
        }
    });
    BackgroundFit {
        tasks_shown,
        tasks_more,
        schedule_shown,
    }
}

/// `SCHEDULE` header plus one row per enabled job and live monitor, newest last.
fn schedule_lines(
    state: &AppState,
    width: usize,
    opts: &RenderOptions,
    now: u64,
    shown: usize,
) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(crate::ui::chrome::section("SCHEDULE", opts))];
    let mut rows = Vec::new();

    for job in state.rail.cron_jobs.iter().filter(|j| j.enabled) {
        let due = job
            .next_run_ms
            .and_then(|at| next_run_label(at, now, &chrono::Local))
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

/// The next run as an absolute local time: `at 14:05` today, `Fri 09:00`
/// within six days, `Oct 17` further out. A countdown (`in 4m59s`) would go stale
/// on an idle screen unless a timer redrew it; a wall-clock time never does.
pub(crate) fn next_run_label<Tz: chrono::TimeZone>(
    at_ms: u64,
    now_ms: u64,
    tz: &Tz,
) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    let local = |ms: u64| tz.timestamp_millis_opt(i64::try_from(ms).ok()?).single();
    let (at, now) = (local(at_ms)?, local(now_ms)?);
    let days = (at.date_naive() - now.date_naive()).num_days();
    let fmt = match days {
        0 => "at %H:%M",
        -6..=6 => "%a %H:%M",
        _ => "%b %-d",
    };
    Some(at.format(fmt).to_string())
}

fn render_task_line(
    task: &pacode_types::TaskInfo,
    width: usize,
    opts: &RenderOptions,
    now: u64,
    lit: bool,
) -> Line<'static> {
    let dur = format_duration_ms(task.duration_ms(now));
    let (sym, sym_style) = match task.status {
        TaskStatus::Running => (
            opts.glyphs.running,
            crate::ui::anim::pulse(&opts.theme, opts.theme.violet, lit),
        ),
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
