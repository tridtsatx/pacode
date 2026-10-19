//! Agent / task panel inside the dialog column (mockup states 03, 04, 05).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::state::TaskStatus;
use pacode_types::time::{format_duration_ms, format_tokens, now_ms};

use crate::layout::ScreenLayout;
use crate::state::files::FileStatus;
use crate::state::{AppState, Focus, PanelTarget};

#[cfg(test)]
#[path = "panel_tests.rs"]
mod panel_tests;

pub(crate) struct PreparedPanel {
    target: Option<PanelTarget>,
    transcript: Option<crate::ui::dialog::PreparedDialog>,
}

pub(crate) fn prepare(
    state: &mut AppState,
    layout: &ScreenLayout,
    welcome_ms: u64,
) -> Option<PreparedPanel> {
    let area = layout.panel?;
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let (target, follow, follow_paused) = match &state.focus {
        Focus::Panel {
            target,
            follow,
            follow_paused,
        } => (Some(target.clone()), *follow, *follow_paused),
        _ => (state.panel.target.clone(), false, false),
    };
    if area.height > 3
        && follow
        && !follow_paused
        && matches!(target.as_ref(), Some(PanelTarget::Agent(_)))
    {
        state.panel.agent_transcript.scroll_to_bottom();
    }
    let opts = state.render_options(area.width);
    let anim_frame = state.anim_frame;
    let (images_on, truecolor) = (state.config.ui.images_enabled(), state.truecolor);
    let transcript = match target.as_ref() {
        Some(PanelTarget::Agent(agent)) if area.height > 3 => {
            let owner = state.panel.agent_transcript_for(agent);
            owner.set_images(images_on, truecolor);
            Some(crate::ui::dialog::prepare(
                Rect::new(area.x, area.y + 3, area.width, area.height - 3),
                owner,
                &opts,
                anim_frame,
                welcome_ms,
            ))
        }
        _ => None,
    };
    if let Some(target) = target.as_ref() {
        state.panel.lines_for(target);
    }
    if area.height > 2 {
        let body = Rect::new(area.x, area.y + 2, area.width, area.height - 2);
        let mut diff_opts = opts.clone();
        diff_opts.width = body.width;
        let cache_key = diff_opts.cache_key();
        match target.as_ref() {
            Some(PanelTarget::FileDiff { path }) => {
                if let Some(diff) = state
                    .panel
                    .diff
                    .as_mut()
                    .filter(|diff| diff.path == *path && diff.patch.is_some())
                    && !matches!(&diff.rendered, Some((key, _)) if *key == cache_key)
                {
                    diff.rendered = Some((
                        cache_key,
                        pacode_render::render_diff_ex(
                            diff.patch.as_deref().unwrap_or_default(),
                            &diff_opts,
                            false,
                        ),
                    ));
                }
            }
            Some(PanelTarget::CommitDiff { hash }) => {
                if let Some(diff) = state
                    .git
                    .diff
                    .as_mut()
                    .filter(|diff| diff.hash == *hash && diff.patch.is_some())
                    && !matches!(&diff.rendered, Some((key, _)) if *key == cache_key)
                {
                    diff.rendered = Some((
                        cache_key,
                        pacode_render::render_diff_ex(
                            diff.patch.as_deref().unwrap_or_default(),
                            &diff_opts,
                            false,
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    Some(PreparedPanel { target, transcript })
}

pub(crate) fn draw(
    frame: &mut Frame,
    area: Rect,
    prepared: &PreparedPanel,
    state: &AppState,
    opts: &RenderOptions,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(target) = prepared.target.as_ref() else {
        return;
    };
    let (follow, follow_paused) = match &state.focus {
        Focus::Panel {
            follow,
            follow_paused,
            ..
        } => (*follow, *follow_paused),
        _ => (false, false),
    };

    match target {
        PanelTarget::Agent(agent_id) => {
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

            if area.height > 3
                && let Some(transcript) = prepared.transcript.as_ref()
            {
                let trans_area = Rect::new(area.x, area.y + 3, area.width, area.height - 3);
                crate::ui::dialog::draw(frame, trans_area, transcript);
            }
        }
        PanelTarget::McpServer(name) => {
            // The menu's `l` view (§5.3): a snapshot of the daemon log's tail
            // filtered to this server, already in `panel.task_lines`.
            let status = state
                .mcp_servers
                .iter()
                .find(|s| s.name == *name)
                .map(|s| s.status.as_str())
                .unwrap_or("stopped");
            let mark = crate::ui::menu::mcp::status_mark(status);
            let (sym, sym_style) = crate::ui::menu::mark_glyph(mark, opts)
                .unwrap_or((opts.glyphs.stopped, opts.theme.dim));

            let total = state.panel.task_total_lines;
            let line1 = Line::from(vec![
                Span::styled(sym, sym_style),
                Span::raw(" "),
                Span::styled(name.clone(), opts.theme.bold),
                Span::styled(format!(" · {status} · {total} log lines"), opts.theme.faint),
            ]);

            let header_lines = vec![line1, Line::default()];
            let header_area = Rect::new(area.x, area.y, area.width, 2.min(area.height));
            frame.render_widget(Paragraph::new(header_lines), header_area);

            if area.height > 2 {
                let output_area = Rect::new(area.x, area.y + 2, area.width, area.height - 2);
                let v_height = output_area.height as usize;

                // For this target `panel.scroll` is an offset from the tail
                // (nav::handle_scroll) — 0 pins the newest lines to the
                // bottom edge, like a task that is still running.
                let visible = state
                    .panel
                    .owned_lines(target)
                    .map(|lines| {
                        visible_lines(
                            lines,
                            output_area,
                            v_height,
                            state.panel.scroll,
                            opts.theme.fg,
                        )
                    })
                    .unwrap_or_default();
                frame.render_widget(Paragraph::new(visible), output_area);
            }
        }
        PanelTarget::FileDiff { path } => {
            // The Files level's `enter` (§5.5): the worktree diff the
            // deferred `file_diff` job produced — `panel.diff` — painted by
            // `render_diff_ex` and cached per width so a frame never re-does
            // the render (§8.1). A payload describing a *different* path is
            // stale — the panel reads as "loading", never as the wrong file.
            let scroll = state.panel.scroll;
            let (status_word, truncated) = match &state.panel.diff {
                Some(d) if d.path == *path => (
                    match d.status {
                        Some(FileStatus::New) => "new file",
                        Some(FileStatus::Deleted) => "deleted",
                        Some(FileStatus::Renamed) => "renamed",
                        Some(FileStatus::Modified) | None => "worktree",
                    },
                    d.truncated,
                ),
                _ => ("worktree", false),
            };

            let mut head = vec![
                Span::styled(format!("{} ", opts.glyphs.pointer), opts.theme.accent),
                Span::styled(path.clone(), opts.theme.bold),
                Span::styled(format!(" · {status_word}"), opts.theme.faint),
            ];
            if truncated {
                head.push(Span::styled(" · truncated", opts.theme.red));
            }
            let header_lines = vec![Line::from(head), Line::default()];
            let header_area = Rect::new(area.x, area.y, area.width, 2.min(area.height));
            frame.render_widget(Paragraph::new(header_lines), header_area);

            if area.height > 2 {
                let body = Rect::new(area.x, area.y + 2, area.width, area.height - 2);
                let current = matches!(&state.panel.diff, Some(d) if d.path == *path);
                match if current {
                    state.panel.diff.as_ref()
                } else {
                    None
                } {
                    Some(d) if d.patch.is_some() => {
                        if let Some((_, lines)) = &d.rendered {
                            // `panel.scroll` counts back from the tail like
                            // the log targets — `usize::MAX` (armed on open)
                            // pins the head, where a diff reads from.
                            let start = tail_start(lines.len(), body.height as usize, scroll);
                            let visible: Vec<Line<'static>> = lines
                                .iter()
                                .skip(start)
                                .take(body.height as usize)
                                .cloned()
                                .collect();
                            frame.render_widget(Paragraph::new(visible), body);
                        }
                    }
                    Some(d) => {
                        // No patch: `note` names why (not a repo, unreadable);
                        // bare `None`/`None` means the file is clean.
                        let msg = d
                            .note
                            .clone()
                            .unwrap_or_else(|| "no changes in the worktree".to_string());
                        let lines = vec![Line::from(Span::styled(msg, opts.theme.faint))];
                        frame.render_widget(Paragraph::new(lines), body);
                    }
                    None => {
                        let lines =
                            vec![Line::from(Span::styled("loading diff…", opts.theme.faint))];
                        frame.render_widget(Paragraph::new(lines), body);
                    }
                }
            }
        }
        PanelTarget::CommitDiff { hash } => {
            // The Git level's `enter` (§5.10): the commit's patch lives in
            // `state.git.diff`, keyed by hash — a payload for a different
            // commit reads as "loading", never as the wrong diff. Painted by
            // `render_diff_ex` and cached per width on the payload itself,
            // so a frame never re-renders (§8.1).
            let commit = state.git.commits.iter().find(|c| c.hash == *hash);
            let title = commit
                .map(|c| format!("{} {}", c.short, c.subject))
                .unwrap_or_else(|| hash.chars().take(12).collect());
            let current = matches!(&state.git.diff, Some(d) if d.hash == *hash);
            let truncated = current && state.git.diff.as_ref().is_some_and(|d| d.truncated);

            let mut head = vec![
                Span::styled(format!("{} ", opts.glyphs.pointer), opts.theme.accent),
                Span::styled(title, opts.theme.bold),
                Span::styled(" · commit".to_string(), opts.theme.faint),
            ];
            if truncated {
                head.push(Span::styled(" · truncated", opts.theme.red));
            }
            let header_lines = vec![Line::from(head), Line::default()];
            let header_area = Rect::new(area.x, area.y, area.width, 2.min(area.height));
            frame.render_widget(Paragraph::new(header_lines), header_area);

            if area.height > 2 {
                let body = Rect::new(area.x, area.y + 2, area.width, area.height - 2);
                let scroll = state.panel.scroll;
                let slot = if current {
                    state.git.diff.as_ref()
                } else {
                    None
                };
                match slot {
                    Some(d) if d.patch.is_some() => {
                        if let Some((_, lines)) = &d.rendered {
                            // `panel.scroll` counts back from the tail like
                            // the log targets — `usize::MAX` (armed on open)
                            // pins the head, where a diff reads from.
                            let start = tail_start(lines.len(), body.height as usize, scroll);
                            let visible: Vec<Line<'static>> = lines
                                .iter()
                                .skip(start)
                                .take(body.height as usize)
                                .cloned()
                                .collect();
                            frame.render_widget(Paragraph::new(visible), body);
                        }
                    }
                    Some(d) => {
                        // `note` names the failure (timeout, vanished ref); a
                        // bare `None` patch means the commit changed no files.
                        let msg = d
                            .note
                            .clone()
                            .unwrap_or_else(|| "no changes in this commit".to_string());
                        let lines = vec![Line::from(Span::styled(msg, opts.theme.faint))];
                        frame.render_widget(Paragraph::new(lines), body);
                    }
                    None => {
                        let lines =
                            vec![Line::from(Span::styled("loading diff…", opts.theme.faint))];
                        frame.render_widget(Paragraph::new(lines), body);
                    }
                }
            }
        }
        PanelTarget::Task(task_id) => {
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
                let v_height = output_area.height as usize;

                let visible = state
                    .panel
                    .owned_lines(target)
                    .map(|lines| {
                        visible_lines(
                            lines,
                            output_area,
                            v_height,
                            state.panel.scroll,
                            opts.theme.fg,
                        )
                    })
                    .unwrap_or_default();
                frame.render_widget(Paragraph::new(visible), output_area);
            }
        }
    }
}

/// The tail-anchored window a log-shaped panel shows: `scroll` counts back from
/// the newest line, 0 pinning it to the bottom edge.
fn visible_lines(
    lines: &[String],
    area: Rect,
    viewport: usize,
    scroll: usize,
    style: ratatui::style::Style,
) -> Vec<Line<'static>> {
    let start_line = tail_start(lines.len(), viewport, scroll);
    lines
        .iter()
        .skip(start_line)
        .take(viewport)
        .map(|line| {
            Line::from(Span::styled(
                truncate_to_width(line, area.width as usize, true),
                style,
            ))
        })
        .collect()
}

fn tail_start(lines: usize, viewport: usize, scroll: usize) -> usize {
    let max_start = lines.saturating_sub(viewport);
    max_start.saturating_sub(scroll.min(max_start))
}
