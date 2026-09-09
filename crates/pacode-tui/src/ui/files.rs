//! Touched files overlay list (spec §11).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use pacode_render::RenderOptions;
use pacode_types::time::{format_duration_ms, now_ms};

use crate::state::AppState;

/// Draw the touched files overlay.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    selected: usize,
    state: &AppState,
    opts: &RenderOptions,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Touched Files ", opts.theme.bold))
        .title_bottom(Span::styled(
            " enter copy path · esc close ",
            opts.theme.dim,
        ))
        .border_style(opts.theme.dim);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.files.is_empty() {
        let msg = Line::from(Span::styled("No files touched yet", opts.theme.faint));
        frame.render_widget(Paragraph::new(vec![msg]), inner);
        return;
    }

    let sorted = state.files.sorted_rows();
    let max_rows = inner.height as usize;
    let now = now_ms();

    let sel_idx = if sorted.is_empty() {
        0
    } else {
        selected.min(sorted.len() - 1)
    };

    let start = if sel_idx >= max_rows {
        sel_idx + 1 - max_rows
    } else {
        0
    };

    let mut lines = Vec::new();
    let avail_path_width = (inner.width as usize).saturating_sub(24).max(10);

    for (i, row) in sorted.iter().enumerate().skip(start).take(max_rows) {
        let is_sel = i == sel_idx;
        let pointer = if is_sel { opts.glyphs.pointer } else { " " };
        let pointer_style = if is_sel {
            opts.theme.accent
        } else {
            opts.theme.fg
        };

        let dot_char = if opts.glyphs.ascii { "." } else { "·" };
        let r_badge = if row.kinds.has_read() {
            Span::styled("R", opts.theme.green)
        } else {
            Span::styled(dot_char, opts.theme.faint)
        };
        let w_badge = if row.kinds.has_write() {
            Span::styled("W", opts.theme.red)
        } else {
            Span::styled(dot_char, opts.theme.faint)
        };
        let e_badge = if row.kinds.has_edit() {
            Span::styled("E", opts.theme.accent)
        } else {
            Span::styled(dot_char, opts.theme.faint)
        };
        let s_badge = if row.kinds.has_search() {
            Span::styled("S", opts.theme.cyan)
        } else {
            Span::styled(dot_char, opts.theme.faint)
        };

        let path_text = truncate_left(&row.path, avail_path_width, opts.glyphs.ascii);
        let path_style = if is_sel {
            opts.theme.selected_bg.patch(opts.theme.bold)
        } else {
            opts.theme.fg
        };

        let count_str = format!("{:>3}x", row.count);
        let age_ms = now.saturating_sub(row.last_ts_ms);
        let age_str = format!("{:>7}", format_duration_ms(age_ms));

        lines.push(Line::from(vec![
            Span::styled(pointer, pointer_style),
            Span::raw(" "),
            r_badge,
            w_badge,
            e_badge,
            s_badge,
            Span::raw(" "),
            Span::styled(path_text, path_style),
            Span::raw(" "),
            Span::styled(count_str, opts.theme.dim),
            Span::raw(" "),
            Span::styled(age_str, opts.theme.faint),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Truncate string on the left, prepending an ellipsis if it exceeds `max_width`.
pub fn truncate_left(s: &str, max_width: usize, ascii: bool) -> String {
    let w = pacode_render::display_width(s);
    if w <= max_width {
        return s.to_string();
    }

    let ellipsis = if ascii { "..." } else { "…" };
    let ell_w = if ascii { 3 } else { 1 };
    if max_width <= ell_w {
        return ellipsis.chars().take(max_width).collect();
    }

    let target_w = max_width - ell_w;
    let mut width_acc = 0;
    let mut chars = Vec::new();
    for c in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1);
        if width_acc + cw > target_w {
            break;
        }
        width_acc += cw;
        chars.push(c);
    }
    chars.reverse();
    let tail: String = chars.into_iter().collect();
    format!("{ellipsis}{tail}")
}
