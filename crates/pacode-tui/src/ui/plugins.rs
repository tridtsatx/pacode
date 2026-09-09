//! Plugins overlay list.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::PluginInfo;

/// Draw the plugins overlay.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    selected: usize,
    plugins: &[PluginInfo],
    opts: &RenderOptions,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Plugins ", opts.theme.bold))
        .title_bottom(Span::styled(" esc close ", opts.theme.dim))
        .border_style(opts.theme.dim);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if plugins.is_empty() {
        let line = Line::from(Span::styled("No plugins loaded", opts.theme.faint));
        frame.render_widget(Paragraph::new(vec![line]), inner);
        return;
    }

    let max_rows = inner.height as usize;
    let sel_idx = selected.min(plugins.len().saturating_sub(1));
    let start = if sel_idx >= max_rows {
        sel_idx + 1 - max_rows
    } else {
        0
    };

    let dot_sep = if opts.glyphs.ascii { " . " } else { " · " };
    let mut lines = Vec::new();

    for (i, p) in plugins.iter().enumerate().skip(start).take(max_rows) {
        let is_sel = i == sel_idx;
        let pointer = if is_sel { opts.glyphs.pointer } else { " " };
        let pointer_style = if is_sel {
            opts.theme.accent
        } else {
            opts.theme.fg
        };

        let name_style = if is_sel {
            opts.theme.selected_bg.patch(opts.theme.bold)
        } else {
            opts.theme.bold
        };

        let counts_str = format!("{} tools{dot_sep}{} cmds", p.tools.len(), p.commands.len());

        let mut row_spans = vec![
            Span::styled(pointer, pointer_style),
            Span::raw(" "),
            Span::styled(p.name.clone(), name_style),
            Span::raw(" "),
            Span::styled(format!("v{}", p.version), opts.theme.dim),
            Span::raw(" "),
            Span::styled(format!("[{}]", p.kind), opts.theme.cyan),
            Span::raw("  "),
            Span::styled(counts_str, opts.theme.dim),
        ];

        if let Some(ref err) = p.error {
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
