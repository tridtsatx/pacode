//! The transcript (main dialog and the agent panel share this renderer).

use ratatui::text::{Line, Span};

use pacode_render::{RenderOptions, render_diff, render_markdown, truncate_to_width, wrap_text};
use pacode_types::transcript::{DiffStat, ToolStatus};
use pacode_types::{ToastLevel, TranscriptKind};

use crate::state::transcript::CellKind;

#[path = "dialog_layout.rs"]
mod layout;

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod dialog_tests;

pub use layout::draw;
pub(crate) use layout::plain_lines;

/// Test-only entry point: render one cell exactly as the transcript would.
#[cfg(test)]
pub(crate) fn render_cell_for_test(
    cell_kind: &CellKind,
    width: u16,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    render_cell(cell_kind, None, width, opts, 0, false)
}

fn render_cell(
    cell_kind: &CellKind,
    stats: Option<&str>,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
    expanded: bool,
) -> Vec<Line<'static>> {
    match cell_kind {
        CellKind::Gap => vec![Line::default()],
        CellKind::Item(kind) => render_item_inner(kind, stats, width, opts, anim_frame, expanded),
        CellKind::BackgroundResult(result) => render_background_result(result, width, opts),
    }
}

/// A finished background job, drawn in the tool-call idiom: symbol, what ran,
/// then the outcome with its duration and exit code.
fn render_background_result(
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

    vec![Line::from(vec![
        Span::styled(head, opts.theme.cyan),
        Span::styled(label, opts.theme.dim),
        Span::raw(" "),
        Span::styled(tail, style),
    ])]
}

/// Render one item in its folded form. Only tests reach for this directly; the
/// transcript goes through the cache, which knows whether the cell is expanded.
#[cfg(test)]
pub(crate) fn render_item(
    kind: &TranscriptKind,
    stats: Option<&str>,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
) -> Vec<Line<'static>> {
    render_item_inner(kind, stats, width, opts, anim_frame, false)
}

/// Output lines shown on a collapsed tool call, and the ceiling on an expanded
/// one: an expansion is a reading aid, not a reason to render a 200 MB log.
pub const COLLAPSED_MAX_LINES: usize = 6;
pub const EXPANDED_MAX_LINES: usize = 400;

/// `expanded` shows a tool call's whole output, wrapped, instead of its first
/// few lines cut to the width.
fn render_item_inner(
    kind: &TranscriptKind,
    stats: Option<&str>,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
    expanded: bool,
) -> Vec<Line<'static>> {
    match kind {
        TranscriptKind::User { text } => {
            let bar = if opts.glyphs.ascii { "|" } else { "▎" };
            let prefix = format!("{bar} ");
            let avail = (width as usize).saturating_sub(2).max(1);
            let lines = wrap_text(text, avail);
            let mut out = Vec::new();
            for (i, l) in lines.into_iter().enumerate() {
                let style = if i == 0 {
                    opts.theme.bold
                } else {
                    opts.theme.fg
                };
                out.push(Line::from(vec![
                    Span::styled(prefix.clone(), opts.theme.user_bar),
                    Span::styled(l, style),
                ]));
            }
            out.push(Line::default());
            out
        }
        TranscriptKind::Assistant { text, complete } => {
            if text.is_empty() && !complete {
                return vec![
                    Line::from(Span::styled("…", opts.theme.faint)),
                    Line::default(),
                ];
            }
            let mut lines = render_markdown(text, opts);
            if let Some(stats) = stats {
                lines.push(Line::from(Span::styled(
                    stats.to_string(),
                    opts.theme.faint,
                )));
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
        } => {
            let mut out = Vec::new();
            let sym = opts.glyphs.tool;
            // The core builds titles as `<name> <arg>`; show `▣ Name arg` (mockup).
            let rest = title
                .strip_prefix(name.as_str())
                .map(str::trim_start)
                .unwrap_or(title.as_str());
            let mut display_name = String::new();
            let mut chars = name.chars();
            if let Some(first) = chars.next() {
                display_name.extend(first.to_uppercase());
                display_name.push_str(chars.as_str());
            }
            let mut spans = vec![
                Span::styled(format!("{sym} {display_name} "), opts.theme.cyan),
                Span::styled(rest.to_string(), opts.theme.dim),
            ];

            let exit_code = if *status == ToolStatus::Error {
                parse_exit_code(preview)
            } else {
                None
            };

            match status {
                ToolStatus::Running => {
                    spans.push(Span::raw(" "));
                    // The cell bypasses the line cache while running, so the
                    // braille spinner can turn on the anim frame alone.
                    let spinner = opts.glyphs.spinner(anim_frame);
                    let label = if let Some(ms) = duration_ms {
                        format!(
                            "{spinner} running {}",
                            pacode_types::time::format_duration_ms(*ms)
                        )
                    } else {
                        format!("{spinner} running")
                    };
                    spans.push(Span::styled(label, opts.theme.accent));
                }
                ToolStatus::Ok => {
                    spans.push(Span::raw(" "));
                    if let Some(diff) = diff {
                        spans.extend(render_diff_stat(diff, opts));
                    } else if let Some(ms) = duration_ms {
                        spans.push(Span::styled(
                            format!("ok {}", pacode_types::time::format_duration_ms(*ms)),
                            opts.theme.green,
                        ));
                    } else {
                        spans.push(Span::styled(opts.glyphs.ok, opts.theme.green));
                    }
                }
                ToolStatus::Error => {
                    spans.push(Span::raw(" "));
                    let mut label = match duration_ms {
                        Some(ms) => {
                            format!("fail {}", pacode_types::time::format_duration_ms(*ms))
                        }
                        None => "fail".to_string(),
                    };
                    if let Some(code) = exit_code {
                        label.push_str(&format!(" · exit {code}"));
                    }
                    spans.push(Span::styled(label, opts.theme.red));
                }
                ToolStatus::Backgrounded => {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        format!("{} background", opts.glyphs.arrow_right),
                        opts.theme.violet,
                    ));
                }
                ToolStatus::Denied => {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled("denied", opts.theme.red));
                }
            }
            out.push(Line::from(spans));

            let is_exit_line = |l: &str| {
                let t = l.trim();
                t.starts_with("[exit code ") && t.ends_with(']')
            };

            if !preview.is_empty() {
                if preview.starts_with("--- ") || preview.contains("\n+++ ") {
                    let diff_lines = render_diff(preview, opts);
                    for dl in diff_lines {
                        let mut spans = vec![Span::styled("  │ ", opts.theme.dim)];
                        spans.extend(dl.spans);
                        out.push(Line::from(spans));
                    }
                } else {
                    let content_lines: Vec<&str> = preview
                        .lines()
                        .filter(|l| !(*status == ToolStatus::Error && is_exit_line(l)))
                        .collect();

                    if content_lines.iter().any(|l| !l.trim().is_empty()) {
                        let body_width = (width as usize).saturating_sub(4);
                        if expanded {
                            // Expanded: the whole output, wrapped rather than cut,
                            // and still bounded so one enormous result cannot make
                            // the transcript unscrollable.
                            let mut drawn = 0usize;
                            for pl in &content_lines {
                                for wrapped in pacode_render::wrap_text(pl, body_width) {
                                    out.push(Line::from(vec![
                                        Span::styled("  │ ", opts.theme.dim),
                                        Span::styled(wrapped, opts.theme.faint),
                                    ]));
                                    drawn += 1;
                                    if drawn >= EXPANDED_MAX_LINES {
                                        break;
                                    }
                                }
                                if drawn >= EXPANDED_MAX_LINES {
                                    out.push(Line::from(Span::styled(
                                        "  │ [output truncated]",
                                        opts.theme.dim,
                                    )));
                                    break;
                                }
                            }
                        } else {
                            let hidden = content_lines.len().saturating_sub(COLLAPSED_MAX_LINES);
                            for pl in content_lines.iter().take(COLLAPSED_MAX_LINES) {
                                out.push(Line::from(vec![
                                    Span::styled("  │ ", opts.theme.dim),
                                    Span::styled(
                                        truncate_to_width(pl, body_width, true),
                                        opts.theme.faint,
                                    ),
                                ]));
                            }
                            // Say there is more, and how to see it.
                            if hidden > 0 {
                                out.push(Line::from(Span::styled(
                                    format!("  │ +{hidden} more lines · click to expand"),
                                    opts.theme.dim,
                                )));
                            }
                        }
                    }
                }
            }

            out.push(Line::default());
            out
        }
        TranscriptKind::Notice { level, text } => {
            let style = match level {
                ToastLevel::Success => opts.theme.green,
                ToastLevel::Warn => opts.theme.accent,
                ToastLevel::Error => opts.theme.red,
                ToastLevel::Info => opts.theme.faint,
            };
            vec![
                Line::from(Span::styled(text.clone(), style)),
                Line::default(),
            ]
        }
        TranscriptKind::Permission(req) => {
            let line1 = Line::from(vec![
                Span::styled(
                    format!("{} permission: ", opts.glyphs.chevrons),
                    opts.theme.accent,
                ),
                Span::styled(req.title.clone(), opts.theme.bold),
            ]);
            let mut lines = vec![line1];
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
            lines.push(Line::from(Span::styled(
                "y allow · a session · n deny",
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
                    }
                    if let Some(text) = a.free_text.as_ref().filter(|t| !t.trim().is_empty()) {
                        lines.push(Line::from(Span::styled(
                            format!("typed: {text}"),
                            opts.theme.faint,
                        )));
                    }
                }
            }
            lines.push(Line::default());
            lines
        }
        // Cache-free fallback; the transcript draws images through `ui::images`.
        TranscriptKind::Images { images } => crate::ui::images::stub_lines(images, width, opts),
        TranscriptKind::BashCommand {
            command,
            output,
            exit_code,
            truncated: _,
        } => {
            let mut out = Vec::new();
            let mut header_spans = vec![
                Span::styled(
                    "! ",
                    opts.theme
                        .accent
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(command.clone(), opts.theme.bold),
            ];

            if let Some(code) = exit_code
                && *code != 0
            {
                header_spans.push(Span::raw(" "));
                header_spans.push(Span::styled(format!("(exit {code})"), opts.theme.red));
            }
            out.push(Line::from(header_spans));

            if !output.is_empty() {
                for line in output.lines() {
                    out.push(Line::from(vec![
                        Span::styled("  │ ", opts.theme.dim),
                        Span::styled(
                            truncate_to_width(line, (width as usize).saturating_sub(4), true),
                            opts.theme.fg,
                        ),
                    ]));
                }
            }

            out.push(Line::default());
            out
        }
    }
}

pub(crate) fn parse_exit_code(preview: &str) -> Option<i32> {
    for line in preview.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("[exit code ")
            && let Some(num_str) = rest.strip_suffix(']')
            && let Ok(code) = num_str.trim().parse::<i32>()
        {
            return Some(code);
        }
    }
    if let Some(start) = preview.find("[exit code ") {
        let rest = &preview[start + "[exit code ".len()..];
        if let Some(end) = rest.find(']')
            && let Ok(code) = rest[..end].trim().parse::<i32>()
        {
            return Some(code);
        }
    }
    None
}

fn render_diff_stat(diff: &DiffStat, opts: &RenderOptions) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("+{}", diff.added), opts.theme.green),
        Span::raw(" "),
        Span::styled(format!("-{}", diff.removed), opts.theme.red),
    ]
}
