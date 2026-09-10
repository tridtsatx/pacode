//! Agent / task panel inside the dialog column (mockup states 03, 04, 05).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::state::TaskStatus;
use pacode_types::time::{format_duration_ms, format_tokens, now_ms};

use crate::state::{AppState, Focus, PanelTarget};

pub fn draw(frame: &mut Frame, area: Rect, state: &mut AppState, opts: &RenderOptions) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let (target, follow, follow_paused) = match &state.focus {
        Focus::Panel {
            target,
            follow,
            follow_paused,
        } => (Some(target.clone()), *follow, *follow_paused),
        _ => (state.panel.target.clone(), false, false),
    };

    let Some(target) = target else {
        return;
    };

    match target {
        PanelTarget::Agent(ref agent_id) => {
            let now = now_ms();
            let agent = state.rail.agent(agent_id);
            let name = agent.map(|a| a.name.as_str()).unwrap_or("agent");
            let dur = agent
                .map(|a| format_duration_ms(a.duration_ms(now)))
                .unwrap_or_default();
            let act = agent.and_then(|a| a.activity.as_deref()).unwrap_or("idle");
            let tok = agent
                .map(|a| format_tokens(a.tokens_in))
                .unwrap_or_else(|| "0".to_string());

            let mut line1_spans = Vec::new();
            if follow {
                line1_spans.push(Span::styled("FOLLOW", opts.theme.selected_bg));
                line1_spans.push(Span::raw(" "));
                if follow_paused {
                    line1_spans.push(Span::styled("paused · ", opts.theme.accent));
                }
                line1_spans.push(Span::styled(name, opts.theme.bold));
                line1_spans.push(Span::styled(format!(" · {dur}"), opts.theme.dim));
            } else {
                line1_spans.push(Span::styled(
                    format!("{} ", opts.glyphs.pointer),
                    opts.theme.accent,
                ));
                line1_spans.push(Span::styled(name, opts.theme.bold));
                line1_spans.push(Span::styled(format!(" · {dur}"), opts.theme.dim));
            }

            let line2 = Line::from(Span::styled(format!("{act} · ↓ {tok}"), opts.theme.faint));

            let header_lines = vec![Line::from(line1_spans), line2, Line::default()];
            let header_area = Rect::new(area.x, area.y, area.width, 3.min(area.height));
            frame.render_widget(Paragraph::new(header_lines), header_area);

            if area.height > 3 {
                let trans_area = Rect::new(area.x, area.y + 3, area.width, area.height - 3);
                if follow && !follow_paused {
                    state.panel.agent_transcript.scroll_to_bottom();
                }
                crate::ui::dialog::draw(
                    frame,
                    trans_area,
                    &mut state.panel.agent_transcript,
                    opts,
                    state.anim_frame,
                    std::time::Instant::now()
                        .saturating_duration_since(state.started_at)
                        .as_millis() as u64,
                );
            }
        }
        PanelTarget::Task(ref task_id) => {
            let now = now_ms();
            let task = state.rail.task(task_id);
            let label = task.map(|t| t.label.as_str()).unwrap_or("task");
            let dur = task
                .map(|t| format_duration_ms(t.duration_ms(now)))
                .unwrap_or_default();
            let status = task.map(|t| t.status).unwrap_or(TaskStatus::Running);

            let (sym, sym_style) = match status {
                TaskStatus::Running => (opts.glyphs.running, opts.theme.violet),
                TaskStatus::Completed => (opts.glyphs.ok, opts.theme.green),
                TaskStatus::Failed | TaskStatus::Killed => (opts.glyphs.fail, opts.theme.red),
            };

            let prog_str = task
                .and_then(|t| t.progress.as_ref())
                .and_then(|p| p.short_label())
                .unwrap_or(dur.clone());

            let line1 = Line::from(vec![
                Span::styled(sym, sym_style),
                Span::raw(" "),
                Span::styled(label, opts.theme.bold),
                Span::styled(format!(" · {dur} · {prog_str}"), opts.theme.faint),
            ]);

            let header_lines = vec![line1, Line::default()];
            let header_area = Rect::new(area.x, area.y, area.width, 2.min(area.height));
            frame.render_widget(Paragraph::new(header_lines), header_area);

            if area.height > 2 {
                let output_area = Rect::new(area.x, area.y + 2, area.width, area.height - 2);
                let lines_count = state.panel.task_lines.len();
                let v_height = output_area.height as usize;

                let start_line = if status == TaskStatus::Running {
                    lines_count.saturating_sub(v_height)
                } else {
                    state.panel.scroll.min(lines_count.saturating_sub(v_height))
                };

                let visible: Vec<Line<'static>> = state
                    .panel
                    .task_lines
                    .iter()
                    .skip(start_line)
                    .take(v_height)
                    .map(|l| {
                        Line::from(Span::styled(
                            truncate_to_width(l, output_area.width as usize, true),
                            opts.theme.fg,
                        ))
                    })
                    .collect();

                frame.render_widget(Paragraph::new(visible), output_area);
            }
        }
    }
}
