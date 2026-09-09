//! MCP servers overlay list.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::McpServerInfo;

/// Draw the MCP servers overlay.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    selected: usize,
    servers: &[McpServerInfo],
    loading: bool,
    opts: &RenderOptions,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" MCP Servers ", opts.theme.bold))
        .title_bottom(Span::styled(
            " r restart · enter toggle · esc close ",
            opts.theme.dim,
        ))
        .border_style(opts.theme.dim);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if servers.is_empty() {
        let msg = if loading {
            "Loading MCP servers…"
        } else {
            "No MCP servers configured"
        };
        let line = Line::from(Span::styled(msg, opts.theme.faint));
        frame.render_widget(Paragraph::new(vec![line]), inner);
        return;
    }

    let max_rows = inner.height as usize;
    let sel_idx = selected.min(servers.len().saturating_sub(1));
    let start = if sel_idx >= max_rows {
        sel_idx + 1 - max_rows
    } else {
        0
    };

    let dot_sep = if opts.glyphs.ascii { " . " } else { " · " };
    let mut lines = Vec::new();

    for (i, s) in servers.iter().enumerate().skip(start).take(max_rows) {
        let is_sel = i == sel_idx;
        let pointer = if is_sel { opts.glyphs.pointer } else { " " };
        let pointer_style = if is_sel {
            opts.theme.accent
        } else {
            opts.theme.fg
        };

        // Status glyph + color:
        // ready green, starting yellow, failed red, stopped dim, disabled dim strikethrough-free
        let (glyph, status_style) = match s.status.as_str() {
            "ready" => (opts.glyphs.ok, opts.theme.green),
            "starting" => (opts.glyphs.running, opts.theme.yellow),
            "failed" => (opts.glyphs.fail, opts.theme.red),
            "disabled" => (if opts.glyphs.ascii { "x" } else { "⊘" }, opts.theme.dim),
            _ => (if opts.glyphs.ascii { "-" } else { "■" }, opts.theme.dim),
        };

        let name_style = if is_sel {
            opts.theme.selected_bg.patch(opts.theme.bold)
        } else if s.status == "disabled" {
            opts.theme.dim
        } else {
            opts.theme.fg
        };

        let counts_str = format!(
            "{} tools{dot_sep}{} res{dot_sep}{} prompts",
            s.tools, s.resources, s.prompts
        );

        let mut row_spans = vec![
            Span::styled(pointer, pointer_style),
            Span::raw(" "),
            Span::styled(glyph, status_style),
            Span::raw(" "),
            Span::styled(s.name.clone(), name_style),
            Span::raw("  "),
            Span::styled(counts_str, opts.theme.dim),
        ];

        if let Some(ref err) = s.error {
            let used_w: usize = row_spans
                .iter()
                .map(|sp| pacode_render::display_width(&sp.content))
                .sum();
            let avail_w = (inner.width as usize).saturating_sub(used_w + 3);
            if avail_w > 4 {
                let err_trunc = truncate_to_width(err, avail_w, opts.glyphs.ascii);
                row_spans.push(Span::raw(" - "));
                row_spans.push(Span::styled(err_trunc, opts.theme.red));
            }
        }

        lines.push(Line::from(row_spans));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}
