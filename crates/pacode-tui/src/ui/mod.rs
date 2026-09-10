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
use crate::state::{AppState, Focus, Overlay};

/// A small key identifying the focus layer, so a draw can tell "a different
/// popup opened" without matching on payloads. Changing the index inside a
/// picker keeps the same key, so navigating does not replay the unfold. `pub`
/// for tests that set `state.focus` directly and mark it already seen.
pub fn focus_key(focus: &Focus) -> u8 {
    match focus {
        Focus::Normal => 0,
        Focus::SelectAgent { .. } => 1,
        Focus::Panel { .. } => 2,
        Focus::BgList { .. } => 3,
        Focus::Overlay(overlay) => match overlay {
            Overlay::ModelPicker { .. } => 10,
            Overlay::EffortPicker { .. } => 11,
            Overlay::ModePicker { .. } => 12,
            Overlay::ConfigPicker => 13,
            Overlay::QuestionPicker { .. } => 14,
            Overlay::ThemePicker { .. } => 15,
            Overlay::SessionPicker { .. } => 16,
            Overlay::Files { .. } => 17,
            Overlay::McpPicker { .. } => 18,
            Overlay::PluginsPicker { .. } => 19,
            Overlay::KeysPicker { .. } => 20,
            Overlay::Import(_) => 21,
            Overlay::RailOverlay => 22,
            Overlay::Help => 23,
        },
    }
}

/// Ease-out cubic height for the unfold: 1 row at elapsed 0, the full `height`
/// at `OVERLAY_GROW_MS`. Pure, so tests can pin the curve.
fn eased_height(height: u16, elapsed_ms: u64) -> u16 {
    if height == 0 || elapsed_ms >= crate::state::OVERLAY_GROW_MS {
        return height;
    }
    let t = elapsed_ms as f32 / crate::state::OVERLAY_GROW_MS as f32;
    let eased = 1.0 - (1.0 - t).powi(3);
    ((height as f32) * eased).round().max(1.0) as u16
}

/// `area` shrunk vertically for the first `OVERLAY_GROW_MS` of an overlay's
/// life, centred on `area` — an unfold. Purely subtractive: the overlay draws
/// into the smaller rect and is clipped, so no part of it appears early.
fn grow_rect(area: Rect, elapsed_ms: u64) -> Rect {
    let h = eased_height(area.height, elapsed_ms);
    Rect::new(area.x, area.y + (area.height - h) / 2, area.width, h)
}

/// The same unfold, anchored to the bottom edge: bottom pickers grow upward
/// from where the input line was.
fn grow_rect_bottom(area: Rect, elapsed_ms: u64) -> Rect {
    let h = eased_height(area.height, elapsed_ms);
    Rect::new(area.x, area.bottom() - h, area.width, h)
}

/// Draw the whole screen; returns the layout used (for mouse hit-testing).
pub fn draw(frame: &mut Frame, state: &mut AppState) -> ScreenLayout {
    let now = std::time::Instant::now();
    // Age of the session UI: drives the welcome cascade in the header banner.
    let welcome_ms = now.saturating_duration_since(state.started_at).as_millis() as u64;
    // Render-side open tracking: a new overlay/picker layer gets `overlay_since`
    // and unfolds for ~140 ms; anything else clears it.
    let fkey = focus_key(&state.focus);
    if fkey != state.last_focus_key {
        state.last_focus_key = fkey;
        state.overlay_since = (fkey >= 3).then_some(now);
    }
    let grow_ms = state
        .overlay_since
        .map(|t| now.saturating_duration_since(t).as_millis() as u64);

    // In replace mode a selected subagent takes over the conversation column, so
    // no second column is laid out at all.
    let panel_open = (state.panel.target.is_some() || matches!(state.focus, Focus::Panel { .. }))
        && !state.agent_replaces_dialog();
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
        // Whose conversation is on screen must be visible: the column looks the
        // same whether it holds the main thread or a subagent.
        let takeover = state.agent_replaces_dialog();
        let dialog_area = if takeover && dialog_area.height > 1 {
            let name = match state.panel_agent_target() {
                Some(crate::state::PanelTarget::Agent(id)) => state
                    .rail
                    .agent(&id)
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| id.to_string()),
                Some(crate::state::PanelTarget::Task(_)) | None => String::new(),
            };
            let banner = Line::from(vec![
                Span::styled(
                    format!("{} agent ", opts.glyphs.agent_dot),
                    opts.theme.accent,
                ),
                Span::styled(name, opts.theme.bold),
                Span::styled("  ·  esc for the main chat", opts.theme.faint),
            ]);
            frame.render_widget(
                Paragraph::new(banner),
                Rect::new(dialog_area.x, dialog_area.y, dialog_area.width, 1),
            );
            Rect::new(
                dialog_area.x,
                dialog_area.y + 1,
                dialog_area.width,
                dialog_area.height - 1,
            )
        } else {
            dialog_area
        };

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
            let transcript = if state.agent_replaces_dialog() {
                &mut state.panel.agent_transcript
            } else {
                &mut state.transcript
            };
            dialog::draw(
                frame,
                trans_area,
                transcript,
                &dialog_opts,
                state.anim_frame,
                welcome_ms,
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
            let transcript = if state.agent_replaces_dialog() {
                &mut state.panel.agent_transcript
            } else {
                &mut state.transcript
            };
            dialog::draw(
                frame,
                dialog_area,
                transcript,
                &dialog_opts,
                state.anim_frame,
                welcome_ms,
            );
        }

        let first_visible = if state.agent_replaces_dialog() {
            state.panel.agent_transcript.first_visible_line
        } else {
            state.transcript.first_visible_line
        };
        apply_selection_highlight(
            frame,
            &state.selection,
            dialog_area,
            first_visible,
            opts.theme.selected_bg,
        );
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
        // Bottom pickers unfold upward from the bottom edge for ~140 ms.
        let picker_area = match grow_ms {
            Some(ms) => grow_rect_bottom(picker_area, ms),
            None => picker_area,
        };
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
    // Centred overlays unfold for ~140 ms: drawing into the shrinking rect is
    // all the animation needs — no timer, no interim layout.
    let overlay_area = match grow_ms.filter(|_| !is_picker) {
        Some(ms) => grow_rect(dialog_or_full, ms),
        None => dialog_or_full,
    };
    overlays::draw(frame, overlay_area, state, &opts);

    let copy_req = state.selection.copy_request;
    let should_copy = match copy_req {
        crate::state::selection::CopyRequest::Explicit => true,
        crate::state::selection::CopyRequest::Auto => state.config.ui.auto_copy,
        crate::state::selection::CopyRequest::None => false,
    };
    state.selection.copy_request = crate::state::selection::CopyRequest::None;

    if should_copy && state.selection.is_active() && !state.selection.is_empty() {
        let dialog_opts = RenderOptions::with_theme(
            dialog_area.width,
            state.config.ui.ascii_only,
            state.theme.clone(),
        )
        .thinking(state.config.ui.thinking);
        let anim_frame = state.anim_frame;
        let lines = if state.agent_replaces_dialog() {
            dialog::plain_lines(
                &mut state.panel.agent_transcript,
                dialog_area.width,
                &dialog_opts,
                anim_frame,
                welcome_ms,
            )
        } else {
            dialog::plain_lines(
                &mut state.transcript,
                dialog_area.width,
                &dialog_opts,
                anim_frame,
                welcome_ms,
            )
        };
        let text = extract_selection_text(&lines, &state.selection, dialog_area.width);
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

/// Paint the part of the selection that is currently on screen. The selection
/// itself is in content coordinates, so scrolling moves the highlight with the
/// text rather than leaving it on the same rows.
pub(crate) fn apply_selection_highlight(
    frame: &mut Frame,
    selection: &crate::state::Selection,
    dialog_area: Rect,
    first_visible_line: usize,
    selected_bg: ratatui::style::Style,
) {
    if !selection.is_active() || selection.is_empty() || dialog_area.width == 0 {
        return;
    }
    for screen_row in 0..dialog_area.height {
        let line = first_visible_line + screen_row as usize;
        let Some(col_range) = selection.col_range_for_line(line, dialog_area.width) else {
            continue;
        };
        for col in col_range {
            let x = dialog_area.x + col;
            let y = dialog_area.y + screen_row;
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_style(cell.style().patch(selected_bg));
            }
        }
    }
}

/// Upper bound on one copy, so selecting a very long transcript cannot build an
/// unbounded string in memory. 4 MiB of text is far past any real selection.
pub const COPY_MAX_CHARS: usize = 4 * 1024 * 1024;

/// The selected text, taken from the transcript's own lines rather than from the
/// screen buffer, so a selection that runs past the viewport copies in full.
pub(crate) fn extract_selection_text(
    lines: &[String],
    selection: &crate::state::Selection,
    width: u16,
) -> String {
    let Some(line_range) = selection.line_range() else {
        return String::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut budget = COPY_MAX_CHARS;
    for line_idx in line_range {
        let Some(col_range) = selection.col_range_for_line(line_idx, width) else {
            continue;
        };
        let Some(text) = lines.get(line_idx) else {
            continue;
        };
        let start = *col_range.start() as usize;
        let end = *col_range.end() as usize;
        let slice: String = text
            .chars()
            .skip(start)
            .take(end.saturating_sub(start) + 1)
            .collect();
        let slice = slice.trim_end().to_string();
        if slice.chars().count() > budget {
            out.push(slice.chars().take(budget).collect());
            break;
        }
        budget -= slice.chars().count();
        out.push(slice);
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eased_height_unfolds_ease_out_to_full() {
        let h = 20;
        assert_eq!(eased_height(h, 0), 1, "starts at the 1-row minimum");
        // Ease-out: well past half the height by mid-window.
        let mid = eased_height(h, crate::state::OVERLAY_GROW_MS / 2);
        assert!(mid > h / 2, "mid was {mid}");
        assert!(mid < h, "mid was {mid}");
        assert_eq!(eased_height(h, crate::state::OVERLAY_GROW_MS), h);
        assert_eq!(eased_height(h, crate::state::OVERLAY_GROW_MS * 10), h);
        assert_eq!(eased_height(0, 0), 0);
        assert_eq!(eased_height(1, 0), 1);
        // Monotone non-decreasing.
        let mut prev = 0;
        for ms in 0..crate::state::OVERLAY_GROW_MS {
            let cur = eased_height(h, ms);
            assert!(cur >= prev, "shrank at {ms}ms");
            prev = cur;
        }
    }

    #[test]
    fn grow_rect_stays_inside_area_and_centres() {
        let area = Rect::new(10, 5, 40, 24);
        let grown = grow_rect(area, crate::state::OVERLAY_GROW_MS / 2);
        assert!(grown.height < area.height);
        assert!(grown.y >= area.y);
        assert!(grown.bottom() <= area.bottom());
        // Centred: equal slack above and below (off by one on odd heights).
        let top = grown.y - area.y;
        let bottom = area.bottom() - grown.bottom();
        assert!((top as i32 - bottom as i32).abs() <= 1);
        // Bottom-anchored variant keeps the bottom edge.
        let grown_b = grow_rect_bottom(area, crate::state::OVERLAY_GROW_MS / 2);
        assert_eq!(grown_b.bottom(), area.bottom());
    }

    #[test]
    fn focus_key_distinguishes_layers_not_indexes() {
        assert_eq!(focus_key(&Focus::Normal), 0);
        assert_eq!(focus_key(&Focus::SelectAgent { index: 2 }), 1);
        assert_ne!(
            focus_key(&Focus::Overlay(Overlay::Help)),
            focus_key(&Focus::Overlay(Overlay::RailOverlay))
        );
        // Index changes inside one picker keep the key — no replayed unfold.
        assert_eq!(
            focus_key(&Focus::Overlay(Overlay::EffortPicker { index: 0 })),
            focus_key(&Focus::Overlay(Overlay::EffortPicker { index: 4 }))
        );
        assert_ne!(
            focus_key(&Focus::Normal),
            focus_key(&Focus::BgList { index: 0 })
        );
    }
}
