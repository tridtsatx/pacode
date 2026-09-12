//! The `Help` level — the same content the legacy `Overlay::Help` draws, as
//! typed rows (§4.1): key hints as reference rows and the slash commands as
//! runnable `cmd:<name>` items — `enter` runs the command. `FilterMode::Local`
//! narrows both halves by label.

use crate::keys::Action;
use crate::state::AppState;

use super::model::{Detail, MenuRow, RowId};
use super::stack::MenuStack;
use super::view::{FilterMode, MenuId, MenuUiState, MenuView};

/// `(keys, what it does)` — the same list `draw_help` paints for the legacy
/// overlay (TODO T19: that copy dies with the overlay).
const KEY_HINTS: &[(&str, &str)] = &[
    ("alt+↓ / alt+↑", "select agents in rail (or ctrl+j/k)"),
    ("enter", "open panel on agent/task · submit"),
    ("alt+b", "follow agent (auto-scroll)"),
    ("alt+f", "show touched files"),
    ("alt+r", "show plan and agents"),
    (".", "show background tasks (empty prompt)"),
    ("shift+tab", "cycle permission mode"),
    ("ctrl+p", "pick / resume session"),
    ("ctrl+,", "menu hub — or double-tap Shift"),
    ("ctrl+c", "interrupt turn (twice: exit)"),
    ("esc", "peel focus layer"),
];

/// The Help view — a filterable reference list, rebuilt per frame.
pub fn view(_state: &AppState, _ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Help);
    view.filter = Some(FilterMode::Local);
    let mut rows = vec![MenuRow::section("Keys")];
    for (keys, desc) in KEY_HINTS {
        rows.push(MenuRow::Item {
            id: RowId::new(format!("key:{keys}")),
            mark: Default::default(),
            label: (*keys).to_string(),
            right: Some((*desc).to_string()),
            detail: Detail::default(),
        });
    }
    rows.push(MenuRow::section("Commands"));
    for cmd in crate::commands::all_commands() {
        rows.push(MenuRow::Item {
            id: RowId::new(format!("cmd:{}", cmd.name)),
            mark: Default::default(),
            label: cmd.usage.to_string(),
            right: Some(cmd.help.to_string()),
            detail: Detail::default(),
        });
    }
    view.rows = rows;
    view
}

/// `enter` on a `cmd:<name>` row runs the slash command — the menu closes
/// like a palette: the stack drains before `execute` so commands that do not
/// move `focus` still leave the menu.
pub fn on_row(
    state: &mut AppState,
    stack: &mut MenuStack,
    _ui: &mut MenuUiState,
    row: &RowId,
) -> Vec<Action> {
    if let Some(name) = row.as_str().strip_prefix("cmd:") {
        while stack.pop().is_some() {}
        return crate::commands::execute(state, &format!("/{name}"));
    }
    vec![]
}
