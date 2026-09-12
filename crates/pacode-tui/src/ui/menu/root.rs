//! The hub level (`MenuId::Root`, §4.1): one `Submenu` row per section with a
//! live summary in the right column. `Theme`/`Model` are `Item` rows instead —
//! `enter` swaps the menu for their bottom pickers rather than pushing a
//! level: pickers are bottom chrome, not centred menus.
//!
//! Sections whose target has no provider yet still get a `Submenu` row —
//! `enter` bridges to the legacy overlay until the port lands (TODO T19).

use crate::state::AppState;

use super::model::{Detail, DetailLine, MenuRow, RowId};
use super::stack::MenuStack;
use super::view::{MenuId, MenuUiState, MenuView};

/// The hub view — rebuilt per frame from live state.
pub fn view(state: &AppState, _ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Root);
    view.rows = vec![
        submenu(
            "settings",
            "Settings",
            settings_summary(state),
            MenuId::Settings,
        ),
        submenu(
            "plugins",
            "Plugins",
            plugins_summary(state),
            MenuId::Plugins,
        ),
        submenu("mcp", "MCP", mcp_summary(state), MenuId::Mcp),
        submenu("skills", "Skills", skills_summary(state), MenuId::Skills),
        submenu("keys", "Keybindings", keys_summary(state), MenuId::Keys),
        item(
            "theme",
            "Theme",
            Some(state.config.theme.name.clone()),
            "opens the theme picker",
        ),
        item(
            "model",
            "Model",
            Some(model_summary(state)),
            "opens the model picker",
        ),
        submenu(
            "sessions",
            "Sessions",
            sessions_summary(state),
            MenuId::Sessions,
        ),
        submenu(
            "background",
            "Background",
            background_summary(state),
            MenuId::Background,
        ),
        submenu("git", "Git", git_summary(state), MenuId::Git),
        submenu("import", "Import", None, MenuId::Import),
        submenu("help", "Help", None, MenuId::Help),
    ];
    view
}

/// `enter` on a hub `Item` row — only the bottom pickers carry one today.
/// The caller drops the menu stack because these set `state.focus`.
pub fn on_row(
    state: &mut AppState,
    _stack: &mut MenuStack,
    _ui: &mut MenuUiState,
    row: &RowId,
) -> Vec<crate::keys::Action> {
    match row.as_str() {
        "theme" => crate::commands::open_theme_picker(state),
        "model" => crate::commands::open_model_picker(state),
        _ => vec![],
    }
}

fn submenu(id: &str, label: &str, right: Option<String>, target: MenuId) -> MenuRow {
    MenuRow::Submenu {
        id: RowId::new(id),
        label: label.to_string(),
        right,
        target,
    }
}

fn item(id: &str, label: &str, right: Option<String>, hint: &str) -> MenuRow {
    MenuRow::Item {
        id: RowId::new(id),
        mark: Default::default(),
        label: label.to_string(),
        right,
        detail: Detail {
            lines: vec![DetailLine::Text(hint.to_string())],
            ..Detail::default()
        },
    }
}

/// "68 fields · 3 changed" — action entries (`keys`, `mcp.servers`,
/// `theme.overrides`) are commands, not settings, so they count in neither.
fn settings_summary(state: &AppState) -> Option<String> {
    let entries: Vec<_> = pacode_config::registry::all_settings()
        .iter()
        .filter(|e| !matches!(e.kind, pacode_config::registry::SettingKind::Action { .. }))
        .collect();
    let total = entries.len();
    let changed = entries
        .iter()
        .filter(|e| pacode_config::registry::is_modified(&state.config, e))
        .count();
    if changed == 0 {
        Some(format!("{total} fields"))
    } else {
        Some(format!("{total} fields · {changed} changed"))
    }
}

fn plugins_summary(state: &AppState) -> Option<String> {
    let n = state.plugins.len();
    (n > 0).then(|| format!("{n} loaded"))
}

fn mcp_summary(state: &AppState) -> Option<String> {
    let n = state.config.mcp.servers.len();
    (n > 0).then(|| format!("{n} configured"))
}

fn skills_summary(state: &AppState) -> Option<String> {
    let n: u32 = state.plugins.iter().map(|p| p.skill_dirs).sum();
    (n > 0).then(|| format!("{n} skill dirs"))
}

fn keys_summary(state: &AppState) -> Option<String> {
    let n = state.config.keys.bindings.len();
    (n > 0).then(|| format!("{n} custom"))
}

fn model_summary(state: &AppState) -> String {
    state
        .model()
        .map(|r| r.to_string())
        .unwrap_or_else(|| "default".to_string())
}

fn sessions_summary(state: &AppState) -> Option<String> {
    let n = state.sessions.len();
    (n > 0).then(|| format!("{n} saved"))
}

fn background_summary(state: &AppState) -> Option<String> {
    let n = state.rail.running_task_count();
    (n > 0).then(|| format!("{n} running"))
}

fn git_summary(state: &AppState) -> Option<String> {
    state
        .meta
        .as_ref()
        .and_then(|m| m.git_branch.as_deref())
        .map(str::to_string)
}
