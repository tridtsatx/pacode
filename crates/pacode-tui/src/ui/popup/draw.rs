use ratatui::Frame;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use pacode_render::RenderOptions;

use crate::at_complete::AtCandidate;
use crate::bash::complete::Candidate;
use crate::commands::SlashCommand;

use super::{PreparedInlineContent, PreparedInlinePopup};

pub(super) fn draw(frame: &mut Frame, popup: &PreparedInlinePopup, opts: &RenderOptions) {
    match &popup.content {
        PreparedInlineContent::Slash(commands) => draw_slash(frame, popup, commands, opts),
        PreparedInlineContent::At(candidates) => draw_at(frame, popup, candidates, opts),
        PreparedInlineContent::Bash(candidates) => draw_bash(frame, popup, candidates, opts),
        PreparedInlineContent::PromptQueue(queue) => draw_queue(frame, popup, queue, opts),
    }
}

fn draw_slash(
    frame: &mut Frame,
    popup: &PreparedInlinePopup,
    commands: &[SlashCommand],
    opts: &RenderOptions,
) {
    frame.render_widget(Clear, popup.area);
    let lines: Vec<Line<'static>> = commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let selected = index + popup.window.start == popup.window.selected;
            let usage = format!("{:<12}", command.usage);
            let line = Line::from(vec![
                Span::styled(
                    usage,
                    if selected {
                        opts.theme
                            .selected_bg
                            .patch(opts.theme.cyan)
                            .patch(opts.theme.bold)
                    } else {
                        opts.theme.accent
                    },
                ),
                Span::raw(" "),
                Span::styled(
                    command.help.to_string(),
                    if selected {
                        opts.theme.selected_bg.patch(opts.theme.fg)
                    } else {
                        opts.theme.dim
                    },
                ),
            ]);
            if selected {
                crate::ui::overlays::selected_row(line, popup.area.width, opts)
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), popup.area);
}

fn draw_at(
    frame: &mut Frame,
    popup: &PreparedInlinePopup,
    candidates: &[AtCandidate],
    opts: &RenderOptions,
) {
    frame.render_widget(Clear, popup.area);
    let lines: Vec<Line<'static>> = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let selected = popup.window.start + index == popup.window.selected;
            let style = if selected {
                opts.theme
                    .selected_bg
                    .patch(opts.theme.cyan)
                    .patch(opts.theme.bold)
            } else if candidate.is_dir {
                opts.theme.cyan
            } else {
                opts.theme.fg
            };
            Line::from(Span::styled(candidate.path.clone(), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), popup.area);
}

fn draw_bash(
    frame: &mut Frame,
    popup: &PreparedInlinePopup,
    candidates: &[Candidate],
    opts: &RenderOptions,
) {
    frame.render_widget(Clear, popup.area);
    let lines: Vec<Line<'static>> = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let style = if popup.window.start + index == popup.window.selected {
                opts.theme
                    .selected_bg
                    .patch(opts.theme.cyan)
                    .patch(opts.theme.bold)
            } else {
                opts.theme.fg
            };
            Line::from(Span::styled(candidate.display.clone(), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), popup.area);
}

fn draw_queue(
    frame: &mut Frame,
    popup: &PreparedInlinePopup,
    queue: &[String],
    opts: &RenderOptions,
) {
    frame.render_widget(Clear, popup.area);
    let lines: Vec<Line<'static>> = queue
        .iter()
        .enumerate()
        .map(|(index, prompt)| {
            let prefix = format!(" queued #{}: ", popup.window.start + index + 1);
            let available = (popup.area.width as usize)
                .saturating_sub(pacode_render::display_width(&prefix))
                .max(1);
            let single_line = prompt.lines().next().unwrap_or("");
            Line::from(vec![
                Span::styled(prefix, opts.theme.accent),
                Span::styled(
                    pacode_render::truncate_to_width(single_line, available, true),
                    opts.theme.fg,
                ),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), popup.area);
}
