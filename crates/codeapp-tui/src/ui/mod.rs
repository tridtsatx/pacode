//! Widgets. `draw` renders one frame from `AppState` using `ScreenLayout`.
//!
//! Each widget is a function `fn draw_x(frame: &mut Frame, area: Rect, state: &mut AppState, opts: &RenderOptions)`.
//! Widgets may mutate render caches inside the state but nothing else.

pub mod anim;
pub mod dialog;
pub mod footer;
pub mod input;
pub mod overlays;
pub mod panel;
pub mod rail;
pub mod rail_session;
pub mod toast;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use codeapp_render::RenderOptions;
use codeapp_types::TranscriptKind;
use codeapp_types::transcript::ToolStatus;

use crate::layout::ScreenLayout;
use crate::state::transcript::CellKind;
use crate::state::{AppState, Focus};

/// Draw the whole screen; returns the layout used (for mouse hit-testing).
pub fn draw(frame: &mut Frame, state: &mut AppState) -> ScreenLayout {
    let panel_open = state.panel.target.is_some() || matches!(state.focus, Focus::Panel { .. });
    let temp_layout = crate::layout::compute(frame.area(), 1, panel_open);
    let input_lines = state.input.wrapped_lines(temp_layout.input.width);
    let layout = crate::layout::compute(frame.area(), input_lines, panel_open);

    let opts = RenderOptions::new(frame.area().width, state.config.ui.ascii_only);

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

    if layout.dialog.width > 0 && layout.dialog.height > 0 {
        let dialog_opts = RenderOptions::new(layout.dialog.width, state.config.ui.ascii_only);
        if state.turn_active && layout.dialog.height > 1 {
            let trans_h = layout.dialog.height - 1;
            let trans_area = Rect::new(
                layout.dialog.x,
                layout.dialog.y,
                layout.dialog.width,
                trans_h,
            );
            let anim_area = Rect::new(
                layout.dialog.x,
                layout.dialog.bottom() - 1,
                layout.dialog.width,
                1,
            );
            dialog::draw(
                frame,
                trans_area,
                &mut state.transcript,
                &dialog_opts,
                state.anim_frame,
            );

            let running_tool = state.transcript.cells.iter().rev().find_map(|c| {
                if let CellKind::Item(TranscriptKind::ToolCall {
                    status: ToolStatus::Running,
                    title,
                    ..
                }) = &c.kind
                {
                    Some(title.clone())
                } else {
                    None
                }
            });
            let activity = running_tool.unwrap_or_else(|| "думает…".to_string());
            let elapsed_ms = state
                .turn_started_at
                .map(|t| {
                    std::time::Instant::now()
                        .saturating_duration_since(t)
                        .as_millis() as u64
                })
                .or_else(|| {
                    let main_agent = state.rail.agents.iter().find(|a| a.id.is_main());
                    main_agent.map(|a| a.duration_ms(codeapp_types::time::now_ms()))
                })
                .unwrap_or(0);
            let anim_line = anim::render_pacman_line(
                state.anim_frame,
                24,
                &dialog_opts,
                &activity,
                elapsed_ms,
                layout.dialog.width as usize,
            );
            frame.render_widget(Paragraph::new(anim_line), anim_area);
        } else {
            dialog::draw(
                frame,
                layout.dialog,
                &mut state.transcript,
                &dialog_opts,
                state.anim_frame,
            );
        }
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
        let panel_opts = RenderOptions::new(panel_rect.width, state.config.ui.ascii_only);
        panel::draw(frame, panel_rect, state, &panel_opts);
    }

    input::draw(frame, &layout, state, &opts);
    footer::draw(frame, layout.footer, state, &opts);

    if layout.toast.width > 0 && layout.toast.height > 0 {
        toast::draw(frame, layout.toast, state, &opts);
    }

    let dialog_or_full = if layout.dialog.width > 0 {
        layout.dialog
    } else {
        frame.area()
    };
    overlays::draw(frame, dialog_or_full, state, &opts);

    layout
}
