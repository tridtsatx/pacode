//! How each transcript item draws: the user band, the assistant's marker,
//! tool calls (via `toolcell`), notices, permissions, questions, `!cmd`
//! results and finished background jobs. `dialog` decides *which* cells draw
//! and caches them; this module decides what one cell looks like.

use ratatui::text::{Line, Span};

use pacode_render::{RenderOptions, display_width, render_markdown, truncate_to_width, wrap_text};
use pacode_types::transcript::ToolStatus;
use pacode_types::{ExplainLevel, RiskLevel, ToastLevel, TranscriptKind};

use crate::state::transcript::{CellKind, PermExplain, ToolStreamView};

#[cfg(test)]
#[path = "cells_tests.rs"]
mod cells_tests;

/// Width the assistant marker (`● `) and its hanging indent take.
const ASSISTANT_INDENT: usize = 2;

/// The user's message as a plaque: a full-width band on `surface`, the accent
/// bar `▌` at its left edge, the text bold, one empty band row above and below
/// the text so the plaque reads as a block (omp's user bubble). Under ANSI-16
/// and mono the band has no background, and the bar plus the weight carry it.
fn user_band(text: &str, opts: &RenderOptions) -> Vec<Line<'static>> {
    let width = opts.width as usize;
    let band = opts.theme.surface;
    let prefix = format!("{} ", crate::ui::chrome::band_bar(opts));
    let prefix_w = display_width(&prefix);
    let avail = width.saturating_sub(prefix_w).max(1);
    let row = |l: String| {
        let pad = width.saturating_sub(prefix_w + display_width(&l));
        Line::from(vec![
            Span::styled(prefix.clone(), opts.theme.user_bar.patch(band)),
            Span::styled(l, opts.theme.bold.patch(band)),
            Span::styled(" ".repeat(pad), band),
        ])
    };
    let mut out = vec![row(String::new())];
    out.extend(wrap_text(text, avail).into_iter().map(row));
    out.push(row(String::new()));
    out.push(Line::default());
    out
}

/// The assistant's markdown with its marker: `● ` on the first row, a
/// two-cell hanging indent on the rest, rendered two cells narrower so the
/// indent never pushes a row past the edge. `marked` is false for the tail of
/// a live answer whose stable part already carries the marker.
pub(super) fn assistant_markdown(
    text: &str,
    opts: &RenderOptions,
    marked: bool,
) -> Vec<Line<'static>> {
    let mut inner = opts.clone();
    inner.width = opts.width.saturating_sub(ASSISTANT_INDENT as u16).max(1);
    let marker = format!("{} ", opts.glyphs.main_dot);
    render_markdown(text, &inner)
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let lead = if i == 0 && marked {
                Span::styled(marker.clone(), opts.theme.accent)
            } else {
                Span::raw(" ".repeat(ASSISTANT_INDENT))
            };
            let mut spans = Vec::with_capacity(line.spans.len() + 1);
            spans.push(lead);
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect()
}

/// The turn's stats under an answer, on the answer's hanging indent.
pub(super) fn assistant_stats_line(stats: &str, opts: &RenderOptions) -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(ASSISTANT_INDENT)),
        Span::styled(stats.to_string(), opts.theme.faint),
    ])
}

/// How many leading columns of a rendered row are decoration rather than
/// text — the user band's `▌ `, the answer's `● ` and its hanging indent, the
/// ` │ ` / ` └ ` gutter of a tool body or an error notice. A copy drops them.
///
/// Every decoration this module draws is the row's first span, so the check
/// is "is the first span exactly the decoration this kind of cell draws":
/// text that merely starts with spaces or a bar is never mistaken for one.
pub(crate) fn decoration_width(kind: &CellKind, line: &Line<'_>, opts: &RenderOptions) -> usize {
    let Some(first) = line.spans.first() else {
        return 0;
    };
    let lead = first.content.as_ref();
    let gutter = |lead: &str| {
        lead == format!(" {} ", opts.glyphs.vline) || lead == format!(" {} ", opts.glyphs.vcorner)
    };
    let decorated = match kind {
        CellKind::Gap | CellKind::BackgroundResult(_) => false,
        CellKind::Item(item) => match item {
            TranscriptKind::User { .. } => {
                lead == format!("{} ", crate::ui::chrome::band_bar(opts))
            }
            TranscriptKind::Assistant { .. } => {
                lead == format!("{} ", opts.glyphs.main_dot) || lead == " ".repeat(ASSISTANT_INDENT)
            }
            TranscriptKind::ToolCall { .. } | TranscriptKind::BashCommand { .. } => gutter(lead),
            TranscriptKind::Notice { level, .. } => *level == ToastLevel::Error && gutter(lead),
            TranscriptKind::Reasoning { .. }
            | TranscriptKind::Permission(_)
            | TranscriptKind::Question { .. }
            | TranscriptKind::AgentMessage { .. } => false,
        },
    };
    if decorated { display_width(lead) } else { 0 }
}

/// A finished background job, drawn in the tool-call idiom: symbol, what ran,
/// then the outcome with its duration and exit code.
pub(super) fn render_background_result(
    result: &crate::state::BackgroundResult,
    width: u16,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    use crate::state::{BackgroundKind, BackgroundOutcome};

    let noun = match result.kind {
        BackgroundKind::Task => "Background",
        BackgroundKind::Agent => "Agent",
    };
    let dur = pacode_types::time::format_duration_ms(result.duration_ms);
    let (word, style) = match result.outcome {
        BackgroundOutcome::Completed => ("done", opts.theme.green),
        BackgroundOutcome::Failed => ("failed", opts.theme.red),
        BackgroundOutcome::Killed => ("killed", opts.theme.yellow),
    };
    let mut tail = format!("{word} {dur}");
    if let Some(code) = result.exit_code
        && code != 0
    {
        tail.push_str(&format!(" · exit {code}"));
    }

    // The label is the part that can be arbitrarily long, so it is the part that
    // gets an ellipsis: the outcome must stay readable at any width.
    let sym = opts.glyphs.tool;
    let head = format!("{sym} {noun} ");
    let fixed = pacode_render::display_width(&head) + pacode_render::display_width(&tail) + 1;
    let label = truncate_to_width(&result.label, (width as usize).saturating_sub(fixed), true);

    // The job's outcome tints the row the way a tool call's does; a job
    // stopped on purpose is no failure, so it takes the neutral pending tint.
    let bg = match result.outcome {
        BackgroundOutcome::Completed => opts.theme.tool_success_bg,
        BackgroundOutcome::Failed => opts.theme.tool_error_bg,
        BackgroundOutcome::Killed => opts.theme.tool_pending_bg,
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(head, opts.theme.cyan),
        Span::styled(label, opts.theme.dim),
        Span::raw(" "),
        Span::styled(tail, style),
    ])];
    crate::ui::toolcell::paint_block(&mut lines, bg, width as usize);
    lines
}

/// `expanded` shows a tool call's whole output, wrapped, instead of its first
/// few lines cut to the width. `perm_explain` rides along for permission
/// cells — the `e`-key answer is cell state, not part of the wire item.
/// `streamed` carries the output a tool call has produced while it is still
/// running; only a `Running` tool call reads it.
pub(super) fn render_item_inner(
    kind: &TranscriptKind,
    perm_explain: Option<&PermExplain>,
    stats: Option<&str>,
    streamed: Option<ToolStreamView<'_>>,
    opts: &RenderOptions,
    anim_frame: u64,
    expanded: bool,
) -> Vec<Line<'static>> {
    // The width comes from the options rather than beside them: `RenderOptions`
    // carries it, and a second copy is a second answer to "how wide is this".
    let width = opts.width;
    match kind {
        TranscriptKind::User { text } => user_band(text, opts),
        TranscriptKind::Assistant { text, complete } => {
            if text.is_empty() && !complete {
                return vec![
                    Line::from(vec![
                        Span::styled(format!("{} ", opts.glyphs.main_dot), opts.theme.accent),
                        Span::styled(opts.glyphs.ellipsis, opts.theme.faint),
                    ]),
                    Line::default(),
                ];
            }
            let mut lines = assistant_markdown(text, opts, true);
            if let Some(stats) = stats {
                lines.push(assistant_stats_line(stats, opts));
            }
            lines.push(Line::default());
            lines
        }
        TranscriptKind::Reasoning { text, complete } => {
            if !opts.thinking {
                return Vec::new();
            }
            let line = if *complete {
                Line::from(Span::styled("thought for a moment", opts.theme.faint))
            } else if text.is_empty() {
                Line::from(Span::styled("thinking…", opts.theme.faint))
            } else {
                Line::from(Span::styled(
                    format!(
                        "thinking: {}",
                        truncate_to_width(text, (width as usize).saturating_sub(11), true)
                    ),
                    opts.theme.faint,
                ))
            };
            vec![line, Line::default()]
        }
        TranscriptKind::ToolCall {
            call_id: _,
            name,
            title,
            intent: _,
            status,
            preview,
            diff,
            duration_ms,
            task: _,
        } => crate::ui::toolcell::render(
            &crate::ui::toolcell::ToolCallView {
                name,
                title,
                status: *status,
                preview,
                diff: diff.as_ref(),
                duration_ms: *duration_ms,
                streamed,
            },
            width,
            opts,
            anim_frame,
            expanded,
        ),
        TranscriptKind::Notice { level, text } => {
            let style = match level {
                ToastLevel::Success => opts.theme.green,
                ToastLevel::Warn => opts.theme.accent,
                ToastLevel::Error => opts.theme.red,
                ToastLevel::Info => opts.theme.faint,
            };
            if *level == ToastLevel::Error {
                crate::ui::notice::render_error(text, width, opts)
            } else {
                let mut out: Vec<Line<'static>> = wrap_text(text, (width as usize).max(1))
                    .into_iter()
                    .map(|l| Line::from(Span::styled(l, style)))
                    .collect();
                out.push(Line::default());
                out
            }
        }
        TranscriptKind::Permission(req) => {
            // The static classifier's risk rides the header as a chip; the
            // title yields width first so the chip can never be cut off.
            let head = format!("{} permission: ", opts.glyphs.chevrons);
            let chip = req
                .risk
                .map(|r| (format!("risk: {}", risk_label(r)), risk_style(r, opts)));
            let chip_w = chip
                .as_ref()
                .map(|(t, _)| display_width(t) + display_width(opts.glyphs.dot_sep) + 2)
                .unwrap_or(0);
            let title_w = (width as usize)
                .saturating_sub(display_width(&head) + chip_w)
                .max(1);
            let mut head_spans = vec![
                Span::styled(head, opts.theme.accent),
                Span::styled(
                    truncate_to_width(&req.title, title_w, true),
                    opts.theme.bold,
                ),
            ];
            if let Some((text, style)) = chip {
                head_spans.push(Span::styled(
                    format!(" {} ", opts.glyphs.dot_sep),
                    opts.theme.faint,
                ));
                head_spans.push(Span::styled(text, style));
            }
            let mut lines = vec![Line::from(head_spans)];
            if !req.detail.is_empty() {
                for dl in req.detail.lines().take(4) {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "    {}",
                            truncate_to_width(dl, (width as usize).saturating_sub(4), true)
                        ),
                        opts.theme.dim,
                    )));
                }
            }
            match perm_explain {
                // `e` pressed — the aux answer is still in flight.
                Some(PermExplain::Requested) => lines.push(Line::from(Span::styled(
                    "    explain requested…".to_string(),
                    opts.theme.faint,
                ))),
                // The aux model's answer landed — display-only, the verdict
                // keys below stay live while it is read.
                Some(PermExplain::Done { text, level }) => {
                    lines.extend(render_explain_lines(text, *level, width, opts));
                }
                None => {}
            }
            lines.push(Line::from(Span::styled(
                "y allow · a session · n deny · e explain",
                opts.theme.cyan,
            )));
            lines.push(Line::default());
            lines
        }
        TranscriptKind::Question { question, answer } => {
            let mut lines = vec![Line::from(vec![
                Span::styled(format!("{} ", opts.glyphs.chevrons), opts.theme.accent),
                Span::styled(question.header.clone(), opts.theme.selected_bg),
            ])];
            for line in pacode_render::wrap_text(&question.question, width as usize) {
                lines.push(Line::from(Span::styled(line, opts.theme.bold)));
            }

            match answer {
                // Unanswered: the picker below is showing the options and the
                // keys, so repeating them here would put the same card on screen
                // twice.
                None => lines.push(Line::from(Span::styled(
                    "waiting for your answer",
                    opts.theme.faint,
                ))),
                Some(a) if a.cancelled => {
                    lines.push(Line::from(Span::styled("dismissed", opts.theme.faint)));
                }
                Some(a) => {
                    for (i, option) in question.options.iter().enumerate() {
                        let chosen = a.selected.contains(&i);
                        let marker = if chosen { opts.glyphs.ok } else { " " };
                        let mut label = format!("{}. {}", i + 1, option.label);
                        if option.recommended {
                            label.push_str(" (recommended)");
                        }
                        lines.push(Line::from(vec![
                            Span::styled(format!("{marker} "), opts.theme.green),
                            Span::styled(
                                truncate_to_width(&label, (width as usize).saturating_sub(3), true),
                                if chosen {
                                    opts.theme.bold
                                } else {
                                    opts.theme.dim
                                },
                            ),
                        ]));
                        // The chosen option keeps its description on record —
                        // the picker's "what this means" is part of the answer.
                        if chosen && !option.description.is_empty() {
                            for dl in wrap_text(
                                &option.description,
                                (width as usize).saturating_sub(4).max(1),
                            ) {
                                lines.push(Line::from(Span::styled(
                                    format!("    {dl}"),
                                    opts.theme.dim,
                                )));
                            }
                        }
                    }
                    if let Some(text) = a.free_text.as_ref().filter(|t| !t.trim().is_empty()) {
                        for (j, tl) in wrap_text(text, (width as usize).saturating_sub(2).max(1))
                            .into_iter()
                            .enumerate()
                        {
                            let prefix = if j == 0 { "> " } else { "  " };
                            lines.push(Line::from(Span::styled(
                                format!("{prefix}{tl}"),
                                opts.theme.faint,
                            )));
                        }
                    }
                }
            }
            lines.push(Line::default());
            lines
        }
        TranscriptKind::BashCommand {
            command,
            output,
            exit_code,
            truncated,
        } => bash_command(
            BashResult {
                command,
                output,
                exit_code: *exit_code,
                truncated: *truncated,
            },
            opts,
            anim_frame,
            expanded,
        ),
        TranscriptKind::AgentMessage {
            subagent: _,
            subagent_name,
            direction,
            text,
            reply,
        } => crate::ui::agent_message::render(
            &crate::ui::agent_message::AgentMessageView {
                subagent_name,
                direction: *direction,
                text,
                reply: reply.as_ref(),
            },
            opts,
            expanded,
        ),
    }
}

/// The fields of a `!cmd` result the cell reads.
struct BashResult<'a> {
    command: &'a str,
    output: &'a str,
    exit_code: Option<i32>,
    truncated: bool,
}

/// The user's own `!cmd` drawn as a bash tool call: the same badge, the same
/// fold with its `ctrl+o` expansion, the command cut to the header and spelled
/// out whole when expanded — and a capped output says so.
fn bash_command(
    run: BashResult<'_>,
    opts: &RenderOptions,
    anim_frame: u64,
    expanded: bool,
) -> Vec<Line<'static>> {
    // A tool title is `<name> <arg>`; the cell strips the name back off.
    let title = format!("bash {}", run.command);
    let mut preview = run.output.to_string();
    if run.truncated {
        if !preview.is_empty() && !preview.ends_with('\n') {
            preview.push('\n');
        }
        preview.push_str(&format!("{} output truncated", opts.glyphs.ellipsis));
    }
    if let Some(code) = run.exit_code {
        preview.push_str(&format!("\n[exit code {code}]"));
    }
    let status = match run.exit_code {
        Some(code) if code != 0 => ToolStatus::Error,
        Some(_) | None => ToolStatus::Ok,
    };
    crate::ui::toolcell::render(
        &crate::ui::toolcell::ToolCallView {
            name: "bash",
            title: &title,
            status,
            preview: &preview,
            diff: None,
            duration_ms: None,
            streamed: None,
        },
        opts.width,
        opts,
        anim_frame,
        expanded,
    )
}

/// The `e`-key answer under a pending permission: `explain · <level>:` in
/// the rating's colour, then the aux model's text dim-wrapped beneath it.
/// `level: None` is the daemon's honest-failure path — the label drops the
/// rating word and the text says what went wrong. Capped at a few lines: the
/// daemon already caps the answer at ~2k chars, but a narrow pane must not
/// let it push the verdict hints off the bottom of the cell.
fn render_explain_lines(
    text: &str,
    level: Option<ExplainLevel>,
    width: u16,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    /// The most wrapped lines a permission explanation may occupy.
    const EXPLAIN_MAX_LINES: usize = 8;
    let indent = "    ";
    let mut label = vec![Span::styled(format!("{indent}explain"), opts.theme.faint)];
    if let Some(lv) = level {
        label.push(Span::styled(
            opts.glyphs.dot_sep.to_string(),
            opts.theme.faint,
        ));
        label.push(Span::styled(
            crate::state::transcript::explain_level_name(lv),
            explain_level_style(lv, opts),
        ));
    }
    label.push(Span::styled(": ".to_string(), opts.theme.faint));
    let label_w: usize = label.iter().map(|s| display_width(&s.content)).sum();
    let text_w = (width as usize).saturating_sub(label_w).max(1);
    let wrapped = wrap_text(text, text_w);

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut first = label;
    if let Some(head) = wrapped.first() {
        first.push(Span::styled(head.clone(), opts.theme.dim));
    }
    out.push(Line::from(first));
    for wl in wrapped.iter().skip(1).take(EXPLAIN_MAX_LINES - 1) {
        out.push(Line::from(Span::styled(
            format!("{indent}{wl}"),
            opts.theme.dim,
        )));
    }
    let hidden = wrapped.len().saturating_sub(EXPLAIN_MAX_LINES);
    if hidden > 0 {
        out.push(Line::from(Span::styled(
            format!(
                "{indent}{} {hidden} more {}",
                opts.glyphs.ellipsis,
                pacode_render::text::lines_noun(hidden)
            ),
            opts.theme.faint,
        )));
    }
    out
}

/// Colour for the aux model's danger rating: `none` stays quiet, `low` is
/// fine, `medium` asks attention, `high` is red, `catastrophic` shouts.
fn explain_level_style(level: ExplainLevel, opts: &RenderOptions) -> ratatui::style::Style {
    match level {
        ExplainLevel::None => opts.theme.faint,
        ExplainLevel::Low => opts.theme.green,
        ExplainLevel::Medium => opts.theme.yellow,
        ExplainLevel::High => opts.theme.red,
        ExplainLevel::Catastrophic => opts.theme.red.add_modifier(ratatui::style::Modifier::BOLD),
    }
}

/// The `risk: <word>` chip on a permission header — the static classifier's
/// vocabulary (`pacode-risk`), kept lowercase like the explain rating.
fn risk_label(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::Safe => "safe",
        RiskLevel::Low => "low",
        RiskLevel::Confirm => "confirm",
        RiskLevel::Catastrophic => "catastrophic",
    }
}

/// Same colour ladder as [`explain_level_style`], mapped onto the static
/// risk classes: `safe` is quiet, `low` fine, `confirm` needs a look, and
/// `catastrophic` shouts.
fn risk_style(risk: RiskLevel, opts: &RenderOptions) -> ratatui::style::Style {
    match risk {
        RiskLevel::Safe => opts.theme.faint,
        RiskLevel::Low => opts.theme.green,
        RiskLevel::Confirm => opts.theme.yellow,
        RiskLevel::Catastrophic => opts.theme.red.add_modifier(ratatui::style::Modifier::BOLD),
    }
}
