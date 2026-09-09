//! Widgets. `draw` renders one frame from `AppState` using `ScreenLayout`.
//!
//! Each widget is a function `fn draw_x(frame: &mut Frame, area: Rect, state: &mut AppState, opts: &RenderOptions)`.
//! Widgets may mutate render caches inside the state but nothing else.

pub mod dialog;
pub mod footer;
pub mod input;
pub mod overlays;
pub mod panel;
pub mod rail;
pub mod toast;

use ratatui::Frame;

use crate::layout::ScreenLayout;
use crate::state::AppState;

/// Draw the whole screen; returns the layout used (for mouse hit-testing).
pub fn draw(frame: &mut Frame, state: &mut AppState) -> ScreenLayout {
    let _ = (frame, state);
    todo!("ui::draw")
}
