//! The `Settings` level (§5.1): every `pacode_config::registry::SETTINGS`
//! entry grouped by its section. `Bool` entries are `Toggle` rows — `enter`
//! flips them; ints, floats, strings and enums are `Value` rows — `enter`
//! cycles an enum in place, the rest open the inline edit; `Action` entries
//! are `Item` rows whose `enter` runs the command. `ctrl+r` resets a row to
//! its registry default; a value away from default renders `*` in cyan — the
//! marks the legacy settings view drew (TODO T19: that view dies then).

use crossterm::event::{KeyCode, KeyModifiers};
use pacode_config::registry::{self, SettingEntry, SettingKind};
use pacode_config::toml;
use pacode_types::ToastLevel;

use crate::keys::Action;
use crate::state::AppState;

use super::model::{Detail, DetailLine, Mark, MenuRow, RowId, ValueKind};
use super::stack::MenuStack;
use super::view::{
    EmptyState, FilterMode, KeyBinding, MenuAction, MenuId, MenuMode, MenuUiState, MenuView,
    StatusLine,
};

#[cfg(test)]
#[path = "settings_tests.rs"]
mod settings_tests;

/// `ctrl+r` — reset the row to its default (§5.1). One constant feeds the
/// hint edge, the per-row chip and the dispatch check.
const RESET_KEY: KeyBinding = KeyBinding {
    code: KeyCode::Char('r'),
    mods: KeyModifiers::CONTROL,
};

/// The Settings view — rebuilt per frame from the live `state.config` (§3.6).
pub fn view(state: &AppState, ui: &MenuUiState) -> MenuView {
    let mut view =
        MenuView::new(MenuId::Settings).with_actions(vec![MenuAction::new(RESET_KEY, "reset")]);
    view.filter = Some(FilterMode::Local);
    // A hook parks its outcome here between frames — the failed-commit
    // validation error is the case §5.1 names.
    view.status = ui.status.clone();
    view.empty = EmptyState::new("no settings match", "clear the filter to list all settings");

    let mut rows = Vec::with_capacity(registry::SETTINGS.len() + registry::SECTIONS.len());
    for &section in registry::SECTIONS {
        let in_section: Vec<&'static SettingEntry> = registry::SETTINGS
            .iter()
            .filter(|e| e.section == section)
            .collect();
        if in_section.is_empty() {
            continue;
        }
        let changed = in_section
            .iter()
            .filter(|e| is_setting_modified(&state.config, e))
            .count();
        rows.push(MenuRow::Section {
            label: format!("[{section}]"),
            right: (changed > 0).then(|| format!("{changed} changed")),
        });
        rows.extend(in_section.into_iter().map(|e| row(state, e)));
    }
    view.rows = rows;
    view
}

/// One registry entry as a row: the label on the left, the live value (or
/// `on`/`off`) on the right, the dotted key as the `RowId` so hooks find the
/// entry back.
fn row(state: &AppState, entry: &'static SettingEntry) -> MenuRow {
    let current = registry::read_value(&state.config, entry.dotted_key).unwrap_or_default();
    let modified = is_setting_modified(&state.config, entry);
    let detail = detail(state, entry, &current, modified);
    match entry.kind {
        SettingKind::Bool => MenuRow::Toggle {
            id: RowId::new(entry.dotted_key),
            label: entry.label.to_string(),
            on: current == "true",
            modified,
            detail,
        },
        SettingKind::Integer { .. } => value_row(entry, current, ValueKind::Int, modified, detail),
        SettingKind::Float { .. } => value_row(entry, current, ValueKind::Float, modified, detail),
        SettingKind::String => value_row(entry, current, ValueKind::Text, modified, detail),
        SettingKind::Enum { options } => value_row(
            entry,
            current,
            ValueKind::Enum {
                options: options.iter().map(|o| (*o).to_string()).collect(),
            },
            modified,
            detail,
        ),
        SettingKind::Action { .. } => MenuRow::Item {
            id: RowId::new(entry.dotted_key),
            mark: Mark::None,
            label: entry.label.to_string(),
            right: Some(current),
            detail,
        },
    }
}

fn value_row(
    entry: &'static SettingEntry,
    current: String,
    kind: ValueKind,
    modified: bool,
    detail: Detail,
) -> MenuRow {
    MenuRow::Value {
        id: RowId::new(entry.dotted_key),
        label: entry.label.to_string(),
        value: current,
        kind,
        modified,
        detail,
    }
}

/// The detail card of one entry (§5.1): the dotted key, the description, the
/// type with its range or options, the default and live value, where the
/// value came from, and when a change applies.
fn detail(state: &AppState, entry: &'static SettingEntry, current: &str, modified: bool) -> Detail {
    Detail {
        title: entry.label.to_string(),
        lines: vec![
            DetailLine::Field {
                label: "key".to_string(),
                value: entry.dotted_key.to_string(),
            },
            DetailLine::Text(entry.description.to_string()),
            DetailLine::Field {
                label: "type".to_string(),
                value: type_desc(&entry.kind),
            },
            DetailLine::Field {
                label: "default".to_string(),
                value: entry.default_value.to_string(),
            },
            DetailLine::Field {
                label: "current".to_string(),
                value: current.to_string(),
            },
            DetailLine::Field {
                label: "origin".to_string(),
                value: registry::value_origin(&state.config, entry)
                    .as_str()
                    .to_string(),
            },
            DetailLine::Field {
                label: "applies".to_string(),
                value: entry.apply.as_str().to_string(),
            },
        ],
        // Reset only makes sense on an overridden value; the chip (and so the
        // hint) stays hidden otherwise.
        actions: vec![MenuAction::new(RESET_KEY, "reset").enabled(modified)],
        // The filter searches by dotted key and by description (§5.1) through
        // `Detail::search` — terms that never render.
        search: vec![entry.dotted_key.to_string(), entry.description.to_string()],
    }
}

fn type_desc(kind: &SettingKind) -> String {
    match kind {
        SettingKind::Bool => "bool".to_string(),
        SettingKind::Integer { min, max } => format!("integer {min}..={max}"),
        SettingKind::Float { min, max } => format!("float {min}..={max}"),
        SettingKind::String => "string".to_string(),
        SettingKind::Enum { options } => format!("enum: {}", options.join(", ")),
        SettingKind::Action { command } => format!("action: {command}"),
    }
}

/// `enter` on a row: a `Toggle` flips, an enum `Value` cycles to the next
/// option, an action `Item` runs its command. Int/float/text `Value`s open
/// the inline edit inside the base — they never reach this hook.
pub fn on_row(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    row: &RowId,
) -> Vec<Action> {
    let Some(entry) = registry::find_entry(row.as_str()) else {
        return vec![];
    };
    ui.status = None;
    match entry.kind {
        SettingKind::Bool => {
            let current = registry::read_value(&state.config, entry.dotted_key).unwrap_or_default();
            return apply_value(
                state,
                entry.dotted_key,
                toml::Value::Boolean(current != "true"),
            );
        }
        SettingKind::Enum { options } => {
            let current = registry::read_value(&state.config, entry.dotted_key).unwrap_or_default();
            let idx = options.iter().position(|&o| o == current).unwrap_or(0);
            let next = options[(idx + 1) % options.len()];
            return apply_value(
                state,
                entry.dotted_key,
                toml::Value::String(next.to_string()),
            );
        }
        SettingKind::Action { command } => {
            // Palette semantics (same as Help): drain the stack before the
            // command so a command that does not move `focus` still leaves
            // the menu.
            while stack.pop().is_some() {}
            return crate::commands::execute(state, command);
        }
        SettingKind::Integer { .. } | SettingKind::Float { .. } | SettingKind::String => {}
    }
    vec![]
}

/// `ctrl+r` — reset the selected row to its registry default: the key leaves
/// the owning config file and the in-memory value returns to the default.
/// `Action` rows are commands, not values — resetting would wipe the
/// `mcp.servers`/`theme.overrides`/`keys` tables, so the hook refuses them.
pub fn on_action(
    state: &mut AppState,
    ui: &mut MenuUiState,
    action: &MenuAction,
    row: Option<&RowId>,
) -> Vec<Action> {
    if action.key != RESET_KEY {
        return vec![];
    }
    let Some(row) = row else {
        return vec![];
    };
    let Some(entry) = registry::find_entry(row.as_str()) else {
        return vec![];
    };
    if matches!(entry.kind, SettingKind::Action { .. }) {
        return vec![];
    }
    if let Err(e) = pacode_config::remove_config_value(&state.paths, entry.dotted_key) {
        log::warn!("failed to remove config key '{}': {e}", entry.dotted_key);
        state.push_toast(
            ToastLevel::Error,
            "Failed to update config".to_string(),
            Some(e.to_string()),
            std::time::Instant::now(),
        );
    }
    registry::reset_in_config(&mut state.config, entry.dotted_key);
    state.apply_runtime_config(entry.dotted_key);
    ui.status = None;
    if entry.dotted_key == "plugins.marketplace" {
        state.marketplace.clear();
    }
    state.dirty = true;
    runtime_actions(state, entry.dotted_key)
}

/// `enter` in `MenuMode::Edit` on a `Value` row — validate through the
/// registry. A failure keeps the editor open and lands in the status line;
/// the config is not written (§5.1).
pub fn on_edit(
    state: &mut AppState,
    ui: &mut MenuUiState,
    row: &RowId,
    buffer: String,
    cursor: usize,
) -> Vec<Action> {
    let Some(entry) = registry::find_entry(row.as_str()) else {
        return vec![];
    };
    match registry::validate_candidate(entry, &buffer) {
        Ok(value) => {
            let actions = apply_value(state, entry.dotted_key, value);
            ui.status = None;
            return actions;
        }
        Err(err) => {
            ui.mode = MenuMode::Edit { buffer, cursor };
            ui.status = Some(StatusLine::error(err.to_string()));
        }
    }
    vec![]
}

/// Write `key = value` into the owning config file and apply it to the live
/// `state.config`. A failed write toasts; the in-memory apply runs
/// regardless, matching the file the next successful save produces.
fn apply_value(state: &mut AppState, key: &str, value: toml::Value) -> Vec<Action> {
    if let Err(e) = pacode_config::update_config_value(&state.paths, key, value.clone()) {
        log::warn!("failed to write config key '{key}': {e}");
        state.push_toast(
            ToastLevel::Error,
            "Failed to update config".to_string(),
            Some(e.to_string()),
            std::time::Instant::now(),
        );
    }
    registry::apply_to_config(&mut state.config, key, &value);
    state.apply_runtime_config(key);
    if key.starts_with("mcp") {
        state.invalidate_cache(crate::cache::ListKind::McpServers);
        state.invalidate_cache(crate::cache::ListKind::Commands);
    }
    if key.starts_with("plugins.") {
        state.invalidate_cache(crate::cache::ListKind::Plugins);
        state.invalidate_cache(crate::cache::ListKind::Marketplace);
        state.invalidate_cache(crate::cache::ListKind::Commands);
    }
    if key == "plugins.marketplace" {
        state.marketplace.clear();
    }
    state.dirty = true;
    runtime_actions(state, key)
}

fn runtime_actions(state: &AppState, key: &str) -> Vec<Action> {
    if key != "plugins.marketplace" {
        return vec![];
    }
    let source = state.config.plugins.marketplace.clone();
    if source.is_empty() {
        return vec![];
    }
    vec![Action::Send(pacode_types::Request::BrowseMarketplace {
        source,
        query: String::new(),
    })]
}

/// `is_modified` minus the `Action` entries: those are commands, not values,
/// so their read-back ("3 servers (/mcp)") never equals the default and would
/// count as permanently changed.
fn is_setting_modified(config: &pacode_types::Config, entry: &SettingEntry) -> bool {
    !matches!(entry.kind, SettingKind::Action { .. }) && registry::is_modified(config, entry)
}
