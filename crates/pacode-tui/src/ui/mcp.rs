//! MCP servers overlay list: one entry per server — the name row, then a dim
//! sub-row with tool/resource/prompt counts or the failure reason.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::McpServerInfo;

use crate::ui::overlays::{empty_lines, menu_block, selected_row, window_groups};

/// Draw the MCP servers overlay.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    selected: usize,
    servers: &[McpServerInfo],
    loading: bool,
    opts: &RenderOptions,
) {
    let block = menu_block(
        "MCP Servers",
        " r restart · enter toggle · esc close ",
        opts,
    );

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    if servers.is_empty() {
        let lines = if loading {
            vec![Line::from(Span::styled("loading…", opts.theme.faint))]
        } else {
            empty_lines(
                "No MCP servers configured",
                "add [mcp_servers] to config, or /import to discover",
                opts,
            )
        };
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let sep = opts.glyphs.dot_sep;
    // Each server is two rows: the name, then counts or the failure reason.
    let heights: Vec<usize> = servers.iter().map(|_| 2).collect();
    let sel_idx = selected.min(servers.len() - 1);
    let budget = inner.height as usize;
    let start = window_groups(&heights, sel_idx, budget);

    let mut lines = Vec::new();
    for (i, s) in servers.iter().enumerate().skip(start) {
        if lines.len() + 2 > budget {
            break;
        }
        let is_sel = i == sel_idx;

        // Status glyph + color:
        // ready green, starting yellow, failed red, stopped dim, disabled dim
        let (glyph, status_style) = match s.status.as_str() {
            "ready" => (opts.glyphs.ok, opts.theme.green),
            "starting" => (opts.glyphs.running, opts.theme.yellow),
            "failed" => (opts.glyphs.fail, opts.theme.red),
            "disabled" => (opts.glyphs.disabled, opts.theme.dim),
            _ => (opts.glyphs.stopped, opts.theme.dim),
        };

        let name_style = if is_sel {
            opts.theme.bold
        } else if s.status == "disabled" {
            opts.theme.dim
        } else {
            opts.theme.fg
        };

        let head = Line::from(vec![
            Span::styled(
                format!("{} ", if is_sel { opts.glyphs.pointer } else { " " }),
                opts.theme.accent,
            ),
            Span::styled(format!("{glyph} "), status_style),
            Span::styled(
                truncate_to_width(&s.name, (inner.width as usize).saturating_sub(8), true),
                name_style,
            ),
        ]);

        let mut sub = vec![Span::raw("    ")];
        match s.status.as_str() {
            "ready" | "starting" => {
                sub.push(Span::styled(s.status.clone(), status_style));
                sub.push(Span::styled(
                    format!(
                        "{sep}{} tools{sep}{} res{sep}{} prompts",
                        s.tools, s.resources, s.prompts
                    ),
                    opts.theme.faint,
                ));
                if !s.prompt_names.is_empty() {
                    let names = s.prompt_names.join(", ");
                    let avail = (inner.width as usize).saturating_sub(16);
                    sub.push(Span::styled(
                        format!(
                            "{sep}{}",
                            truncate_to_width(&names, avail, opts.glyphs.ascii)
                        ),
                        opts.theme.faint,
                    ));
                }
            }
            "disabled" => {
                sub.push(Span::styled("disabled", opts.theme.dim));
                sub.push(Span::styled(
                    format!("{sep}enter re-enables"),
                    opts.theme.faint,
                ));
            }
            _ => {
                sub.push(Span::styled(s.status.clone(), status_style));
                if let Some(ref err) = s.error {
                    let avail = (inner.width as usize).saturating_sub(8);
                    sub.push(Span::styled(
                        format!("{sep}{}", truncate_to_width(err, avail, opts.glyphs.ascii)),
                        opts.theme.red,
                    ));
                }
            }
        }
        let sub = Line::from(sub);

        if is_sel {
            lines.push(selected_row(head, inner.width, opts));
            lines.push(selected_row(sub, inner.width, opts));
        } else {
            lines.push(head);
            lines.push(sub);
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}
