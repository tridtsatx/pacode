//! The `Keybindings` level (§5.4): every rebindable [`KeyAction`], grouped by
//! [`Scope`]. `enter` starts the chord capture — `MenuMode::Capture` hides
//! the list and the base forwards every key but `esc` to [`on_capture`].
//! A chord owned by another action parks as `pending` with a status naming
//! the owner; pressing the same chord again takes it over — the owner loses
//! the chord in the keymap and in `keybindings.toml` alike. `ctrl+r`
//! restores the factory binding.
//!
//! Persistence goes through the `keys.*` split file exactly as the legacy
//! picker wrote it (`pacode_config::update_config_value`); a takeover also
//! rewrites the losing action so the steal survives a restart — an empty
//! `keys.<owner> = ""` is the explicit unbind `Keymap::from_config` reads.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::ToastLevel;

use crate::binding::{Action as KeyAction, Binding, Keymap, Scope, format_binding};
use crate::keys::Action;
use crate::state::AppState;

use super::model::{Detail, DetailLine, Mark, MenuRow, RowId};
use super::stack::MenuStack;
use super::view::{
    EmptyState, FilterMode, KeyBinding, MenuAction, MenuId, MenuMode, MenuUiState, MenuView,
    StatusKind, StatusLine,
};

#[cfg(test)]
#[path = "keys_tests.rs"]
mod keys_tests;

/// `ctrl+r` — restore the factory binding (§5.4). One constant feeds the
/// hint edge, the per-row chip and the dispatch check.
const RESET_KEY: KeyBinding = KeyBinding {
    code: KeyCode::Char('r'),
    mods: KeyModifiers::CONTROL,
};

/// The Keys view — rebuilt per frame from the live `state.keymap` (§3.6).
pub fn view(state: &AppState, ui: &MenuUiState) -> MenuView {
    let mut view =
        MenuView::new(MenuId::Keys).with_actions(vec![MenuAction::new(RESET_KEY, "reset")]);
    view.filter = Some(FilterMode::Local);
    // A hook parks its outcome here between frames — the capture conflict
    // warning is the case §5.4 names.
    view.status = ui.status.clone();
    view.empty = EmptyState::new("no bindings match", "clear the filter to list every action");

    let mut rows = Vec::with_capacity(Keymap::action_names().len() + Scope::ALL.len());
    for scope in Scope::ALL {
        let in_scope: Vec<(&'static str, KeyAction)> = Keymap::action_names()
            .iter()
            .filter(|(_, a)| a.scope() == scope)
            .copied()
            .collect();
        if in_scope.is_empty() {
            continue;
        }
        let custom = in_scope
            .iter()
            .filter(|(_, a)| state.keymap.is_overridden(*a))
            .count();
        rows.push(MenuRow::Section {
            label: format!("[{}]", scope.label()),
            right: (custom > 0).then(|| format!("{custom} custom")),
        });
        rows.extend(in_scope.into_iter().map(|(name, a)| row(state, name, a)));
    }
    view.rows = rows;
    view
}

/// One action as a row: the description on the left, the live chord list on
/// the right (`*` once overridden), the action's TOML name as the `RowId`.
fn row(state: &AppState, name: &'static str, action: KeyAction) -> MenuRow {
    let current = state.keymap.bindings_for(action);
    let right = if current.is_empty() {
        "unbound".to_string()
    } else if state.keymap.is_overridden(action) {
        format!("{} *", join_bindings(current))
    } else {
        join_bindings(current)
    };
    MenuRow::Item {
        id: RowId::new(name),
        mark: Mark::None,
        label: action.description().to_string(),
        right: Some(right),
        detail: detail(state, action),
    }
}

/// The detail card of one action (§5.4): the TOML name as the title, then
/// live binding, factory default, scope and description — plus a `⚠` line
/// for every chord a common terminal may claim before we see it.
fn detail(state: &AppState, action: KeyAction) -> Detail {
    let current = state.keymap.bindings_for(action);
    let mut lines = vec![
        DetailLine::Field {
            label: "binding".to_string(),
            value: join_bindings(current),
        },
        DetailLine::Field {
            label: "default".to_string(),
            value: join_bindings(action.default()),
        },
        DetailLine::Field {
            label: "scope".to_string(),
            value: action.scope().label().to_string(),
        },
        DetailLine::Text(action.description().to_string()),
    ];
    for claimed in current
        .iter()
        .filter_map(|b| terminal_claim(b).map(|c| (b, c)))
    {
        let fallback = current
            .iter()
            .find(|o| *o != claimed.0 && terminal_claim(o).is_none())
            .map(format_binding);
        let note = match fallback {
            Some(f) => format!(" — {f} still works"),
            None => " — no fallback bound".to_string(),
        };
        let sign = if state.config.ui.ascii_only {
            "!"
        } else {
            "⚠"
        };
        lines.push(DetailLine::Text(format!(
            "{sign} {} {}{note}",
            format_binding(claimed.0),
            claimed.1
        )));
    }
    Detail {
        title: action.name().to_string(),
        lines,
        // Reset only makes sense on an overridden binding (§5.1 semantics).
        actions: vec![
            MenuAction::new(RESET_KEY, "reset").enabled(state.keymap.is_overridden(action)),
        ],
        // The filter finds an action by its TOML name and its description.
        search: vec![action.name().to_string(), action.description().to_string()],
    }
}

/// `enter` on an action row — start the chord capture. The base forwards
/// every key but `esc` to [`on_capture`] until the mode closes (§5.4).
pub fn on_row(
    _state: &mut AppState,
    _stack: &mut MenuStack,
    ui: &mut MenuUiState,
    row: &RowId,
) -> Vec<Action> {
    let Some(action) = action_by_name(row.as_str()) else {
        return vec![];
    };
    ui.mode = MenuMode::Capture {
        action: action.name(),
        pending: None,
    };
    ui.status = None;
    vec![]
}

/// `ctrl+r` — restore the factory binding: `keys.<action>` leaves
/// `keybindings.toml` and the keymap drops back to defaults.
pub fn on_action(
    state: &mut AppState,
    ui: &mut MenuUiState,
    action: &MenuAction,
    row: Option<&RowId>,
) -> Vec<Action> {
    if action.key != RESET_KEY {
        return vec![];
    }
    let Some(target) = row.and_then(|r| action_by_name(r.as_str())) else {
        return vec![];
    };
    state.keymap.reset_default(target);
    let dotted = format!("keys.{}", target.name());
    if let Err(e) = pacode_config::remove_config_value(&state.paths, &dotted) {
        log::warn!("failed to remove config key '{dotted}': {e}");
        state.push_toast(
            ToastLevel::Error,
            "Failed to update config".to_string(),
            Some(e.to_string()),
            std::time::Instant::now(),
        );
    }
    ui.status = None;
    state.dirty = true;
    vec![]
}

/// A captured chord (§5.4). A clean one binds at once; one owned by another
/// action becomes `pending` with a status naming the owner — the same chord
/// pressed again confirms the takeover: the owner's list loses the chord and
/// `keys.<owner>` is rewritten so the steal survives a restart. `esc` never
/// reaches here — the base leaves the mode first.
pub fn on_capture(
    state: &mut AppState,
    ui: &mut MenuUiState,
    action_name: &'static str,
    key: KeyEvent,
) -> Vec<Action> {
    let Some(target) = action_by_name(action_name) else {
        ui.mode = MenuMode::List;
        return vec![];
    };
    let Some(binding) = capture_binding(key) else {
        // Modifier-only presses and keys `format_binding` cannot spell land
        // here — the capture stays open for a real chord.
        ui.status = Some(StatusLine {
            kind: StatusKind::Warn,
            text: "that key cannot be bound — press another, esc cancels".to_string(),
        });
        return vec![];
    };
    let pending = match &ui.mode {
        MenuMode::Capture { pending, .. } => *pending,
        MenuMode::List | MenuMode::Edit { .. } => None,
    };
    if let Some((owner_name, owner)) = state.keymap.find_conflict(&binding, target) {
        if pending != Some(binding) {
            ui.mode = MenuMode::Capture {
                action: action_name,
                pending: Some(binding),
            };
            ui.status = Some(StatusLine {
                kind: StatusKind::Warn,
                text: format!(
                    "{} is held by {owner_name} — press it again to take it over, esc cancels",
                    format_binding(&binding)
                ),
            });
            return vec![];
        }
        // Second identical press — the confirmed takeover.
        state.keymap.remove_binding(owner, &binding);
        persist(state, owner);
    }
    state.keymap.set_binding(target, binding);
    persist(state, target);
    ui.mode = MenuMode::List;
    ui.status = Some(StatusLine::info(format!(
        "{} bound to {}",
        format_binding(&binding),
        target.name()
    )));
    state.dirty = true;
    vec![]
}

/// Write the action's whole binding list back to `keys.<action>` — several
/// chords join with `,` (what `from_config` splits on), an empty list writes
/// `""` — the explicit unbind. The same split-file write path the legacy
/// picker ran.
fn persist(state: &mut AppState, action: KeyAction) {
    let spec = state
        .keymap
        .bindings_for(action)
        .iter()
        .map(format_binding)
        .collect::<Vec<_>>()
        .join(",");
    let dotted = format!("keys.{}", action.name());
    if let Err(e) = pacode_config::update_config_value(
        &state.paths,
        &dotted,
        pacode_config::toml::Value::String(spec),
    ) {
        log::warn!("failed to update config value '{dotted}': {e}");
        state.push_toast(
            ToastLevel::Error,
            "Failed to update config".to_string(),
            Some(e.to_string()),
            std::time::Instant::now(),
        );
    }
}

/// Turn the raw key event into a `Binding` — the legacy picker's
/// normalization: an uppercase `Char` carries SHIFT itself, `shift+tab` is
/// `BackTab`. `None` when `format_binding` cannot spell the chord
/// (`Media`, `Modifier`, `Null`).
fn capture_binding(key: KeyEvent) -> Option<Binding> {
    let mut mods = key.modifiers
        & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::SUPER);
    let code = match key.code {
        KeyCode::Char(c) if c.is_ascii_uppercase() => {
            mods.insert(KeyModifiers::SHIFT);
            KeyCode::Char(c.to_ascii_lowercase())
        }
        KeyCode::Tab if mods.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
        other => other,
    };
    let binding = Binding { code, mods };
    (!format_binding(&binding).ends_with("unknown")).then_some(binding)
}

/// `None` when nothing claims the chord before the app sees it — otherwise a
/// short reason the detail line quotes verbatim (§5.4 `⚠`, `!` under
/// `ui.ascii_only`).
fn terminal_claim(b: &Binding) -> Option<&'static str> {
    let m = b.mods;
    match b.code {
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right if m == KeyModifiers::ALT => {
            Some("may be claimed by the terminal for word jumps or pane moves")
        }
        KeyCode::Char('c') if m == KeyModifiers::CONTROL => {
            Some("interrupts the foreground process (SIGINT)")
        }
        KeyCode::Char('c') | KeyCode::Char('v')
            if m == KeyModifiers::CONTROL.union(KeyModifiers::SHIFT) =>
        {
            Some("is terminal copy/paste on most emulators")
        }
        KeyCode::Char('t') | KeyCode::Char('w') | KeyCode::Char('n')
            if m == KeyModifiers::CONTROL.union(KeyModifiers::SHIFT) =>
        {
            Some("drives terminal tabs and windows")
        }
        KeyCode::Tab | KeyCode::BackTab if m == KeyModifiers::CONTROL => {
            Some("switches terminal tabs")
        }
        KeyCode::F(11) if m == KeyModifiers::NONE => Some("toggles terminal fullscreen"),
        _ if m.contains(KeyModifiers::SUPER) => Some("is usually claimed by the OS"),
        _ => None,
    }
}

/// The action behind a `RowId`/capture name — `RowId`s are the TOML names.
fn action_by_name(name: &str) -> Option<KeyAction> {
    Keymap::action_names()
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, a)| *a)
}

/// `ctrl+p, ctrl+o` for display; an empty list reads `none`.
fn join_bindings(bindings: &[Binding]) -> String {
    if bindings.is_empty() {
        return "none".to_string();
    }
    bindings
        .iter()
        .map(format_binding)
        .collect::<Vec<_>>()
        .join(", ")
}
