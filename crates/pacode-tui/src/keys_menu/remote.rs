//! Open hooks and the remote-filter debounce (§3.2) — the pieces of
//! `keys_menu` that turn a freshly entered level and a `FilterMode::Remote`
//! query into daemon requests.
//!
//! A query edit under a remote filter re-arms `MenuUiState::remote_at`; the
//! event loop sleeps on [`remote_deadline`] (no pending send — no timer) and
//! calls [`remote_filter_due`] when it fires. `descend` flushes a pending
//! send through [`flush_remote`] before the level's `ui` is saved away, and
//! a tab switch onto an empty remote list asks for it via [`tab_follow_up`]
//! — the fetch-on-entry the legacy pickers did.

use std::time::{Duration, Instant};

use crate::keys::Action;
use crate::state::{AppState, Focus, Overlay};
use crate::ui::menu::{self, FilterMode, MenuId, MenuStack, MenuUiState, MenuView, selectable};

/// Hooks a freshly entered level runs — the refresh-on-open the legacy
/// overlays issued (`ListPlugins`+`BrowseMarketplace`, `ListMcpServers`, …),
/// or Import's filesystem scan. Runs on the direct entry (`open`) and on a
/// hub descent alike. `&mut`: Import writes its scan into `state.import`.
pub(super) fn on_open(state: &mut AppState, id: MenuId) -> Vec<Action> {
    match id {
        MenuId::Plugins => menu::plugins::on_open(state),
        MenuId::Mcp => menu::mcp::on_open(state),
        MenuId::Sessions => menu::sessions::on_open(state),
        MenuId::Import => menu::import::on_open(state),
        MenuId::Root
        | MenuId::Help
        | MenuId::Settings
        | MenuId::Keys
        | MenuId::Files
        | MenuId::Background
        | MenuId::Skills
        | MenuId::Git => vec![],
    }
}

/// The request a level's `FilterMode::Remote` sends for `query` — one arm
/// per provider that declares it (§3.2). Levels without a remote filter
/// never arm `ui.remote_at`, so the miss arm is unreachable plumbing.
fn remote_query(state: &AppState, id: MenuId, query: &str) -> Vec<Action> {
    match id {
        MenuId::Plugins => menu::plugins::remote_query(state, query),
        MenuId::Root
        | MenuId::Help
        | MenuId::Settings
        | MenuId::Mcp
        | MenuId::Keys
        | MenuId::Files
        | MenuId::Sessions
        | MenuId::Import
        | MenuId::Background
        | MenuId::Skills
        | MenuId::Git => vec![],
    }
}

/// A real query edit under `FilterMode::Remote` re-arms the debounce: the
/// send instant is `debounce_ms` after the *last* keystroke, so a burst of
/// typing produces one request (§3.2). Arming uses wall-clock `now`; the
/// due check lives in [`remote_filter_due`].
pub(super) fn arm_remote(ui: &mut MenuUiState, view: &MenuView, changed: bool) {
    if !changed {
        return;
    }
    if let Some(FilterMode::Remote { debounce_ms }) = view.filter {
        ui.remote_at = Some(Instant::now() + Duration::from_millis(debounce_ms));
    }
}

/// The instant the open menu's remote-filter debounce fires — the event
/// loop arms a sleep on it; `None` means no timer at all.
pub fn remote_deadline(state: &AppState) -> Option<Instant> {
    if let Focus::Overlay(Overlay::Menu { ui, .. }) = &state.focus {
        ui.remote_at
    } else {
        None
    }
}

/// The debounce fired: if the armed instant passed, send the current
/// level's remote-filter request once and disarm (§3.2).
pub fn remote_filter_due(state: &mut AppState, now: Instant) -> Vec<Action> {
    let pending = {
        let Focus::Overlay(Overlay::Menu { stack, ui }) = &mut state.focus else {
            return vec![];
        };
        match ui.remote_at {
            Some(at) if now >= at => {
                ui.remote_at = None;
                stack.current().map(|l| (l.id, ui.query.clone()))
            }
            _ => None,
        }
    };
    let Some((id, query)) = pending else {
        return vec![];
    };
    state.dirty = true;
    remote_query(state, id, &query)
}

/// Send the pending remote query now — called by `descend` before the
/// level's `ui` is saved away, so a half-debounced edit is not lost.
pub(super) fn flush_remote(
    state: &AppState,
    stack: &MenuStack,
    ui: &mut MenuUiState,
) -> Vec<Action> {
    if ui.remote_at.take().is_none() {
        return vec![];
    }
    let Some(level) = stack.current() else {
        return vec![];
    };
    remote_query(state, level.id, &ui.query)
}

/// After a tab switch: landing on a remote-filtered tab whose list is still
/// empty asks for it — the same fetch-on-entry the legacy picker did.
pub(super) fn tab_follow_up(
    state: &AppState,
    stack: &MenuStack,
    ui: &mut MenuUiState,
) -> Vec<Action> {
    if stack.current().is_none() {
        return vec![];
    }
    let view = menu::view_for(stack, state, ui);
    if matches!(view.filter, Some(FilterMode::Remote { .. }))
        && selectable(&view.rows).next().is_none()
    {
        return remote_query(state, view.id, &ui.query);
    }
    vec![]
}
