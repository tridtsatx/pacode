//! The shared base for the centred submenus (design spec §3): one frame, seven
//! zones, typed rows, declared actions, a shared scroll window and a
//! navigation stack.
//!
//! A submenu does not draw — on every frame it builds a [`MenuView`] of typed
//! [`MenuRow`]s plus its declared [`MenuAction`]s, and [`draw`] renders it:
//! the breadcrumb border, tabs, filter line, the list with the detail panel
//! (or its two collapsed lines), the status line and the hint edge. Only
//! [`MenuStack`] and [`MenuUiState`] live between frames; rows and views die
//! with the frame (§3.6).
//!
//! The key routing itself (`keys_menu`) is a later task; the base already owns
//! the base-key table ([`is_base_key`]) that dispatch must defer to (§3.4).

mod layout;
mod model;
mod render;
mod rows;
mod stack;
mod view;

pub use layout::{
    COLLAPSED_DETAIL_LINES, DETAIL_MIN_INNER, DETAIL_MIN_PANEL, MIN_WIDTH, TOP_OFFSET,
    WIDTH_PERCENT, detail_panel_width, inner_height, overlay_area, overlay_width, resolve_window,
    row_height, shows_filter, shows_tabs, window_rows,
};
pub use model::{Detail, DetailLine, Mark, MenuRow, RowId, ValueKind, resolve_cursor, selectable};
pub use render::{draw, hint_line};
pub use stack::{MAX_DEPTH, MenuLevel, MenuStack};
pub use view::{
    ActionChip, EmptyState, FilterMode, KeyBinding, MenuAction, MenuId, MenuMode, MenuUiState,
    MenuView, StatusKind, StatusLine, Tab, Tabs, is_base_key,
};
