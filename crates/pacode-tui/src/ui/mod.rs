//! Widgets. `draw` renders one frame from `AppState` using `ScreenLayout`.
//!
//! Each widget is a function `fn draw_x(frame: &mut Frame, area: Rect, state: &mut AppState, opts: &RenderOptions)`.
//! Widgets may mutate render caches inside the state but nothing else.

pub mod anim;
pub mod config_view;
pub mod dialog;
pub mod files;
pub mod footer;
pub mod header;
pub mod import;
pub mod input;
pub mod keys_overlay;
pub mod mascot;
pub mod mcp;
pub mod overlays;
pub mod panel;
pub mod phrases;
pub mod picker;
pub mod plugins;
pub mod popup;
pub mod rail;
pub mod rail_session;
pub mod theme_picker;
pub mod toast;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::RenderOptions;

use crate::layout::ScreenLayout;
use crate::state::{AppState, Focus};

/// Draw the whole screen; returns the layout used (for mouse hit-testing).
pub fn draw(frame: &mut Frame, state: &mut AppState) -> ScreenLayout {
    let panel_open = state.panel.target.is_some() || matches!(state.focus, Focus::Panel { .. });
    let temp_layout = crate::layout::compute(frame.area(), 1, panel_open);
    let input_lines = state.input.wrapped_lines(temp_layout.input.width);
    let layout = crate::layout::compute(frame.area(), input_lines, panel_open);

    let opts = RenderOptions::with_theme(
        frame.area().width,
        state.config.ui.ascii_only,
        state.theme.clone(),
    )
    .thinking(state.config.ui.thinking);

    if layout.rail.width > 0 {
        if layout.rail_separator.width > 0 {
            let vline = opts.glyphs.vline;
            let sep_line = Line::from(Span::styled(vline, opts.theme.dim));
            for y in layout.rail_separator.y..layout.rail_separator.bottom() {
                frame.render_widget(
                    Paragraph::new(sep_line.clone()),
                    Rect::new(layout.rail_separator.x, y, 1, 1),
                );
            }
        }
        rail::draw(frame, layout.rail, state, &opts);
    }

    let is_picker = state.is_bottom_picker();
    let picker_h = if is_picker {
        state.bottom_picker_height().min(frame.area().height)
    } else {
        0
    };
    let dialog_area = if is_picker {
        let picker_y = frame.area().height.saturating_sub(picker_h);
        let d_h = picker_y.saturating_sub(layout.dialog.y);
        Rect::new(layout.dialog.x, layout.dialog.y, layout.dialog.width, d_h)
    } else {
        layout.dialog
    };

    if dialog_area.width > 0 && dialog_area.height > 0 {
        let dialog_opts = RenderOptions::with_theme(
            dialog_area.width,
            state.config.ui.ascii_only,
            state.theme.clone(),
        )
        .thinking(state.config.ui.thinking);
        // The activity line is drawn whenever there is something to report, which
        // includes waiting on a subagent or a background task with the turn over.
        let phase = state.phase.as_ref().map(|(p, _)| p.clone());
        if let Some(phase) = phase.filter(|_| dialog_area.height > 1) {
            let trans_h = dialog_area.height - 1;
            let trans_area = Rect::new(dialog_area.x, dialog_area.y, dialog_area.width, trans_h);
            let anim_area = Rect::new(
                dialog_area.x,
                dialog_area.bottom() - 1,
                dialog_area.width,
                1,
            );
            dialog::draw(
                frame,
                trans_area,
                &mut state.transcript,
                &dialog_opts,
                state.anim_frame,
            );

            let anim_line = anim::render_activity_line(
                state.anim_frame,
                &dialog_opts,
                &phase,
                state.phase_elapsed_ms,
                dialog_area.width as usize,
            );
            frame.render_widget(Paragraph::new(anim_line), anim_area);
        } else {
            dialog::draw(
                frame,
                dialog_area,
                &mut state.transcript,
                &dialog_opts,
                state.anim_frame,
            );
        }

        apply_selection_highlight(frame, &state.selection, dialog_area, opts.theme.selected_bg);
    }

    if let Some(panel_rect) = layout.panel {
        if let Some(sep_rect) = layout.panel_separator {
            let vline = opts.glyphs.vline;
            let sep_line = Line::from(Span::styled(vline, opts.theme.dim));
            for y in sep_rect.y..sep_rect.bottom() {
                frame.render_widget(
                    Paragraph::new(sep_line.clone()),
                    Rect::new(sep_rect.x, y, 1, 1),
                );
            }
        }
        let panel_opts = RenderOptions::with_theme(
            panel_rect.width,
            state.config.ui.ascii_only,
            state.theme.clone(),
        )
        .thinking(state.config.ui.thinking);
        panel::draw(frame, panel_rect, state, &panel_opts);
    }

    if is_picker {
        let picker_y = frame.area().height.saturating_sub(picker_h);
        let picker_w = if layout.dialog.width > 0 {
            layout.dialog.width
        } else {
            frame.area().width
        };
        let picker_area = Rect::new(layout.dialog.x, picker_y, picker_w, picker_h);
        picker::draw(frame, picker_area, state, &opts);
    } else {
        input::draw(frame, &layout, state, &opts);
        footer::draw(frame, layout.footer, state, &opts);
    }

    if layout.toast.width > 0 && layout.toast.height > 0 {
        let toast_area = if is_picker {
            let picker_y = frame.area().height.saturating_sub(picker_h);
            let toast_y = picker_y.saturating_sub(layout.toast.height);
            Rect::new(
                layout.toast.x,
                toast_y,
                layout.toast.width,
                layout.toast.height,
            )
        } else {
            layout.toast
        };
        toast::draw(frame, toast_area, state, &opts);
    }

    let dialog_or_full = if layout.dialog.width > 0 {
        layout.dialog
    } else {
        frame.area()
    };
    overlays::draw(frame, dialog_or_full, state, &opts);

    let copy_req = state.selection.copy_request;
    let should_copy = match copy_req {
        crate::state::selection::CopyRequest::Explicit => true,
        crate::state::selection::CopyRequest::Auto => state.config.ui.auto_copy,
        crate::state::selection::CopyRequest::None => false,
    };
    state.selection.copy_request = crate::state::selection::CopyRequest::None;

    if should_copy && state.selection.is_active() && !state.selection.is_empty() {
        let text = extract_selection_text(frame.buffer_mut(), &state.selection, dialog_area);
        if !text.is_empty() {
            let count = text.chars().count();
            if let Err(e) = crate::clipboard::copy(&text) {
                log::warn!("clipboard copy failed ({count} chars): {e}");
            }
            let detail = match copy_req {
                crate::state::selection::CopyRequest::Auto => {
                    Some("disable autocopy in /config".to_string())
                }
                crate::state::selection::CopyRequest::Explicit
                | crate::state::selection::CopyRequest::None => None,
            };
            state.push_toast(
                pacode_types::ToastLevel::Info,
                format!("copied {count} chars"),
                detail,
                std::time::Instant::now(),
            );
            if crate::clipboard::remote_hint().is_some() && !state.clipboard_warned {
                state.clipboard_warned = true;
                let now = std::time::Instant::now();
                let shown_at = now
                    .checked_sub(std::time::Duration::from_secs(
                        crate::state::TOAST_TTL_SECS.saturating_sub(4),
                    ))
                    .unwrap_or(now);
                state.toasts.push_back(crate::state::Toast {
                    level: pacode_types::ToastLevel::Warn,
                    title: popup::POPUP_TOAST_TITLE.to_string(),
                    detail: Some(crate::clipboard::REMOTE_HINT_TEXT.to_string()),
                    shown_at,
                });
            }
        }
    }

    if let Some(popup_toast) = state
        .toasts
        .iter()
        .find(|t| t.title == popup::POPUP_TOAST_TITLE)
    {
        let text = popup_toast
            .detail
            .as_deref()
            .unwrap_or(crate::clipboard::REMOTE_HINT_TEXT);
        popup::draw(frame, frame.area(), text, &opts);
    }

    layout
}

pub(crate) fn apply_selection_highlight(
    frame: &mut Frame,
    selection: &crate::state::Selection,
    dialog_area: Rect,
    selected_bg: ratatui::style::Style,
) {
    if !selection.is_active() || selection.is_empty() {
        return;
    }
    let Some(row_range) = selection.row_range(dialog_area) else {
        return;
    };
    for row in row_range {
        if let Some(col_range) = selection.col_range_for_row(row, dialog_area) {
            for col in col_range {
                if let Some(cell) = frame.buffer_mut().cell_mut((col, row)) {
                    cell.set_style(cell.style().patch(selected_bg));
                }
            }
        }
    }
}

pub(crate) fn extract_selection_text(
    buffer: &ratatui::buffer::Buffer,
    selection: &crate::state::Selection,
    dialog_area: Rect,
) -> String {
    let Some(row_range) = selection.row_range(dialog_area) else {
        return String::new();
    };
    let mut rows = Vec::new();
    for row in row_range {
        if let Some(col_range) = selection.col_range_for_row(row, dialog_area) {
            let mut row_str = String::new();
            for col in col_range {
                if let Some(cell) = buffer.cell((col, row)) {
                    row_str.push_str(cell.symbol());
                }
            }
            rows.push(row_str.trim_end().to_string());
        }
    }
    rows.join("\n")
}
