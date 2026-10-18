//! Keys and actions of the `/model` picker: moving through providers, kinds and
//! rows, typing the search, and assigning the highlighted model to a role.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use pacode_config::toml::Value;
use pacode_types::{Effort, ModelInfo, ModelRoute, Request, ToastLevel};

use crate::keys::Action;
use crate::state::model_picker::{PickerRole, SIDE_ALL, SIDE_ROLES};
use crate::state::{AppState, Focus, Overlay};

/// Rows a PageUp / PageDown moves.
const PAGE: isize = 10;

/// Open the picker on `All models` with the current model highlighted.
pub fn open(state: &mut AppState) -> Vec<Action> {
    let recent = pacode_config::load_prefs(&state.paths).recent_models;
    let mp = &mut state.model_picker;
    mp.set_recent(recent);
    mp.query.clear();
    mp.kind = None;
    mp.role = PickerRole::Main;
    mp.side = SIDE_ALL;
    state.focus = Focus::Overlay(Overlay::ModelPicker {
        query: String::new(),
        index: 0,
    });
    state.refresh_model_picker();
    state.dirty = true;
    vec![
        Action::Send(Request::ListModels),
        Action::Send(Request::ListAuth),
    ]
}

pub fn handle_key(state: &mut AppState, key: KeyEvent) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    state.dirty = true;
    match key.code {
        KeyCode::Esc => state.focus = Focus::Normal,
        KeyCode::Enter => return enter(state),
        KeyCode::Up => state.model_picker.move_side(-1, &state.models),
        KeyCode::Down => state.model_picker.move_side(1, &state.models),
        KeyCode::Left if alt => state.model_picker.cycle_kind(-1, &state.models),
        KeyCode::Right if alt => state.model_picker.cycle_kind(1, &state.models),
        KeyCode::Tab => state.model_picker.move_cursor(1),
        KeyCode::BackTab => state.model_picker.move_cursor(-1),
        KeyCode::PageDown => state.model_picker.move_cursor(PAGE),
        KeyCode::PageUp => state.model_picker.move_cursor(-PAGE),
        KeyCode::Home => state.model_picker.cursor = 0,
        KeyCode::End => {
            state.model_picker.cursor = state.model_picker.row_count().saturating_sub(1);
        }
        KeyCode::Char('n') if ctrl => state.model_picker.move_cursor(1),
        KeyCode::Char('p') if ctrl => state.model_picker.move_cursor(-1),
        KeyCode::Char('r') if ctrl || (!alt && state.model_picker.query.is_empty()) => {
            state.model_picker.role = state.model_picker.role.next();
        }
        KeyCode::Backspace => state.model_picker.pop_query(&state.models),
        KeyCode::Char(c) if !ctrl && !alt => state.model_picker.push_query(c, &state.models),
        _ => {}
    }
    vec![]
}

/// Bracketed paste into the search box.
pub fn paste(state: &mut AppState, text: &str) {
    let one_line = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    state.model_picker.append_query(&one_line, &state.models);
    state.dirty = true;
}

fn enter(state: &mut AppState) -> Vec<Action> {
    if state.model_picker.side == SIDE_ROLES {
        // Choose the role, then pick its model from the full list.
        let role = PickerRole::ALL
            .get(state.model_picker.cursor)
            .copied()
            .unwrap_or_default();
        state.model_picker.role = role;
        state.model_picker.side = SIDE_ALL;
        state.model_picker.recompute(&state.models);
        if let Some(target) = state.role_target(role) {
            state.model_picker.select_route(&target, &state.models);
        }
        return vec![];
    }
    let Some(model) = state.model_picker.highlighted(&state.models).cloned() else {
        return vec![];
    };
    let role = state.model_picker.role;
    assign(state, role, &model.route)
}

/// Assign `route` to `role`. The main role is `/model`; the others write their
/// config key and keep the picker open so the role strip shows the result.
pub fn assign(state: &mut AppState, role: PickerRole, route: &ModelRoute) -> Vec<Action> {
    let info = state.models.iter().find(|m| m.route == *route).cloned();
    let now = Instant::now();
    match role {
        PickerRole::Main => {
            state.model_picker.touch_recent(route);
            save_recent(state);
            state.focus = Focus::Normal;
            set_main_model(state, route.clone(), info.as_ref())
        }
        PickerRole::Agents => {
            write_config(state, "agents.default_model", route.to_string(), now);
            if let (Some(cur), Some(info)) = (state.config.agents.default_effort, info.as_ref())
                && let Some(clamped) = info.clamp_effort(cur)
                && clamped != cur
            {
                write_config(
                    state,
                    "agents.default_effort",
                    clamped.as_str().to_string(),
                    now,
                );
                toast(state, effort_clamp_message(clamped, info), now);
            }
            toast(state, format!("agents use {route}"), now);
            vec![]
        }
        PickerRole::Compaction => {
            write_config(state, "context.compaction_model", route.to_string(), now);
            toast(state, format!("compaction uses {route}"), now);
            vec![]
        }
    }
}

/// `/model` semantics: remember the choice, switch the session, and move the
/// effort onto a level the new model supports.
pub fn set_main_model(
    state: &mut AppState,
    route: ModelRoute,
    info: Option<&ModelInfo>,
) -> Vec<Action> {
    state.save_pref_model(&route.to_string());
    let mut actions = vec![Action::Send(Request::SetModel(route))];
    let current = state.effort();
    if let Some(info) = info
        && let Some(clamped) = info.clamp_effort(current)
        && clamped != current
    {
        state.save_pref_effort(clamped);
        actions.push(Action::Send(Request::SetEffort(clamped)));
        toast(state, effort_clamp_message(clamped, info), Instant::now());
    }
    actions
}

/// Set the session effort, moved onto a level the current model supports.
pub fn set_effort(state: &mut AppState, wanted: Effort) -> Vec<Action> {
    let info = state.model_info().cloned();
    let effort = info
        .as_ref()
        .and_then(|i| i.clamp_effort(wanted))
        .unwrap_or(wanted);
    if let (true, Some(i)) = (effort != wanted, info.as_ref()) {
        toast(state, effort_clamp_message(effort, i), Instant::now());
    }
    state.save_pref_effort(effort);
    vec![Action::Send(Request::SetEffort(effort))]
}

/// `effort set to high (model supports low, medium, high)`.
pub fn effort_clamp_message(effort: Effort, info: &ModelInfo) -> String {
    let levels: Vec<&str> = info.efforts.iter().map(|e| e.as_str()).collect();
    format!(
        "effort set to {effort} (model supports {})",
        levels.join(", ")
    )
}

fn toast(state: &mut AppState, title: String, now: Instant) {
    state.push_toast(ToastLevel::Info, title, None, now);
}

fn save_recent(state: &AppState) {
    let mut prefs = pacode_config::load_prefs(&state.paths);
    prefs.recent_models = state.model_picker.recent.clone();
    if let Err(e) = pacode_config::save_prefs(&state.paths, &prefs) {
        log::warn!("failed to save prefs (recent models): {e}");
    }
}

fn write_config(state: &mut AppState, key: &str, value: String, now: Instant) {
    let val = Value::String(value);
    if let Err(e) = pacode_config::update_config_value(&state.paths, key, val.clone()) {
        log::warn!("failed to write config key '{key}': {e}");
        state.push_toast(
            ToastLevel::Error,
            "Failed to update config".to_string(),
            Some(e.to_string()),
            now,
        );
    }
    pacode_config::registry::apply_to_config(&mut state.config, key, &val);
}
