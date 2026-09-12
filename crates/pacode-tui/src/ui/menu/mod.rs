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
//! Key routing lives in `keys_menu` (§3.4); the provider table below is the
//! one place a [`MenuId`] meets its `view` — a port adds `ui/menu/<name>.rs`
//! with `pub fn view(state: &AppState, ui: &MenuUiState) -> MenuView` and
//! flips its [`has_provider`] arm (§3.1).

mod git;
mod layout;
mod model;
mod render;
mod rows;
mod stack;
mod view;
// `pub(crate)`: `keys_menu` calls each menu's `on_row`/`on_action`/`on_edit`
// hooks directly — the `view` providers stay behind `view_for`.
pub(crate) mod help;
pub(crate) mod root;
mod skills;

use crate::state::AppState;

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

/// Whether `id` has a `ui/menu/<name>::view` provider. `false` entries are
/// served by the legacy-overlay bridge in `keys_menu` (TODO T19); a port
/// flips its arm to `true` the moment its provider lands.
pub fn has_provider(id: MenuId) -> bool {
    match id {
        MenuId::Root | MenuId::Help | MenuId::Skills | MenuId::Git => true,
        MenuId::Settings
        | MenuId::Plugins
        | MenuId::Mcp
        | MenuId::Keys
        | MenuId::Files
        | MenuId::Sessions
        | MenuId::Import
        | MenuId::Background => false,
    }
}

/// The view of the stack's top level — the one place `MenuId` meets its
/// provider (§3.1). The base then patches the breadcrumb from the parent
/// levels and, for `FilterMode::Local`, applies the query itself — providers
/// return unfiltered rows.
pub fn view_for(stack: &MenuStack, state: &AppState, ui: &MenuUiState) -> MenuView {
    let Some(level) = stack.current() else {
        return MenuView::new(MenuId::Root);
    };
    let mut view = match level.id {
        MenuId::Root => root::view(state, ui),
        MenuId::Help => help::view(state, ui),
        MenuId::Skills => skills::view(state, ui),
        MenuId::Git => git::view(state, ui),
        // No provider yet — `has_provider` keeps these out of the stack, so
        // the arm is a defensive stub, not a real path (TODO T19).
        MenuId::Settings
        | MenuId::Plugins
        | MenuId::Mcp
        | MenuId::Keys
        | MenuId::Files
        | MenuId::Sessions
        | MenuId::Import
        | MenuId::Background => MenuView::new(level.id),
    };
    // The breadcrumb between `Menu` and this level is the parents' titles
    // (§3.2): `Root › X` needs nothing, `Root › X › Y` names `X`.
    view.breadcrumb = stack
        .levels()
        .iter()
        .skip(1)
        .take(stack.depth().saturating_sub(2))
        .map(|l| l.id.title().to_string())
        .collect();
    if matches!(view.filter, Some(FilterMode::Local)) {
        view.apply_filter(&ui.query);
    }
    view
}
