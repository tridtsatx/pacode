//! Mouse handling for the TUI.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::keys::{Action, handle_navigate_down, handle_navigate_up};
use crate::layout::ScreenLayout;
use crate::state::AppState;

/// Scrolling with the button still held keeps growing the selection: the pointer
/// has not moved, but different text is under it now, so the moving end is
/// re-read from the new content line at the pointer.
fn extend_selection_after_scroll(
    state: &mut AppState,
    col: u16,
    row: u16,
    dialog: ratatui::layout::Rect,
) {
    if !state.selection.dragging {
        return;
    }
    let first = visible_transcript(state).first_visible_line;
    state.selection.drag(col, row, dialog, first);
}

/// The transcript the dialog column is showing right now.
fn visible_transcript(state: &AppState) -> &crate::state::Transcript {
    if state.agent_replaces_dialog() {
        &state.panel.agent_transcript
    } else {
        &state.transcript
    }
}

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
                let first = visible_transcript(state).first_visible_line;
                state.selection.start(col, row, layout.dialog, first);
            }
            return vec![];
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if state.selection.dragging {
                let first = visible_transcript(state).first_visible_line;
                state.selection.drag(col, row, layout.dialog, first);
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
                // In replace mode the dialog column shows the subagent, so the
                // wheel has to move the transcript that is actually on screen.
                if state.agent_replaces_dialog() {
                    state.panel.agent_transcript.scroll_by(delta);
                } else {
                    state.transcript.scroll_by(delta);
                }
                extend_selection_after_scroll(state, col, row, layout.dialog);
            } else if layout.input.contains((col, row).into()) {
                let max_scroll = state
                    .input
                    .wrap_lines(layout.input.width)
                    .len()
                    .saturating_sub(layout.input.height as usize);
                state.input.input_scroll = (state.input.input_scroll + 1).min(max_scroll);
            } else if let Some(panel) = layout.panel {
                if panel.contains((col, row).into()) {
                    state.panel.agent_transcript.scroll_by(delta);
                }
            } else if layout.rail.contains((col, row).into()) {
                handle_navigate_down(state);
            }
        }
        MouseEventKind::ScrollUp => {
            let delta = 3;
            if layout.dialog.contains((col, row).into()) {
                // In replace mode the dialog column shows the subagent, so the
                // wheel has to move the transcript that is actually on screen.
                if state.agent_replaces_dialog() {
                    state.panel.agent_transcript.scroll_by(delta);
                } else {
                    state.transcript.scroll_by(delta);
                }
                extend_selection_after_scroll(state, col, row, layout.dialog);
                if state.transcript.is_at_top() && state.can_load_history() {
                    state.transcript.loading_history = true;
                    return vec![Action::LoadHistory];
                }
            } else if layout.input.contains((col, row).into()) {
                state.input.input_scroll = state.input.input_scroll.saturating_sub(1);
            } else if let Some(panel) = layout.panel {
                if panel.contains((col, row).into()) {
                    state.panel.agent_transcript.scroll_by(delta);
                }
            } else if layout.rail.contains((col, row).into()) {
                handle_navigate_up(state);
            }
        }
        _ => return vec![],
    }

    vec![]
}
