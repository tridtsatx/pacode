//! The transcript (main dialog and the agent panel share this renderer).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::cache::CacheKey;
use pacode_render::markdown::split_stable_tail;
use pacode_render::{RenderOptions, render_diff, render_markdown, truncate_to_width, wrap_text};
use pacode_types::transcript::{DiffStat, ToolStatus};
use pacode_types::{ToastLevel, TranscriptKind};

use crate::state::transcript::{CellKind, Transcript};

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod dialog_tests;

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    transcript: &mut Transcript,
    opts: &RenderOptions,
    anim_frame: u64,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let width = area.width;
    let live_id = transcript.live_cell;

    let cell_snapshots: Vec<(u64, u32, CellKind, Option<String>)> = transcript
        .cells
        .iter()
        .map(|c| (c.id, c.version, c.kind.clone(), c.stats.clone()))
        .collect();

    let cell_lines = build_cell_lines(
        transcript,
        &cell_snapshots,
        live_id,
        width,
        opts,
        anim_frame,
    );

    let total_lines: usize = cell_lines.iter().map(|l| l.len()).sum();
    let viewport = area.height as usize;

    transcript.record_render(total_lines, viewport);

    let (start_line, is_scrolled_up) = if total_lines <= viewport {
        (0, false)
    } else {
        let max_scroll = total_lines.saturating_sub(viewport);
        let scroll = transcript.scroll_from_bottom;
        let start = max_scroll.saturating_sub(scroll);
        (start, scroll > 0)
    };

    let mut visible_lines: Vec<Line<'static>> = Vec::with_capacity(viewport);
    let mut current_idx = 0usize;

    for lines in cell_lines {
        for line in lines {
            if current_idx >= start_line && visible_lines.len() < viewport {
                visible_lines.push(line);
            }
            current_idx += 1;
        }
    }

    if is_scrolled_up && !visible_lines.is_empty() {
        let last_idx = visible_lines.len() - 1;
        let msg = format!("↓ {} new lines", transcript.scroll_from_bottom);
        visible_lines[last_idx] = Line::from(Span::styled(msg, opts.theme.dim));
    }

    frame.render_widget(Paragraph::new(visible_lines), area);
}

/// Render every block of the transcript in display order: the header banner
/// first (it lives outside `cells` so a transcript seq can never collide with
/// it), then one block per cell.
fn build_cell_lines(
    transcript: &mut Transcript,
    cell_snapshots: &[(u64, u32, CellKind, Option<String>)],
    live_id: Option<u64>,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
) -> Vec<Vec<Line<'static>>> {
    let mut cell_lines: Vec<Vec<Line<'static>>> = Vec::with_capacity(cell_snapshots.len() + 1);

    if let Some(info) = transcript.header.clone() {
        cell_lines.push(crate::ui::header::render(&info, width, opts, anim_frame));
    }

    for (id, version, kind, stats) in cell_snapshots {
        let is_live = live_id == Some(*id);
        let args = CellRenderArgs {
            cell_id: *id,
            cell_version: *version,
            cell_kind: kind,
            cell_stats: stats.as_deref(),
            is_live,
            width,
            anim_frame,
        };
        cell_lines.push(get_or_render_cell(args, opts, transcript));
    }

    cell_lines
}

/// The transcript as plain text, one entry per rendered line, in the same order
/// and with the same indices the selection uses. Rendering goes through the same
/// line cache as drawing, so this costs a walk of already-rendered lines.
pub(crate) fn plain_lines(
    transcript: &mut Transcript,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
) -> Vec<String> {
    let live_id = transcript.live_cell;
    let cell_snapshots: Vec<(u64, u32, CellKind, Option<String>)> = transcript
        .cells
        .iter()
        .map(|c| (c.id, c.version, c.kind.clone(), c.stats.clone()))
        .collect();
    build_cell_lines(
        transcript,
        &cell_snapshots,
        live_id,
        width,
        opts,
        anim_frame,
    )
    .into_iter()
    .flatten()
    .map(|line| {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    })
    .collect()
}

struct CellRenderArgs<'a> {
    cell_id: u64,
    cell_version: u32,
    cell_kind: &'a CellKind,
    cell_stats: Option<&'a str>,
    is_live: bool,
    width: u16,
    anim_frame: u64,
}

fn get_or_render_cell(
    args: CellRenderArgs<'_>,
    opts: &RenderOptions,
    transcript: &mut Transcript,
) -> Vec<Line<'static>> {
    let key = CacheKey {
        cell: args.cell_id ^ ((args.cell_version as u64) << 48),
        width: args.width,
    };

    let is_running_tool = matches!(
        args.cell_kind,
        CellKind::Item(TranscriptKind::ToolCall {
            status: ToolStatus::Running,
            ..
        })
    );

    if !args.is_live && !is_running_tool {
        if let Some(cached) = transcript.cache.get(key) {
            return cached.to_vec();
        }
        let rendered = render_cell(
            args.cell_kind,
            args.cell_stats,
            args.width,
            opts,
            args.anim_frame,
        );
        let cached = transcript.cache.insert(key, rendered);
        return cached.to_vec();
    }

    if let CellKind::Item(TranscriptKind::Assistant { text, .. }) = args.cell_kind {
        let (stable, tail) = split_stable_tail(text);
        let stable_key = CacheKey {
            cell: (args.cell_id << 1) ^ (stable.len() as u64) ^ 0x8000_0000_0000_0000,
            width: args.width,
        };

        let stable_lines = if let Some(cached) = transcript.cache.get(stable_key) {
            cached.to_vec()
        } else {
            let rendered = render_markdown(stable, opts);
            let cached = transcript.cache.insert(stable_key, rendered);
            cached.to_vec()
        };

        let mut tail_lines = render_markdown(tail, opts);
        let mut combined = stable_lines;
        combined.append(&mut tail_lines);
        if let Some(stats) = args.cell_stats {
            combined.push(Line::from(Span::styled(
                stats.to_string(),
                opts.theme.faint,
            )));
        }
        combined.push(Line::default());
        return combined;
    }

    let rendered = render_cell(
        args.cell_kind,
        args.cell_stats,
        args.width,
        opts,
        args.anim_frame,
    );
    if !is_running_tool {
        let cached = transcript.cache.insert(key, rendered);
        cached.to_vec()
    } else {
        rendered
    }
}

/// Test-only entry point: render one cell exactly as the transcript would.
#[cfg(test)]
pub(crate) fn render_cell_for_test(
    cell_kind: &CellKind,
    width: u16,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    render_cell(cell_kind, None, width, opts, 0)
}

fn render_cell(
    cell_kind: &CellKind,
    stats: Option<&str>,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
) -> Vec<Line<'static>> {
    match cell_kind {
        CellKind::Gap => vec![Line::default()],
        CellKind::Item(kind) => render_item(kind, stats, width, opts, anim_frame),
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

pub(crate) fn render_item(
    kind: &TranscriptKind,
    stats: Option<&str>,
    width: u16,
    opts: &RenderOptions,
    _anim_frame: u64,
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
                    let label = if let Some(ms) = duration_ms {
                        format!("running {}", pacode_types::time::format_duration_ms(*ms))
                    } else {
                        "running".to_string()
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
                        for pl in content_lines.iter().take(6) {
                            out.push(Line::from(vec![
                                Span::styled("  │ ", opts.theme.dim),
                                Span::styled(
                                    truncate_to_width(pl, (width as usize).saturating_sub(4), true),
                                    opts.theme.faint,
                                ),
                            ]));
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
