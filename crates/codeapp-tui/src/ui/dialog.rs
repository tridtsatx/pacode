//! The transcript (main dialog and the agent panel share this renderer).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use codeapp_render::cache::CacheKey;
use codeapp_render::markdown::split_stable_tail;
use codeapp_render::{RenderOptions, render_diff, render_markdown, truncate_to_width, wrap_text};
use codeapp_types::transcript::{DiffStat, ToolStatus};
use codeapp_types::{ToastLevel, TranscriptKind};

use crate::state::transcript::{CellKind, Transcript};

pub fn draw(frame: &mut Frame, area: Rect, transcript: &mut Transcript, opts: &RenderOptions) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let width = area.width;
    let live_id = transcript.live_cell;

    let cell_snapshots: Vec<(u64, u32, CellKind)> = transcript
        .cells
        .iter()
        .map(|c| (c.id, c.version, c.kind.clone()))
        .collect();

    let mut cell_lines: Vec<Vec<Line<'static>>> = Vec::with_capacity(cell_snapshots.len());

    for (id, version, kind) in cell_snapshots {
        let is_live = live_id == Some(id);
        let lines = get_or_render_cell(id, version, &kind, is_live, width, opts, transcript);
        cell_lines.push(lines);
    }

    let total_lines: usize = cell_lines.iter().map(|l| l.len()).sum();
    let viewport = area.height as usize;

    let (start_line, is_scrolled_up) = if total_lines <= viewport {
        (0, false)
    } else {
        let max_scroll = total_lines.saturating_sub(viewport);
        let scroll = transcript.scroll_from_bottom.min(max_scroll);
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

fn get_or_render_cell(
    cell_id: u64,
    cell_version: u32,
    cell_kind: &CellKind,
    is_live: bool,
    width: u16,
    opts: &RenderOptions,
    transcript: &mut Transcript,
) -> Vec<Line<'static>> {
    let key = CacheKey {
        cell: cell_id ^ ((cell_version as u64) << 48),
        width,
    };

    if !is_live {
        if let Some(cached) = transcript.cache.get(key) {
            return cached.to_vec();
        }
        let rendered = render_cell(cell_kind, width, opts);
        let cached = transcript.cache.insert(key, rendered);
        return cached.to_vec();
    }

    if let CellKind::Item(TranscriptKind::Assistant { text, .. }) = cell_kind {
        let (stable, tail) = split_stable_tail(text);
        let stable_key = CacheKey {
            cell: (cell_id << 1) ^ (stable.len() as u64) ^ 0x8000_0000_0000_0000,
            width,
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
        combined.push(Line::default());
        return combined;
    }

    let rendered = render_cell(cell_kind, width, opts);
    let cached = transcript.cache.insert(key, rendered);
    cached.to_vec()
}

fn render_cell(cell_kind: &CellKind, width: u16, opts: &RenderOptions) -> Vec<Line<'static>> {
    match cell_kind {
        CellKind::Gap => vec![Line::default()],
        CellKind::Item(kind) => render_item(kind, width, opts),
    }
}

fn render_item(kind: &TranscriptKind, width: u16, opts: &RenderOptions) -> Vec<Line<'static>> {
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
            lines.push(Line::default());
            lines
        }
        TranscriptKind::Reasoning { text, complete } => {
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

            match status {
                ToolStatus::Running => {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled("…", opts.theme.accent));
                }
                ToolStatus::Ok => {
                    spans.push(Span::raw(" "));
                    if let Some(diff) = diff {
                        spans.extend(render_diff_stat(diff, opts));
                    } else if let Some(ms) = duration_ms {
                        spans.push(Span::styled(
                            format!("ok {:.1}s", *ms as f64 / 1000.0),
                            opts.theme.green,
                        ));
                    } else {
                        spans.push(Span::styled(opts.glyphs.ok, opts.theme.green));
                    }
                }
                ToolStatus::Error => {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled("failed", opts.theme.red));
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

            if !preview.is_empty() {
                if preview.starts_with("--- ") || preview.contains("\n+++ ") {
                    let diff_lines = render_diff(preview, opts);
                    for dl in diff_lines {
                        let mut spans = vec![Span::styled("  │ ", opts.theme.dim)];
                        spans.extend(dl.spans);
                        out.push(Line::from(spans));
                    }
                } else {
                    for pl in preview.lines().take(6) {
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
    }
}

fn render_diff_stat(diff: &DiffStat, opts: &RenderOptions) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("+{}", diff.added), opts.theme.green),
        Span::raw(" "),
        Span::styled(format!("-{}", diff.removed), opts.theme.red),
    ]
}
