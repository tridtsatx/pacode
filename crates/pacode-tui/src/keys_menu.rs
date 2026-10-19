//! Key handling for the menu base (§3.4). The base owns navigation, tabs, the
//! filter line, `enter` by row kind, `esc`/`ctrl+c` stack control and `?`;
//! what a submenu does with a row, a declared action or an edit lives in its
//! `ui/menu/<name>.rs` behind `on_row` / `on_action` / `on_edit` — this file
//! is the dispatch table between the base keys and those hooks.
//!
//! Entries: [`open`] — a direct route (`/plugins`, `ctrl+,`, double-Shift) —
//! and [`handle_menu_key`], which `keys::handle_key` routes every key to while
//! `Focus::Overlay(Overlay::Menu)` is up. Every `MenuId` has a provider, so
//! the base owns all central submenus end to end.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::binding::matches_key;
use crate::keys::Action;
use crate::state::{AppState, Focus, Overlay};
use crate::ui::menu::{
    self, MenuAction, MenuId, MenuLevel, MenuMode, MenuRow, MenuStack, MenuUiState, RowId,
    ValueKind, resolve_cursor, selectable, shows_filter, shows_tabs,
};

mod remote;

use remote::{arm_remote, flush_remote, on_open, tab_follow_up};
pub(crate) use remote::{remote_deadline, remote_filter_due};

#[cfg(test)]
#[path = "keys_menu_tests.rs"]
mod keys_menu_tests;

/// Chars the filter query holds — it is a matcher, not an editor. Same cap as
/// the paste path (`paste::FILTER_MAX_CHARS`) so typing and pasting agree.
const QUERY_MAX_CHARS: usize = crate::paste::FILTER_MAX_CHARS;

/// Open a menu directly — `/plugins`, `.`, `ctrl+,`, double-Shift, `?`.
/// A menu becomes a one-level stack (§3.5).
pub fn open(state: &mut AppState, id: MenuId) -> Vec<Action> {
    log::debug!("open menu: {}", id.slug());
    state.focus = Focus::Overlay(Overlay::Menu {
        stack: MenuStack::root(id),
        ui: MenuUiState::default(),
    });
    state.dirty = true;
    on_open(state, id)
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
        MenuMode::Capture { action, .. } => capture_key(state, ui, view.id, action, key),
        MenuMode::List => list_key(state, stack, ui, &view, key),
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
        // `esc` and `←` — one level back; the last level pops into "closed"
        // (§3.4). Edit mode keeps `←` for its own cursor — this arm is List only.
        KeyCode::Esc | KeyCode::Left => {
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
    // filter line owns the printable keys (§3.4). An enabled declared action
    // on the same bare char outranks the alias — the menu's contract wins
    // (Background's `k` is "kill", not "up") — while arrows and ctrl+p/ctrl+n
    // stay movement everywhere.
    let up = key.code == KeyCode::Up
        || (key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL))
        || (!filtering && key.code == KeyCode::Char('k') && bare);
    let down = key.code == KeyCode::Down
        || (key.code == KeyCode::Char('n') && key.modifiers.contains(KeyModifiers::CONTROL))
        || (!filtering && key.code == KeyCode::Char('j') && bare);
    if up || down {
        if bare && matches!(key.code, KeyCode::Char(_)) && declared_action(view, ui, key).is_some()
        {
            return run_action(state, stack, ui, view, key);
        }
        move_sel(state, ui, view, if up { -1 } else { 1 });
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
            tab_follow_up(state, stack, ui)
        }
        KeyCode::BackTab | KeyCode::Tab => {
            tab_step(ui, view, -1);
            tab_follow_up(state, stack, ui)
        }
        // `→` activates the row like `enter` — descend into a submenu, flip
        // an enum, open the row's target (§3.4).
        KeyCode::Enter | KeyCode::Right => enter_row(state, stack, ui, view),
        KeyCode::Backspace if filtering => {
            let changed = ui.query.pop().is_some();
            ui.cursor = RowId::default();
            arm_remote(ui, view, changed);
            vec![]
        }
        KeyCode::Char(c) if bare => {
            if filtering {
                let changed = ui.query.chars().count() < QUERY_MAX_CHARS;
                if changed {
                    ui.query.push(c);
                }
                ui.cursor = RowId::default();
                arm_remote(ui, view, changed);
                return vec![];
            }
            run_action(state, stack, ui, view, key)
        }
        _ => run_action(state, stack, ui, view, key),
    }
}

/// `enter` on a Submenu row or `?`: save the level's UI state and push the
/// target (§3.5). `pub(crate)`: the Help palette jumps the same
/// way (§5.8) and then points the fresh `ui` at a filter or a row.
pub(crate) fn descend(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    target: MenuId,
) -> Vec<Action> {
    // A remote query still inside its debounce window would die with the
    // saved level's `ui` — send it now rather than lose the edit.
    let mut actions = flush_remote(state, stack, ui);
    if let Some(level) = stack.current_mut() {
        level.save(ui);
    }
    if stack.push(MenuLevel::new(target)) {
        *ui = MenuUiState::default();
    }
    actions.extend(on_open(state, target));
    actions
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
        MenuRow::Item { id, .. }
        | MenuRow::Toggle { id, .. }
        | MenuRow::Check { id, .. }
        | MenuRow::Rich { id, .. } => row_enter(state, stack, ui, view.id, id),
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
        MenuId::Plugins => menu::plugins::on_row(state, stack, ui, row),
        MenuId::Mcp => menu::mcp::on_row(state, stack, ui, row),
        MenuId::Settings => menu::settings::on_row(state, stack, ui, row),
        MenuId::Keys => menu::keys::on_row(state, stack, ui, row),
        MenuId::Sessions => menu::sessions::on_row(state, stack, ui, row),
        MenuId::Files => menu::files::on_row(state, stack, ui, row),
        MenuId::Background => menu::background::on_row(state, stack, ui, row),
        // `enter` here is the apply — the marked set, not the row (§5.7).
        MenuId::Import => menu::import::on_row(state, stack, ui, row),
        MenuId::Git => menu::git::on_row(state, stack, ui, row),
        MenuId::GitBranches => menu::git_branches::on_row(state, stack, ui, row),
        MenuId::Skills => vec![],
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
    let row_id = resolve_cursor(&view.rows, &ui.cursor).and_then(|i| view.rows[i].id().cloned());
    match declared_action(view, ui, key) {
        Some(action) => menu_action(state, stack, ui, view.id, action, row_id.as_ref()),
        None => vec![],
    }
}

/// The enabled declared action `key` hits — menu-level `view.actions` first,
/// then the selected row's chips (§3.4). Shared by `run_action` and the
/// j/k-alias gate in `list_key_inner`.
fn declared_action<'a>(
    view: &'a menu::MenuView,
    ui: &MenuUiState,
    key: KeyEvent,
) -> Option<&'a MenuAction> {
    let sel = resolve_cursor(&view.rows, &ui.cursor);
    let row_actions = sel
        .and_then(|i| view.rows[i].detail())
        .map(|d| d.actions.as_slice())
        .unwrap_or(&[]);
    view.actions
        .iter()
        .chain(row_actions)
        .find(|a| a.enabled && matches_key(&a.key, key))
}

/// Declared-action dispatch (§3.4): one arm per menu, filled by its port —
/// `ui/menu/<name>::on_action` decides what its `MenuAction`s do.
fn menu_action(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    menu: MenuId,
    action: &MenuAction,
    row: Option<&RowId>,
) -> Vec<Action> {
    match menu {
        MenuId::Plugins => menu::plugins::on_action(state, ui, action, row),
        MenuId::Mcp => menu::mcp::on_action(state, ui, action, row),
        MenuId::Settings => menu::settings::on_action(state, ui, action, row),
        MenuId::Keys => menu::keys::on_action(state, ui, action, row),
        MenuId::Files => menu::files::on_action(state, ui, action, row),
        MenuId::Background => menu::background::on_action(state, ui, action, row),
        MenuId::Import => menu::import::on_action(state, ui, action, row),
        // `ctrl+b` pushes a level — the stack is this hook's business (§5.10).
        MenuId::Git => menu::git::on_action(state, stack, ui, action, row),
        MenuId::Root | MenuId::Help | MenuId::Sessions | MenuId::Skills | MenuId::GitBranches => {
            vec![]
        }
    }
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
            // A parked status (a failed commit's validation error) dies with
            // the edit that caused it.
            ui.status = None;
            vec![]
        }
        KeyCode::Enter => {
            let MenuMode::Edit { buffer, cursor } = std::mem::replace(&mut ui.mode, MenuMode::List)
            else {
                return vec![];
            };
            let row =
                resolve_cursor(&view.rows, &ui.cursor).and_then(|i| view.rows[i].id().cloned());
            match row {
                Some(id) => edit_commit(state, stack, ui, view.id, &id, buffer, cursor),
                None => vec![],
            }
        }
        _ => {
            edit_buffer(&mut ui.mode, key);
            // Editing on retires the previous validation error (§5.1).
            ui.status = None;
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
/// (§3.4). `cursor` is handed through so a menu whose commit fails can put
/// the editor back exactly as it was.
fn edit_commit(
    state: &mut AppState,
    _stack: &mut MenuStack,
    ui: &mut MenuUiState,
    menu: MenuId,
    row: &RowId,
    buffer: String,
    cursor: usize,
) -> Vec<Action> {
    match menu {
        MenuId::Settings => menu::settings::on_edit(state, ui, row, buffer, cursor),
        MenuId::Root
        | MenuId::Help
        | MenuId::Plugins
        | MenuId::Mcp
        | MenuId::Keys
        | MenuId::Files
        | MenuId::Sessions
        | MenuId::Import
        | MenuId::Background
        | MenuId::Skills
        | MenuId::Git
        | MenuId::GitBranches => vec![],
    }
}

/// Key capture (§5.4): `esc` backs out to the list (and retires a parked
/// conflict status); every other key is the capturing menu's business — Keys
/// is the only menu that enters this mode.
fn capture_key(
    state: &mut AppState,
    ui: &mut MenuUiState,
    menu: MenuId,
    action: &'static str,
    key: KeyEvent,
) -> Vec<Action> {
    if key.code == KeyCode::Esc {
        ui.mode = MenuMode::List;
        ui.status = None;
        return vec![];
    }
    match menu {
        MenuId::Keys => menu::keys::on_capture(state, ui, action, key),
        // No other menu enters Capture — the miss arm is defensive plumbing.
        MenuId::Root
        | MenuId::Settings
        | MenuId::Plugins
        | MenuId::Mcp
        | MenuId::Files
        | MenuId::Sessions
        | MenuId::Import
        | MenuId::Help
        | MenuId::Background
        | MenuId::Skills
        | MenuId::Git
        | MenuId::GitBranches => vec![],
    }
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
    let area = menu_area(state, view, ui);
    (area.height as usize)
        .saturating_sub(2 + usize::from(shows_tabs(view)) + usize::from(shows_filter(view)))
        .max(1)
}

/// Keep `ui.scroll` a valid window anchor for the cursor (§3.5): the window
/// start a level remembers survives the descent and the return.
fn sync_scroll(state: &AppState, ui: &mut MenuUiState, view: &menu::MenuView) {
    let area = menu_area(state, view, ui);
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

fn menu_area(state: &AppState, view: &menu::MenuView, ui: &MenuUiState) -> Rect {
    menu::overlay_area(
        crate::ui::popup::overlay_container(&crate::layout::current(state)),
        view,
        ui,
    )
}
