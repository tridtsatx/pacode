//! Keyboard handling for overlays and pickers.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use pacode_types::Request;

use crate::keys::Action;
use crate::state::{AppState, Focus, Overlay};

fn is_prev(key: &KeyEvent) -> bool {
    key.code == KeyCode::Up
        || (key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL))
}

fn is_next(key: &KeyEvent) -> bool {
    key.code == KeyCode::Down
        || (key.code == KeyCode::Char('n') && key.modifiers.contains(KeyModifiers::CONTROL))
}

fn is_overlay_up(key: &KeyEvent) -> bool {
    is_prev(key) || key.code == KeyCode::Char('k')
}

fn is_overlay_down(key: &KeyEvent) -> bool {
    is_next(key) || key.code == KeyCode::Char('j')
}

pub fn handle_picker_key(state: &mut AppState, key: KeyEvent) -> Vec<Action> {
    match &mut state.focus {
        Focus::Overlay(Overlay::EffortPicker { index }) => {
            match key.code {
                KeyCode::Esc => state.focus = Focus::Normal,
                KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => {
                    *index = index.saturating_sub(1);
                }
                KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') => {
                    *index = (*index + 1).min(pacode_types::Effort::ALL.len() - 1);
                }
                KeyCode::Enter => {
                    let effort = pacode_types::Effort::ALL[*index];
                    state.focus = Focus::Normal;
                    state.save_pref_effort(effort);
                    return vec![Action::Send(Request::SetEffort(effort))];
                }
                _ => {}
            }
            vec![]
        }
        Focus::Overlay(Overlay::ModePicker { index }) => {
            match key.code {
                KeyCode::Esc => state.focus = Focus::Normal,
                KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => {
                    *index = index.saturating_sub(1);
                }
                KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') => {
                    *index = (*index + 1).min(3);
                }
                KeyCode::Enter => {
                    let mode = pacode_types::Mode::CYCLE[*index];
                    state.focus = Focus::Normal;
                    state.save_pref_mode(mode);
                    return vec![Action::Send(Request::SetMode(mode))];
                }
                _ => {}
            }
            vec![]
        }
        Focus::Overlay(Overlay::ModelPicker { query, index }) => {
            let q_lower = query.to_lowercase();
            let filtered_count = state
                .models
                .iter()
                .filter(|m| {
                    q_lower.is_empty()
                        || m.route.to_string().to_lowercase().contains(&q_lower)
                        || m.display_name.to_lowercase().contains(&q_lower)
                })
                .count();

            if is_prev(&key) && *index > 0 {
                *index -= 1;
                return vec![];
            }
            if is_next(&key) && filtered_count > 0 && *index + 1 < filtered_count {
                *index += 1;
                return vec![];
            }

            match key.code {
                KeyCode::Esc => state.focus = Focus::Normal,
                KeyCode::Enter => {
                    let filtered: Vec<_> = state
                        .models
                        .iter()
                        .filter(|m| {
                            q_lower.is_empty()
                                || m.route.to_string().to_lowercase().contains(&q_lower)
                                || m.display_name.to_lowercase().contains(&q_lower)
                        })
                        .collect();
                    if let Some(m) = filtered.get(*index) {
                        let route = m.route.clone();
                        state.focus = Focus::Normal;
                        state.save_pref_model(&route.to_string());
                        return vec![Action::Send(Request::SetModel(route))];
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    *index = 0;
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    query.push(c);
                    *index = 0;
                }
                _ => {}
            }
            vec![]
        }
        Focus::Overlay(Overlay::SessionPicker { query, index }) => {
            let q_lower = query.to_lowercase();
            let filtered_count = state
                .sessions
                .iter()
                .filter(|s| {
                    q_lower.is_empty()
                        || s.title().to_lowercase().contains(&q_lower)
                        || s.id.as_str().to_lowercase().contains(&q_lower)
                })
                .count();

            if is_prev(&key) && *index > 0 {
                *index -= 1;
                return vec![];
            }
            if is_next(&key) && filtered_count > 0 && *index + 1 < filtered_count {
                *index += 1;
                return vec![];
            }

            match key.code {
                KeyCode::Esc => state.focus = Focus::Normal,
                KeyCode::Enter => {
                    let filtered: Vec<_> = state
                        .sessions
                        .iter()
                        .filter(|s| {
                            q_lower.is_empty()
                                || s.title().to_lowercase().contains(&q_lower)
                                || s.id.as_str().to_lowercase().contains(&q_lower)
                        })
                        .collect();
                    if let Some(session) = filtered.get(*index) {
                        let id = session.id.clone();
                        state.focus = Focus::Normal;
                        return vec![Action::Send(Request::Attach(
                            pacode_types::Attach::Resume { session: id },
                        ))];
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    *index = 0;
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    query.push(c);
                    *index = 0;
                }
                _ => {}
            }
            vec![]
        }
        Focus::Overlay(Overlay::Files { index }) => {
            let count = state.files.len();
            if is_overlay_up(&key) && *index > 0 {
                *index -= 1;
                return vec![];
            }
            if is_overlay_down(&key) && count > 0 && *index + 1 < count {
                *index += 1;
                return vec![];
            }
            match key.code {
                KeyCode::Esc => state.focus = Focus::Normal,
                KeyCode::Enter => {
                    let sorted = state.files.sorted_rows();
                    if let Some(row) = sorted.get(*index) {
                        state.input.insert_str(&row.path);
                    }
                    state.focus = Focus::Normal;
                }
                _ => {}
            }
            vec![]
        }
        Focus::Overlay(Overlay::RailOverlay) | Focus::Overlay(Overlay::Help) => {
            if key.code == KeyCode::Esc {
                state.focus = Focus::Normal;
            }
            vec![]
        }
        Focus::Overlay(Overlay::Import(_)) => vec![],
        Focus::Overlay(Overlay::McpPicker {
            index,
            servers,
            loading,
        }) => {
            let count = servers.len();
            if is_overlay_up(&key) && *index > 0 {
                *index -= 1;
                return vec![];
            }
            if is_overlay_down(&key) && count > 0 && *index + 1 < count {
                *index += 1;
                return vec![];
            }
            match key.code {
                KeyCode::Esc => state.focus = Focus::Normal,
                KeyCode::Char('r') => {
                    if let Some(s) = servers.get(*index) {
                        let server_name = s.name.clone();
                        *loading = true;
                        return vec![
                            Action::Send(Request::RestartMcpServer {
                                server: server_name,
                            }),
                            Action::Send(Request::ListMcpServers),
                        ];
                    }
                }
                KeyCode::Enter => {
                    if let Some(s) = servers.get(*index) {
                        let server_name = s.name.clone();
                        let enabled = s.status == "disabled";
                        *loading = true;
                        return vec![
                            Action::Send(Request::SetMcpServerEnabled {
                                server: server_name,
                                enabled,
                            }),
                            Action::Send(Request::ListMcpServers),
                        ];
                    }
                }
                _ => {}
            }
            vec![]
        }
        Focus::Overlay(Overlay::PluginsPicker { index, plugins }) => {
            let count = plugins.len();
            if is_overlay_up(&key) && *index > 0 {
                *index -= 1;
                return vec![];
            }
            if is_overlay_down(&key) && count > 0 && *index + 1 < count {
                *index += 1;
                return vec![];
            }
            if key.code == KeyCode::Esc {
                state.focus = Focus::Normal;
            }
            vec![]
        }
        Focus::Overlay(Overlay::KeysPicker { index, capturing }) => {
            let count = crate::binding::Keymap::action_names().len();
            if *capturing {
                if key.code == KeyCode::Esc {
                    *capturing = false;
                    return vec![];
                }

                let mut mods = key.modifiers
                    & (KeyModifiers::CONTROL
                        | KeyModifiers::ALT
                        | KeyModifiers::SHIFT
                        | KeyModifiers::SUPER);
                let code = match key.code {
                    KeyCode::Char(c) if c.is_ascii_uppercase() => {
                        mods.insert(KeyModifiers::SHIFT);
                        KeyCode::Char(c.to_ascii_lowercase())
                    }
                    KeyCode::Tab if mods.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
                    other => other,
                };
                let binding = crate::binding::Binding { code, mods };
                let formatted = crate::binding::format_binding(&binding);
                if formatted.ends_with("unknown") {
                    *capturing = false;
                    state.push_toast(
                        pacode_types::ToastLevel::Warn,
                        "Cannot bind key".to_string(),
                        Some("Key cannot be represented".to_string()),
                        std::time::Instant::now(),
                    );
                    return vec![];
                }

                let Some(&(_, action)) = crate::binding::Keymap::action_names().get(*index) else {
                    *capturing = false;
                    return vec![];
                };

                if let Some((_, conflict_action)) = state.keymap.find_conflict(&binding, action) {
                    *capturing = false;
                    state.push_toast(
                        pacode_types::ToastLevel::Warn,
                        "Keybinding conflict".to_string(),
                        Some(conflict_action.description().to_string()),
                        std::time::Instant::now(),
                    );
                    return vec![];
                }

                state.keymap.set_binding(action, binding);
                *capturing = false;

                let act_name = action.name();
                let dotted = format!("keys.{act_name}");
                if let Err(err) = pacode_config::update_config_value(
                    &state.paths,
                    &dotted,
                    pacode_config::toml::Value::String(formatted),
                ) {
                    state.push_toast(
                        pacode_types::ToastLevel::Error,
                        "Failed to update config".to_string(),
                        Some(err.to_string()),
                        std::time::Instant::now(),
                    );
                }
            } else {
                if is_overlay_up(&key) && *index > 0 {
                    *index -= 1;
                    return vec![];
                }
                if is_overlay_down(&key) && count > 0 && *index + 1 < count {
                    *index += 1;
                    return vec![];
                }
                match key.code {
                    KeyCode::Esc => state.focus = Focus::Normal,
                    KeyCode::Enter => {
                        *capturing = true;
                    }
                    KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(&(_, action)) =
                            crate::binding::Keymap::action_names().get(*index)
                        {
                            state.keymap.reset_default(action);
                            let act_name = action.name();
                            let dotted = format!("keys.{act_name}");
                            if let Err(err) =
                                pacode_config::remove_config_value(&state.paths, &dotted)
                            {
                                state.push_toast(
                                    pacode_types::ToastLevel::Error,
                                    "Failed to update config".to_string(),
                                    Some(err.to_string()),
                                    std::time::Instant::now(),
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            vec![]
        }
        Focus::Overlay(Overlay::ThemePicker {
            index,
            original_theme,
            original_name,
            step,
            user_themes,
        }) => {
            let truecolor = match state.config.ui.color.as_str() {
                "ansi" => false,
                "truecolor" | "24bit" => true,
                _ => pacode_render::detect_truecolor(),
            };
            let builtins = pacode_render::builtin_palettes();

            match step {
                crate::state::ThemePickerStep::SelectTheme => {
                    let total_rows = builtins.len() + user_themes.len() + 1; // +1 for "Create new one…"

                    let mut moved = false;
                    if is_overlay_up(&key) && *index > 0 {
                        *index -= 1;
                        moved = true;
                    }
                    if is_overlay_down(&key) && *index + 1 < total_rows {
                        *index += 1;
                        moved = true;
                    }

                    if moved {
                        if *index < builtins.len() {
                            let p = &builtins[*index];
                            let theme_cfg = pacode_types::ThemeConfig {
                                name: p.name.clone(),
                                overrides: state.config.theme.overrides.clone(),
                            };
                            let (pal, _) = pacode_config::load_theme(&state.paths, &theme_cfg);
                            state.theme = pacode_render::Theme::from_palette(&pal, truecolor);
                        } else if *index < builtins.len() + user_themes.len() {
                            let u_name = &user_themes[*index - builtins.len()];
                            let theme_cfg = pacode_types::ThemeConfig {
                                name: u_name.clone(),
                                overrides: state.config.theme.overrides.clone(),
                            };
                            let (pal, _) = pacode_config::load_theme(&state.paths, &theme_cfg);
                            state.theme = pacode_render::Theme::from_palette(&pal, truecolor);
                        } else {
                            state.theme = (**original_theme).clone();
                        }
                        return vec![];
                    }

                    match key.code {
                        KeyCode::Esc => {
                            state.theme = (**original_theme).clone();
                            state.config.theme.name = original_name.clone();
                            state.focus = Focus::Normal;
                        }
                        KeyCode::Enter => {
                            if *index < builtins.len() {
                                let name = builtins[*index].name.clone();
                                state.config.theme.name = name.clone();
                                let _ = pacode_config::update_config_value(
                                    &state.paths,
                                    "theme.name",
                                    pacode_config::toml::Value::String(name),
                                );
                                state.focus = Focus::Normal;
                            } else if *index < builtins.len() + user_themes.len() {
                                let name = user_themes[*index - builtins.len()].clone();
                                state.config.theme.name = name.clone();
                                let _ = pacode_config::update_config_value(
                                    &state.paths,
                                    "theme.name",
                                    pacode_config::toml::Value::String(name),
                                );
                                state.focus = Focus::Normal;
                            } else {
                                *step = crate::state::ThemePickerStep::SelectBase;
                                *index = 0;
                                let p = &builtins[0];
                                state.theme = pacode_render::Theme::from_palette(p, truecolor);
                            }
                        }
                        _ => {}
                    }
                }
                crate::state::ThemePickerStep::SelectBase => {
                    let total_rows = builtins.len();

                    let mut moved = false;
                    if is_overlay_up(&key) && *index > 0 {
                        *index -= 1;
                        moved = true;
                    }
                    if is_overlay_down(&key) && *index + 1 < total_rows {
                        *index += 1;
                        moved = true;
                    }

                    if moved {
                        let p = &builtins[*index];
                        state.theme = pacode_render::Theme::from_palette(p, truecolor);
                        return vec![];
                    }

                    match key.code {
                        KeyCode::Esc => {
                            *step = crate::state::ThemePickerStep::SelectTheme;
                            *index = 0;
                            state.theme = (**original_theme).clone();
                        }
                        KeyCode::Enter => {
                            let base_palette = &builtins[*index % builtins.len()];
                            let custom_name =
                                pacode_config::theme::next_custom_theme_name(&state.paths);
                            let mut new_palette = base_palette.clone();
                            new_palette.name = custom_name.clone();

                            match pacode_config::write_theme(&state.paths, &new_palette) {
                                Ok(path) => {
                                    state.config.theme.name = custom_name.clone();
                                    let _ = pacode_config::update_config_value(
                                        &state.paths,
                                        "theme.name",
                                        pacode_config::toml::Value::String(custom_name.clone()),
                                    );
                                    state.theme =
                                        pacode_render::Theme::from_palette(&new_palette, truecolor);
                                    state.focus = Focus::Normal;

                                    let path_str = path.display().to_string();
                                    state.push_toast(
                                        pacode_types::ToastLevel::Success,
                                        format!("Theme '{custom_name}' created"),
                                        Some(format!("Edit {path_str}")),
                                        std::time::Instant::now(),
                                    );
                                }
                                Err(err) => {
                                    state.focus = Focus::Normal;
                                    state.push_toast(
                                        pacode_types::ToastLevel::Error,
                                        "Failed to create theme".to_string(),
                                        Some(err.to_string()),
                                        std::time::Instant::now(),
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            vec![]
        }
        Focus::Overlay(Overlay::ConfigPicker {
            index,
            editing_number,
        }) => {
            if let Some(buf) = editing_number {
                match key.code {
                    KeyCode::Esc => *editing_number = None,
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() => buf.push(c),
                    KeyCode::Enter => {
                        let text = buf.clone();
                        let idx = *index;
                        *editing_number = None;
                        match idx {
                            7 => {
                                if let Ok(n) = text.parse::<u64>() {
                                    state.config.exec.yield_after_secs = n;
                                    let _ = pacode_config::update_config_value(
                                        &state.paths,
                                        "exec.yield_after_secs",
                                        pacode_config::toml::Value::Integer(n as i64),
                                    );
                                    state.push_notice(format!(
                                        "saved exec.yield_after_secs = {n} (restart the daemon for daemon/exec settings)"
                                    ));
                                }
                            }
                            8 => {
                                if let Ok(n) = text.parse::<usize>() {
                                    state.config.agents.max_live = n;
                                    let _ = pacode_config::update_config_value(
                                        &state.paths,
                                        "agents.max_live",
                                        pacode_config::toml::Value::Integer(n as i64),
                                    );
                                    state.push_notice(format!(
                                        "saved agents.max_live = {n} (restart the daemon for daemon/exec settings)"
                                    ));
                                }
                            }
                            9 => {
                                if let Ok(n) = text.parse::<u64>() {
                                    state.config.daemon.idle_timeout_secs = n;
                                    let _ = pacode_config::update_config_value(
                                        &state.paths,
                                        "daemon.idle_timeout_secs",
                                        pacode_config::toml::Value::Integer(n as i64),
                                    );
                                    state.push_notice(format!(
                                        "saved daemon.idle_timeout_secs = {n} (restart the daemon for daemon/exec settings)"
                                    ));
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                return vec![];
            }

            if is_overlay_up(&key) && *index > 0 {
                *index -= 1;
                return vec![];
            }
            if is_overlay_down(&key) && *index + 1 < 10 {
                *index += 1;
                return vec![];
            }

            match key.code {
                KeyCode::Esc => state.focus = Focus::Normal,
                KeyCode::Enter | KeyCode::Left | KeyCode::Right => {
                    let is_enter = key.code == KeyCode::Enter;
                    match *index {
                        0 => {
                            state.focus = Focus::Overlay(Overlay::ModelPicker {
                                query: String::new(),
                                index: 0,
                            });
                            return vec![Action::Send(Request::ListModels)];
                        }
                        1 => {
                            if is_enter {
                                let cur = pacode_types::Effort::ALL
                                    .iter()
                                    .position(|e| *e == state.effort())
                                    .unwrap_or(1);
                                state.focus = Focus::Overlay(Overlay::EffortPicker { index: cur });
                            } else {
                                let all = pacode_types::Effort::ALL;
                                let cur =
                                    all.iter().position(|e| *e == state.effort()).unwrap_or(0);
                                let next = all[(cur + 1) % all.len()];
                                state.config.provider.effort = next;
                                let _ = pacode_config::update_config_value(
                                    &state.paths,
                                    "provider.effort",
                                    pacode_config::toml::Value::String(next.as_str().to_string()),
                                );
                                state.save_pref_effort(next);
                                state.push_notice(format!(
                                    "saved provider.effort = \"{next}\" (restart the daemon for daemon/exec settings)"
                                ));
                                return vec![Action::Send(Request::SetEffort(next))];
                            }
                        }
                        2 => {
                            if is_enter {
                                let cur = pacode_types::Mode::CYCLE
                                    .iter()
                                    .position(|m| *m == state.mode())
                                    .unwrap_or(0);
                                state.focus = Focus::Overlay(Overlay::ModePicker { index: cur });
                            } else {
                                let next = state.mode().next();
                                state.config.permissions.default_mode = next;
                                let _ = pacode_config::update_config_value(
                                    &state.paths,
                                    "permissions.default_mode",
                                    pacode_config::toml::Value::String(next.as_str().to_string()),
                                );
                                state.save_pref_mode(next);
                                state.push_notice(format!(
                                    "saved permissions.default_mode = \"{}\" (restart the daemon for daemon/exec settings)",
                                    next.as_str()
                                ));
                                return vec![Action::Send(Request::SetMode(next))];
                            }
                        }
                        3 => {
                            let new_val = !state.config.ui.mouse;
                            state.config.ui.mouse = new_val;
                            let _ = pacode_config::update_config_value(
                                &state.paths,
                                "ui.mouse",
                                pacode_config::toml::Value::Boolean(new_val),
                            );
                            state.push_notice(format!(
                                "saved ui.mouse = {new_val} (restart the daemon for daemon/exec settings)"
                            ));
                        }
                        4 => {
                            let new_val = !state.config.ui.ascii_only;
                            state.config.ui.ascii_only = new_val;
                            let _ = pacode_config::update_config_value(
                                &state.paths,
                                "ui.ascii_only",
                                pacode_config::toml::Value::Boolean(new_val),
                            );
                            state.push_notice(format!(
                                "saved ui.ascii_only = {new_val} (restart the daemon for daemon/exec settings)"
                            ));
                        }
                        5 => {
                            let new_val = !state.config.ui.hints.effort;
                            state.config.ui.hints.effort = new_val;
                            let _ = pacode_config::update_config_value(
                                &state.paths,
                                "ui.hints.effort",
                                pacode_config::toml::Value::Boolean(new_val),
                            );
                            state.push_notice(format!(
                                "saved ui.hints.effort = {new_val} (restart the daemon for daemon/exec settings)"
                            ));
                        }
                        6 => {
                            let colors = ["auto", "truecolor", "ansi"];
                            let cur = colors
                                .iter()
                                .position(|&c| c == state.config.ui.color)
                                .unwrap_or(0);
                            let next_idx = if key.code == KeyCode::Left {
                                (cur + 2) % 3
                            } else {
                                (cur + 1) % 3
                            };
                            let new_val = colors[next_idx].to_string();
                            state.config.ui.color = new_val.clone();
                            let _ = pacode_config::update_config_value(
                                &state.paths,
                                "ui.color",
                                pacode_config::toml::Value::String(new_val.clone()),
                            );
                            state.push_notice(format!(
                                "saved ui.color = \"{new_val}\" (restart the daemon for daemon/exec settings)"
                            ));
                        }
                        7 => *editing_number = Some(state.config.exec.yield_after_secs.to_string()),
                        8 => *editing_number = Some(state.config.agents.max_live.to_string()),
                        9 => {
                            *editing_number =
                                Some(state.config.daemon.idle_timeout_secs.to_string());
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            vec![]
        }
        Focus::Normal | Focus::SelectAgent { .. } | Focus::Panel { .. } | Focus::BgList { .. } => {
            vec![]
        }
    }
}
