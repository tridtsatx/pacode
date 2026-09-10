//! Touched files overlay list with inline image previews (spec §11).

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use pacode_render::RenderOptions;
use pacode_types::time::{format_duration_ms, now_ms};

use crate::state::AppState;
use crate::state::files::{CachedImagePreview, FileRow, ImageCacheKey};

#[cfg(test)]
#[path = "files_tests.rs"]
mod files_tests;

/// Check if a path has a supported image extension (case-insensitive).
pub fn is_image_file(path_str: &str) -> bool {
    let p = Path::new(path_str);
    let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
    )
}

/// Draw the touched files overlay.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    selected: usize,
    state: &mut AppState,
    opts: &RenderOptions,
) {
    let block =
        crate::ui::overlays::menu_block("Touched Files", " enter copy path · esc close ", opts);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.files.is_empty() {
        state.files.clear_preview();
        let lines = crate::ui::overlays::empty_lines(
            "No files touched yet",
            "files the agent reads or writes land here",
            opts,
        );
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    // `sorted` borrows `state.files`, so decide the layout and draw the list
    // first, then release the borrow before the preview needs `&mut state`.
    let preview_path = {
        let sorted = state.files.sorted_rows();
        let sel_idx = if sorted.is_empty() {
            0
        } else {
            selected.min(sorted.len() - 1)
        };

        let is_img = state.config.ui.images_enabled()
            && !sorted.is_empty()
            && is_image_file(&sorted[sel_idx].path);

        if is_img && inner.width >= 40 {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(inner);
            render_file_list(frame, chunks[0], &sorted, sel_idx, opts);
            Some((chunks[1], sorted[sel_idx].path.clone()))
        } else {
            render_file_list(frame, inner, &sorted, sel_idx, opts);
            None
        }
    };

    match preview_path {
        Some((preview_area, path)) => {
            render_image_preview(frame, preview_area, &path, state, opts);
        }
        None => state.files.clear_preview(),
    }
}

fn render_file_list(
    frame: &mut Frame,
    area: Rect,
    sorted: &[&FileRow],
    sel_idx: usize,
    opts: &RenderOptions,
) {
    let max_rows = area.height as usize;
    let now = now_ms();

    let start = if sel_idx >= max_rows {
        sel_idx + 1 - max_rows
    } else {
        0
    };

    let mut lines = Vec::new();
    let avail_path_width = (area.width as usize).saturating_sub(24).max(10);

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
            opts.theme.bold
        } else {
            opts.theme.fg
        };

        let count_str = format!("{:>3}x", row.count);
        let age_ms = now.saturating_sub(row.last_ts_ms);
        let age_str = format!("{:>7}", format_duration_ms(age_ms));

        let line = Line::from(vec![
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
        ]);
        if is_sel {
            lines.push(crate::ui::overlays::selected_row(line, area.width, opts));
        } else {
            lines.push(line);
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_image_preview(
    frame: &mut Frame,
    area: Rect,
    path_str: &str,
    state: &mut AppState,
    opts: &RenderOptions,
) {
    let preview_block = Block::default()
        .borders(Borders::LEFT)
        .border_style(opts.theme.dim);
    let p_inner = preview_block.inner(area);
    frame.render_widget(preview_block, area);

    if p_inner.width == 0 || p_inner.height == 0 {
        return;
    }

    let p = Path::new(path_str);
    // Files are listed relative to the session cwd reported by the daemon.
    let cwd = state
        .meta
        .as_ref()
        .map(|m| m.cwd.clone())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let resolved_path = if p.is_absolute() {
        p.to_path_buf()
    } else if cwd.join(p).exists() {
        cwd.join(p)
    } else {
        p.to_path_buf()
    };

    let key = ImageCacheKey {
        path: resolved_path.clone(),
        cell_w: p_inner.width,
        cell_h: p_inner.height,
    };

    let cached_hit = state.files.cached_preview.as_ref().filter(|c| c.key == key);

    let preview_result = if let Some(cached) = cached_hit {
        cached.preview.clone()
    } else {
        let proto = pacode_image::detect();
        let req = pacode_image::PreviewRequest {
            path: resolved_path.clone(),
            cell_w: p_inner.width,
            cell_h: p_inner.height,
        };
        let val = pacode_image::render(&req, proto).ok();
        state.files.cached_preview = Some(CachedImagePreview {
            key,
            preview: val.clone(),
        });
        val
    };

    match preview_result {
        Some(pacode_image::Preview::Cells(grid)) => {
            state.files.pending_escape = None;
            let mut lines = Vec::new();
            for row in grid.iter().take(p_inner.height as usize) {
                let mut spans = Vec::new();
                for cell in row.iter().take(p_inner.width as usize) {
                    let style = ratatui::style::Style::default()
                        .fg(ratatui::style::Color::Rgb(
                            cell.top.0, cell.top.1, cell.top.2,
                        ))
                        .bg(ratatui::style::Color::Rgb(
                            cell.bottom.0,
                            cell.bottom.1,
                            cell.bottom.2,
                        ));
                    spans.push(Span::styled("▀", style));
                }
                lines.push(Line::from(spans));
            }
            frame.render_widget(Paragraph::new(lines), p_inner);
        }
        Some(pacode_image::Preview::Escape(esc)) => {
            frame.render_widget(ratatui::widgets::Clear, p_inner);
            state.files.pending_escape = Some((p_inner, esc, resolved_path));
        }
        None => {
            state.files.pending_escape = None;
            let filename = p.file_name().and_then(|f| f.to_str()).unwrap_or(path_str);
            let placeholder = vec![
                Line::from(Span::styled("Image preview unavailable", opts.theme.dim)),
                Line::from(Span::styled(filename.to_string(), opts.theme.faint)),
            ];
            frame.render_widget(Paragraph::new(placeholder), p_inner);
        }
    }
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
