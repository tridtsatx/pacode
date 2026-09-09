//! Mouse handling for the TUI.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::keys::{Action, handle_navigate_down, handle_navigate_up};
use crate::layout::ScreenLayout;
use crate::state::AppState;

pub fn handle_mouse(state: &mut AppState, mouse: MouseEvent, layout: &ScreenLayout) -> Vec<Action> {
    state.dirty = true;
    let col = mouse.column;
    let row = mouse.row;

    // Shift+drag is left to the terminal (mouse capture already grabs plain drag).
    if mouse
        .modifiers
        .contains(crossterm::event::KeyModifiers::SHIFT)
    {
        return vec![];
    }

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            state.selection.clear();
            if layout.dialog.contains((col, row).into()) {
                state.selection.start(col, row, layout.dialog);
            }
            return vec![];
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if state.selection.dragging {
                state.selection.drag(col, row, layout.dialog);
            }
            return vec![];
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if state.selection.dragging {
                state.selection.finish();
            }
            return vec![];
        }
        MouseEventKind::ScrollDown => {
            let delta = -3;
            if layout.dialog.contains((col, row).into()) {
                state
                    .transcript
                    .scroll_by(delta, 1000, layout.dialog.height as usize);
            } else if layout.input.contains((col, row).into()) {
                let max_scroll = state
                    .input
                    .wrap_lines(layout.input.width)
                    .len()
                    .saturating_sub(layout.input.height as usize);
                state.input.input_scroll = (state.input.input_scroll + 1).min(max_scroll);
            } else if let Some(panel) = layout.panel {
                if panel.contains((col, row).into()) {
                    state
                        .panel
                        .agent_transcript
                        .scroll_by(delta, 1000, panel.height as usize);
                }
            } else if layout.rail.contains((col, row).into()) {
                handle_navigate_down(state);
            }
        }
        MouseEventKind::ScrollUp => {
            let delta = 3;
            if layout.dialog.contains((col, row).into()) {
                let viewport = layout.dialog.height as usize;
                state.transcript.scroll_by(delta, 1000, viewport);
                if state.transcript.is_at_top(1000, viewport) && state.can_load_history() {
                    state.transcript.loading_history = true;
                    return vec![Action::LoadHistory];
                }
            } else if layout.input.contains((col, row).into()) {
                state.input.input_scroll = state.input.input_scroll.saturating_sub(1);
            } else if let Some(panel) = layout.panel {
                if panel.contains((col, row).into()) {
                    state
                        .panel
                        .agent_transcript
                        .scroll_by(delta, 1000, panel.height as usize);
                }
            } else if layout.rail.contains((col, row).into()) {
                handle_navigate_up(state);
            }
        }
        _ => return vec![],
    }

    vec![]
}
