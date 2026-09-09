//! Markdown → styled, wrapped ratatui lines. Port the structure of jcode's
//! `jcode-render-core` (pulldown-cmark event walk into a block model, then layout),
//! without syntax highlighting or images.
//!
//! Supported: paragraphs, headings (bold, accent), emphasis/strong, inline code
//! (accent), fenced code blocks (dim `│` gutter, no highlighting), bullet and numbered
//! lists (nested, `•`/`1.`), block quotes (dim bar), horizontal rule, links (text +
//! dim URL), tables (simple column alignment, rendered only when complete), soft/hard
//! breaks. Unknown constructs degrade to plain text; never drop content.

#[path = "markdown_parse.rs"]
mod markdown_parse;

use markdown_parse::{Block, parse_to_blocks};
use ratatui::text::{Line, Span};

use crate::RenderOptions;
use crate::wrap::{display_width, truncate_to_width, wrap_line};

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod markdown_tests;

/// Render `source` into wrapped lines for `opts.width`.
pub fn render_markdown(source: &str, opts: &RenderOptions) -> Vec<Line<'static>> {
    let blocks = parse_to_blocks(source, opts);
    let mut output: Vec<Line<'static>> = Vec::new();

    let mut prev_was_list_item = false;
    for block in blocks {
        let is_list_item = matches!(block, Block::ListItem { .. });
        let block_lines = render_block(block, opts);
        if block_lines.is_empty() {
            continue;
        }
        if !output.is_empty() && !(prev_was_list_item && is_list_item) {
            output.push(Line::default());
        }
        output.extend(block_lines);
        prev_was_list_item = is_list_item;
    }

    while output
        .first()
        .is_some_and(|l| l.spans.is_empty() || l.to_string().trim().is_empty())
    {
        output.remove(0);
    }
    while output
        .last()
        .is_some_and(|l| l.spans.is_empty() || l.to_string().trim().is_empty())
    {
        output.pop();
    }

    output
}

fn render_block(block: Block, opts: &RenderOptions) -> Vec<Line<'static>> {
    let width = opts.width as usize;
    match block {
        Block::Paragraph(line) => wrap_line(line, width, 0),
        Block::Heading { line } => wrap_line(line, width, 0),
        Block::Code { lines } => {
            let vline = opts.glyphs.vline;
            let gutter = format!("{vline} ");
            let gutter_width = display_width(&gutter);
            let available_width = width.saturating_sub(gutter_width);
            lines
                .into_iter()
                .map(|code_line| {
                    let truncated = truncate_to_width(&code_line, available_width, true);
                    Line::from(vec![
                        Span::styled(gutter.clone(), opts.theme.dim),
                        Span::styled(truncated, opts.theme.fg),
                    ])
                })
                .collect()
        }
        Block::ListItem {
            ordered,
            number,
            depth,
            line,
        } => {
            let indent_str = "  ".repeat(depth);
            let bullet = if opts.glyphs.ascii { "- " } else { "• " };
            let marker = if ordered {
                format!("{indent_str}{number}. ")
            } else {
                format!("{indent_str}{bullet}")
            };
            let marker_width = display_width(&marker);
            let mut line_spans = vec![Span::styled(marker, opts.theme.dim)];
            line_spans.extend(line.spans);
            wrap_line(Line::from(line_spans), width, marker_width)
        }
        Block::Quote(lines) => {
            let prefix = if opts.glyphs.ascii { "> " } else { "▎ " };
            let prefix_width = display_width(prefix);
            let available_width = width.saturating_sub(prefix_width);
            let mut result = Vec::new();
            for l in lines {
                let wrapped = wrap_line(l, available_width, 0);
                for mut wl in wrapped {
                    wl.spans
                        .insert(0, Span::styled(prefix.to_string(), opts.theme.dim));
                    result.push(wl);
                }
            }
            result
        }
        Block::ThematicBreak => {
            vec![Line::from(Span::styled(
                opts.glyphs.hline.repeat(width),
                opts.theme.dim,
            ))]
        }
        Block::Table { rows } => render_table(&rows, opts),
    }
}

fn render_table(rows: &[Vec<String>], opts: &RenderOptions) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }
    let num_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if num_cols == 0 {
        return Vec::new();
    }

    let mut col_widths: Vec<usize> = vec![1; num_cols];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < num_cols {
                col_widths[i] = col_widths[i].max(display_width(cell));
            }
        }
    }

    let sep_len = if num_cols > 1 { (num_cols - 1) * 3 } else { 0 };
    let available_cells_width = (opts.width as usize).saturating_sub(sep_len);

    while col_widths.iter().sum::<usize>() > available_cells_width {
        if let Some((max_idx, _)) = col_widths
            .iter()
            .enumerate()
            .filter(|(_, w)| **w > 1)
            .max_by_key(|(_, w)| **w)
        {
            col_widths[max_idx] -= 1;
        } else {
            break;
        }
    }

    let mut lines = Vec::new();
    let cross = if opts.glyphs.ascii { "+" } else { "┼" };
    let hline = opts.glyphs.hline;
    let vline = opts.glyphs.vline;
    let sep_row = format!("{hline}{cross}{hline}");
    let sep_cell = format!(" {vline} ");

    for (row_idx, row) in rows.iter().enumerate() {
        let mut spans = Vec::new();
        for (col_idx, &col_w) in col_widths.iter().enumerate() {
            if col_idx > 0 {
                spans.push(Span::styled(sep_cell.clone(), opts.theme.dim));
            }
            let cell = row.get(col_idx).map(String::as_str).unwrap_or("");
            let dw = display_width(cell);
            let padded = if dw > col_w {
                truncate_to_width(cell, col_w, true)
            } else {
                let pad = " ".repeat(col_w - dw);
                format!("{cell}{pad}")
            };
            let style = if row_idx == 0 {
                opts.theme.bold
            } else {
                opts.theme.fg
            };
            spans.push(Span::styled(padded, style));
        }
        lines.push(Line::from(spans));

        if row_idx == 0 {
            let separator: String = col_widths
                .iter()
                .map(|&w| opts.glyphs.hline.repeat(w))
                .collect::<Vec<_>>()
                .join(&sep_row);
            lines.push(Line::from(Span::styled(separator, opts.theme.dim)));
        }
    }

    lines
}

/// Split `source` into a stable prefix (complete blocks) and a mutable tail (the last
/// block, which may still be streaming). Used by the streaming cell so that only the
/// tail is re-rendered on each delta. A fenced code block or table that is not closed
/// makes the whole block part of the tail.
pub fn split_stable_tail(source: &str) -> (&str, &str) {
    let mut search_end = source.len();

    while let Some(pos) = source[..search_end].rfind("\n\n") {
        let split_pos = pos + 2;

        let mut fence_count = 0usize;
        for line in source[..split_pos].lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                fence_count += 1;
            }
        }

        if fence_count % 2 == 1 {
            search_end = pos;
            continue;
        }

        let line_before = source[..pos]
            .lines()
            .next_back()
            .map(str::trim)
            .unwrap_or("");
        let line_after = source[split_pos..]
            .lines()
            .next()
            .map(str::trim)
            .unwrap_or("");
        if line_before.starts_with('|') && line_after.starts_with('|') {
            search_end = pos;
            continue;
        }

        return (&source[..split_pos], &source[split_pos..]);
    }

    ("", source)
}
