//! Key handling for the menu base (§3.4). The base owns navigation, tabs, the
//! filter line, `enter` by row kind, `esc`/`ctrl+c` stack control and `?`;
//! what a submenu does with a row, a declared action or an edit lives in its
//! `ui/menu/<name>.rs` behind `on_row` / `on_action` / `on_edit` — this file
//! is the dispatch table between the base keys and those hooks.
//!
//! Entries: [`open`] — a direct route (`/plugins`, `ctrl+,`, double-Shift) —
//! and [`handle_menu_key`], which `keys::handle_key` routes every key to while
//! `Focus::Overlay(Overlay::Menu)` is up. Until T19 unported sections hand off
//! to their legacy overlay through [`open_legacy`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::Request;
use ratatui::layout::Rect;

use crate::binding::matches_key;
use crate::keys::Action;
use crate::state::{AppState, Focus, Overlay};
use crate::ui::menu::{
    self, MenuAction, MenuId, MenuLevel, MenuMode, MenuRow, MenuStack, MenuUiState, RowId,
    ValueKind, resolve_cursor, selectable, shows_filter, shows_tabs,
};

#[cfg(test)]
#[path = "keys_menu_tests.rs"]
mod keys_menu_tests;

/// Chars the filter query holds — it is a matcher, not an editor. Same cap as
/// the paste path (`paste::FILTER_MAX_CHARS`) so typing and pasting agree.
const QUERY_MAX_CHARS: usize = crate::paste::FILTER_MAX_CHARS;

/// Open a menu directly — `/plugins`, `.`, `ctrl+,`, double-Shift, `?`.
/// A provider-backed menu becomes a one-level stack (§3.5); an unported one
/// opens its legacy overlay — the bridge dies with the old variants (TODO T19).
pub fn open(state: &mut AppState, id: MenuId) -> Vec<Action> {
    if menu::has_provider(id) {
        log::debug!("open menu: {}", id.slug());
        state.focus = Focus::Overlay(Overlay::Menu {
            stack: MenuStack::root(id),
            ui: MenuUiState::default(),
        });
        state.dirty = true;
        vec![]
    } else {
        open_legacy(state, id)
    }
}

/// `keys::handle_key` routes here while a menu is open. `stack`/`ui` are the
/// variant payloads taken out of `state.focus`; the caller puts the overlay
/// back when the stack is still non-empty and `state.focus` is back to
/// `Normal` — handlers close the menu either by emptying the stack or by
/// pointing `focus` at something else.
pub fn handle_menu_key(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    key: KeyEvent,
) -> Vec<Action> {
    if stack.current().is_none() {
        return vec![];
    }
    state.dirty = true;
    // `ctrl+c` is the stack-wide abort (§3.4) in List and Edit alike; only
    // Capture forwards it — §5.4 hands the submenu every key but `esc`.
    if !matches!(ui.mode, MenuMode::Capture { .. })
        && key.code == KeyCode::Char('c')
        && key.modifiers.contains(KeyModifiers::CONTROL)
    {
        while stack.pop().is_some() {}
        return vec![];
    }
    let view = menu::view_for(stack, state, ui);
    match ui.mode {
        MenuMode::Edit { .. } => edit_key(state, stack, ui, &view, key),
        MenuMode::Capture { .. } => capture_key(ui, key),
        MenuMode::List => list_key(state, stack, ui, &view, key),
    }
}

/// The temporary bridge: a `MenuId` without a provider opens the overlay the
/// section has today — the same bodies the slash commands and keymap actions
/// used to run (TODO T19: this function dies with the legacy variants).
fn open_legacy(state: &mut AppState, id: MenuId) -> Vec<Action> {
    log::debug!("open menu→legacy: {}", id.slug());
    state.dirty = true;
    match id {
        MenuId::Settings => {
            state.config_view = crate::ui::config_view::ConfigViewState::new(&state.config);
            state.focus = Focus::Overlay(Overlay::ConfigPicker);
            vec![]
        }
        MenuId::Plugins => {
            let market_source = state.config.plugins.marketplace.clone();
            let has_market = !market_source.is_empty();
            // §8.2: open on the cached copies — `lists` carries the refresh's
            // progress; `stale` mirrors the daemon-reported flag on the reply.
            state.focus = Focus::Overlay(Overlay::PluginsPicker {
                index: 0,
                plugins: state.plugins.clone(),
                tab: crate::state::PluginsTab::Installed,
                market: state.marketplace.clone(),
                query: String::new(),
                loading: has_market,
                stale: matches!(
                    state.lists.badge(crate::cache::ListKind::Marketplace),
                    Some(crate::cache::Badge::OfflineCopy)
                ),
            });
            let mut actions = vec![Action::Send(Request::ListPlugins)];
            if has_market {
                actions.push(Action::Send(Request::BrowseMarketplace {
                    source: market_source,
                    query: String::new(),
                }));
            }
            actions
        }
        MenuId::Mcp => {
            // §8.2: the cached server list is on screen immediately; the
            // `loading…`/`offline copy` badge comes from `lists`.
            state.focus = Focus::Overlay(Overlay::McpPicker {
                index: 0,
                servers: state.mcp_servers.clone(),
                loading: true,
            });
            vec![Action::Send(Request::ListMcpServers)]
        }
        MenuId::Keys => {
            state.focus = Focus::Overlay(Overlay::KeysPicker {
                index: 0,
                capturing: false,
            });
            vec![]
        }
        MenuId::Files => {
            state.focus = Focus::Overlay(Overlay::Files { index: 0 });
            vec![]
        }
        MenuId::Sessions => {
            state.focus = Focus::Overlay(Overlay::SessionPicker {
                query: String::new(),
                index: 0,
            });
            vec![Action::Send(Request::ListSessions { limit: 50 })]
        }
        MenuId::Import => {
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let overlay_state =
                crate::ui::import::ImportOverlayState::discover(&home, &cwd, &state.paths);
            state.focus = Focus::Overlay(Overlay::Import(overlay_state));
            vec![]
        }
        MenuId::Background => {
            state.focus = Focus::BgList { index: 0 };
            vec![]
        }
        // Providers exist for these (`has_provider` is true) — the bridge
        // never sees them; the arm keeps the match exhaustive.
        MenuId::Root | MenuId::Help | MenuId::Skills | MenuId::Git => vec![],
    }
}

// ─── List mode ──────────────────────────────────────────────────────────────

/// The browsing keys (§3.4): esc peels a level, ctrl+c closes the stack, `?`
/// opens Help on top — then movement, tabs, filter input and `enter`.
fn list_key(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    view: &menu::MenuView,
    key: KeyEvent,
) -> Vec<Action> {
    match key.code {
        // `esc` — one level back; the last level pops into "closed" (§3.4).
        KeyCode::Esc => {
            stack.pop();
            if let Some(parent) = stack.current() {
                *ui = parent.ui();
            }
            return vec![];
        }
        // `?` — Help on top of the current level (§3.4). A live filter owns
        // printable keys (§4.3's "in a non-empty prompt it is a plain char"),
        // so with a filter line up `?` falls through to the query instead.
        // On Help itself it would only stack Help on Help — a no-op.
        KeyCode::Char('?')
            if !shows_filter(view)
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            if view.id != MenuId::Help {
                return descend(state, stack, ui, MenuId::Help);
            }
            return vec![];
        }
        _ => {}
    }
    list_key_inner(state, stack, ui, view, key)
}

fn list_key_inner(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    view: &menu::MenuView,
    key: KeyEvent,
) -> Vec<Action> {
    let filtering = shows_filter(view);
    let bare = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);

    // Vertical movement — arrows and ctrl+p/ctrl+n always; j/k join when no
    // filter line owns the printable keys (§3.4).
    let up = key.code == KeyCode::Up
        || (key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL))
        || (!filtering && key.code == KeyCode::Char('k') && bare);
    let down = key.code == KeyCode::Down
        || (key.code == KeyCode::Char('n') && key.modifiers.contains(KeyModifiers::CONTROL))
        || (!filtering && key.code == KeyCode::Char('j') && bare);
    if up {
        move_sel(state, ui, view, -1);
        return vec![];
    }
    if down {
        move_sel(state, ui, view, 1);
        return vec![];
    }

    match key.code {
        KeyCode::PageUp => {
            let page = page_rows(state, view, ui) as isize;
            move_sel(state, ui, view, -page);
            vec![]
        }
        KeyCode::PageDown => {
            let page = page_rows(state, view, ui) as isize;
            move_sel(state, ui, view, page);
            vec![]
        }
        KeyCode::Home => {
            if let Some((_, id)) = selectable(&view.rows).next() {
                ui.cursor = id;
                sync_scroll(state, ui, view);
            }
            vec![]
        }
        KeyCode::End => {
            if let Some((_, id)) = selectable(&view.rows).last() {
                ui.cursor = id;
                sync_scroll(state, ui, view);
            }
            vec![]
        }
        // Both spellings of shift+tab arrive in the wild (`BackTab` and
        // `Tab`+SHIFT).
        KeyCode::Tab if !key.modifiers.contains(KeyModifiers::SHIFT) => {
            tab_step(ui, view, 1);
            vec![]
        }
        KeyCode::BackTab | KeyCode::Tab => {
            tab_step(ui, view, -1);
            vec![]
        }
        KeyCode::Enter => enter_row(state, stack, ui, view),
        KeyCode::Backspace if filtering => {
            ui.query.pop();
            ui.cursor = RowId::default();
            vec![]
        }
        KeyCode::Char(c) if bare => {
            if filtering {
                if ui.query.chars().count() < QUERY_MAX_CHARS {
                    ui.query.push(c);
                }
                ui.cursor = RowId::default();
                return vec![];
            }
            run_action(state, stack, ui, view, key)
        }
        _ => run_action(state, stack, ui, view, key),
    }
}

/// `enter` on a Submenu row or `?`: save the level's UI state and push the
/// target — or hand off to the legacy overlay while the target has no
/// provider (§3.5, TODO T19).
fn descend(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    target: MenuId,
) -> Vec<Action> {
    if !menu::has_provider(target) {
        return open_legacy(state, target);
    }
    if let Some(level) = stack.current_mut() {
        level.save(ui);
    }
    if stack.push(MenuLevel::new(target)) {
        *ui = MenuUiState::default();
    }
    vec![]
}

/// `enter` by row kind (§3.4): Submenu descends, Value rows either cycle
/// (enum) or open the inline edit, everything else goes to the menu's
/// `on_row`.
fn enter_row(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    view: &menu::MenuView,
) -> Vec<Action> {
    let Some(sel) = resolve_cursor(&view.rows, &ui.cursor) else {
        return vec![];
    };
    match &view.rows[sel] {
        MenuRow::Section { .. } => vec![],
        MenuRow::Submenu { target, .. } => descend(state, stack, ui, *target),
        MenuRow::Value {
            id, value, kind, ..
        } => match kind {
            // Enums cycle in place; the rest open the inline edit (§3.4).
            ValueKind::Enum { .. } => row_enter(state, stack, ui, view.id, id),
            ValueKind::Int | ValueKind::Float | ValueKind::Text => {
                ui.mode = MenuMode::Edit {
                    buffer: value.clone(),
                    cursor: value.chars().count(),
                };
                vec![]
            }
        },
        MenuRow::Item { id, .. } | MenuRow::Toggle { id, .. } | MenuRow::Check { id, .. } => {
            row_enter(state, stack, ui, view.id, id)
        }
    }
}

/// `enter` on a non-Submenu row — one arm per menu, filled by its port
/// (`ui/menu/<name>::on_row`). The row covers `Item`, `Check`, `Toggle` and
/// enum `Value` flips; the hook knows its rows by `RowId`.
fn row_enter(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    menu: MenuId,
    row: &RowId,
) -> Vec<Action> {
    match menu {
        MenuId::Root => menu::root::on_row(state, stack, ui, row),
        MenuId::Help => menu::help::on_row(state, stack, ui, row),
        MenuId::Settings
        | MenuId::Plugins
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

/// Declared actions — menu-level `view.actions` first, then the selected
/// row's chips (§3.4). `run_action` is also the fallback for keys nothing
/// else claimed, so non-char declared keys (F-keys, ctrl+…) work.
fn run_action(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    view: &menu::MenuView,
    key: KeyEvent,
) -> Vec<Action> {
    let sel = resolve_cursor(&view.rows, &ui.cursor);
    let row_actions = sel
        .and_then(|i| view.rows[i].detail())
        .map(|d| d.actions.as_slice())
        .unwrap_or(&[]);
    let row_id = sel.and_then(|i| view.rows[i].id().cloned());
    let action = view
        .actions
        .iter()
        .chain(row_actions)
        .find(|a| a.enabled && matches_key(&a.key, key));
    match action {
        Some(action) => menu_action(state, stack, ui, view.id, action, row_id.as_ref()),
        None => vec![],
    }
}

/// Declared-action dispatch (§3.4): one arm per menu, filled by its port —
/// `ui/menu/<name>::on_action` decides what its `MenuAction`s do. No T2 menu
/// declares actions, so every arm is a no-op placeholder for now.
fn menu_action(
    _state: &mut AppState,
    _stack: &mut MenuStack,
    _ui: &mut MenuUiState,
    _menu: MenuId,
    _action: &MenuAction,
    _row: Option<&RowId>,
) -> Vec<Action> {
    vec![]
}

// ─── Edit / capture modes ───────────────────────────────────────────────────

/// The inline-edit keys (§3.4): `esc` cancels, `enter` commits through the
/// menu's `on_edit`, everything else edits the buffer.
fn edit_key(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    view: &menu::MenuView,
    key: KeyEvent,
) -> Vec<Action> {
    match key.code {
        KeyCode::Esc => {
            ui.mode = MenuMode::List;
            vec![]
        }
        KeyCode::Enter => {
            let MenuMode::Edit { buffer, .. } = std::mem::replace(&mut ui.mode, MenuMode::List)
            else {
                return vec![];
            };
            let row =
                resolve_cursor(&view.rows, &ui.cursor).and_then(|i| view.rows[i].id().cloned());
            match row {
                Some(id) => edit_commit(state, stack, ui, view.id, &id, buffer),
                None => vec![],
            }
        }
        _ => {
            edit_buffer(&mut ui.mode, key);
            vec![]
        }
    }
}

/// Char-level editing of the `MenuMode::Edit` buffer — insert at the char
/// cursor, backspace/delete, home/end (§3.4).
fn edit_buffer(mode: &mut MenuMode, key: KeyEvent) {
    let MenuMode::Edit { buffer, cursor } = mode else {
        return;
    };
    let at_byte = |chars: usize| {
        buffer
            .char_indices()
            .nth(chars)
            .map(|(b, _)| b)
            .unwrap_or(buffer.len())
    };
    match key.code {
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            buffer.insert(at_byte(*cursor), c);
            *cursor += 1;
        }
        KeyCode::Backspace if *cursor > 0 => {
            buffer.remove(at_byte(*cursor - 1));
            *cursor -= 1;
        }
        KeyCode::Delete if *cursor < buffer.chars().count() => {
            buffer.remove(at_byte(*cursor));
        }
        KeyCode::Left => *cursor = cursor.saturating_sub(1),
        KeyCode::Right => *cursor = (*cursor + 1).min(buffer.chars().count()),
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = buffer.chars().count(),
        _ => {}
    }
}

/// `enter` in Edit mode — the port turns the buffer into a config write
/// (§3.4). No T2 menu produces `Value` rows, so every arm is a placeholder
/// until its port lands.
fn edit_commit(
    _state: &mut AppState,
    _stack: &mut MenuStack,
    _ui: &mut MenuUiState,
    _menu: MenuId,
    _row: &RowId,
    _buffer: String,
) -> Vec<Action> {
    vec![]
}

/// Key capture (§5.4): `esc` cancels; every other key is the capturing menu's
/// business — Keys (T6) is the only menu that enters this mode.
fn capture_key(ui: &mut MenuUiState, key: KeyEvent) -> Vec<Action> {
    if key.code == KeyCode::Esc {
        ui.mode = MenuMode::List;
    }
    vec![]
}

// ─── Movement / geometry ────────────────────────────────────────────────────

/// Move the cursor `delta` selectable rows; clamps at the edges. An unset
/// cursor anchors on the first selectable row (§3.4).
fn move_sel(state: &AppState, ui: &mut MenuUiState, view: &menu::MenuView, delta: isize) {
    let rows: Vec<RowId> = selectable(&view.rows).map(|(_, id)| id).collect();
    if rows.is_empty() {
        return;
    }
    let cur = rows.iter().position(|id| *id == ui.cursor).unwrap_or(0);
    let next = (cur as isize + delta).clamp(0, rows.len() as isize - 1) as usize;
    ui.cursor = rows[next].clone();
    sync_scroll(state, ui, view);
}

/// Tab step over the peer lists (§3.4); the row set changes, so cursor and
/// scroll reset.
fn tab_step(ui: &mut MenuUiState, view: &menu::MenuView, dir: isize) {
    if shows_tabs(view)
        && let Some(tabs) = &view.tabs
    {
        let n = tabs.items.len() as isize;
        ui.tab = ((ui.tab as isize + dir).rem_euclid(n)) as usize;
        ui.cursor = RowId::default();
        ui.scroll = 0;
    }
}

/// Rows a PageUp/PageDown jumps — the list-zone height the current view would
/// occupy (§3.4). The dialog column is the terminal minus the input/footer
/// chrome; being a row off is invisible — paging is approximate by nature.
fn page_rows(state: &AppState, view: &menu::MenuView, ui: &MenuUiState) -> usize {
    let dialog = Rect::new(0, 0, state.cols, state.rows.saturating_sub(6));
    let area = menu::overlay_area(dialog, view, ui);
    (area.height as usize)
        .saturating_sub(2 + usize::from(shows_tabs(view)) + usize::from(shows_filter(view)))
        .max(1)
}

/// Keep `ui.scroll` a valid window anchor for the cursor (§3.5): the window
/// start a level remembers survives the descent and the return.
fn sync_scroll(state: &AppState, ui: &mut MenuUiState, view: &menu::MenuView) {
    let dialog = Rect::new(0, 0, state.cols, state.rows.saturating_sub(6));
    let area = menu::overlay_area(dialog, view, ui);
    let inner_w = area.width.saturating_sub(2);
    let detail_inline = menu::detail_panel_width(inner_w).is_none();
    let sel = resolve_cursor(&view.rows, &ui.cursor).unwrap_or(0);
    let heights: Vec<usize> = view
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| menu::row_height(r, i == sel, detail_inline))
        .collect();
    let budget = (area.height as usize)
        .saturating_sub(2 + usize::from(shows_tabs(view)) + usize::from(shows_filter(view)))
        .saturating_sub(usize::from(view.status.is_some()));
    ui.scroll = menu::resolve_window(&heights, sel, budget, ui.scroll);
}
