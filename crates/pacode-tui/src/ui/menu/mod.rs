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
//! Key routing lives in `keys_menu` (§3.4); the provider table in
//! [`view_for`] is the one place a [`MenuId`] meets its `view` — every
//! `MenuId` has a `ui/menu/<name>.rs` provider (§3.1).

mod layout;
mod model;
mod render;
mod rows;
mod stack;
mod view;
// `pub(crate)`: `keys_menu` calls each menu's `on_row`/`on_action`/`on_edit`
// hooks directly — the `view` providers stay behind `view_for`. `files` and
// `background` are also reached from `app`'s deferred jobs and overlay
// preparation (preview want / tail want).
pub(crate) mod background;
pub(crate) mod files;
// `pub(crate)`: overlay preparation records the level's wants and `keys_menu`
// calls its `on_row`/`on_action`/`on_open` hooks — same contract as `files`.
pub(crate) mod git;
pub(crate) mod git_branches;
pub(crate) mod help;
pub(crate) mod import;
pub(crate) mod keys;
pub(crate) mod mcp;
pub(crate) mod plugins;
pub(crate) mod root;
pub(crate) mod sessions;
pub(crate) mod settings;
mod skills;

use crate::state::AppState;

pub use layout::{
    COLLAPSED_DETAIL_LINES, DETAIL_MIN_INNER, DETAIL_MIN_PANEL, MIN_WIDTH, TOP_OFFSET,
    WIDTH_PERCENT, detail_panel_width, inner_height, overlay_area, overlay_width, resolve_window,
    row_height, shows_filter, shows_tabs, window_rows,
};
pub use model::{
    Detail, DetailLine, LabelSpan, LabelTone, Mark, MenuRow, RowId, ValueKind, resolve_cursor,
    selectable,
};
pub use render::{draw, draw_in, hint_line};
// `pub(crate)`: the MCP log panel's header reuses the row glyph table so a
// server reads the same in the menu and in its log view.
pub(crate) use rows::mark_glyph;
pub use stack::{MAX_DEPTH, MenuLevel, MenuStack};
pub use view::{
    ActionChip, EmptyState, FilterMode, KeyBinding, MenuAction, MenuId, MenuMode, MenuUiState,
    MenuView, StatusKind, StatusLine, Tab, Tabs, is_base_key,
};

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
        MenuId::GitBranches => git_branches::view(state, ui),
        MenuId::Plugins => plugins::view(state, ui),
        MenuId::Mcp => mcp::view(state, ui),
        MenuId::Keys => keys::view(state, ui),
        MenuId::Sessions => sessions::view(state, ui),
        MenuId::Settings => settings::view(state, ui),
        MenuId::Files => files::view(state, ui),
        MenuId::Background => background::view(state, ui),
        MenuId::Import => import::view(state, ui),
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
