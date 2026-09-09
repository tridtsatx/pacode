//! Keyboard handling for bottom in-place pickers (effort, mode, model, config).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use codeapp_types::Request;

use crate::keys::Action;
use crate::state::{AppState, Focus, Overlay};

pub fn handle_picker_key(state: &mut AppState, key: KeyEvent) -> Vec<Action> {
    match &mut state.focus {
        Focus::Overlay(Overlay::EffortPicker { index }) => {
            match key.code {
                KeyCode::Esc => {
                    state.focus = Focus::Normal;
                }
                KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => {
                    *index = index.saturating_sub(1);
                }
                KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') => {
                    *index = (*index + 1).min(3);
                }
                KeyCode::Enter => {
                    let effort = codeapp_types::Effort::ALL[*index];
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
                KeyCode::Esc => {
                    state.focus = Focus::Normal;
                }
                KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => {
                    *index = index.saturating_sub(1);
                }
                KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') => {
                    *index = (*index + 1).min(3);
                }
                KeyCode::Enter => {
                    let mode = codeapp_types::Mode::CYCLE[*index];
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

            match key.code {
                KeyCode::Esc => {
                    state.focus = Focus::Normal;
                }
                KeyCode::Up | KeyCode::Char('p')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if *index > 0 {
                        *index -= 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('n')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if filtered_count > 0 && *index + 1 < filtered_count {
                        *index += 1;
                    }
                }
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
        Focus::Overlay(Overlay::ConfigPicker {
            index,
            editing_number,
        }) => {
            if let Some(buf) = editing_number {
                match key.code {
                    KeyCode::Esc => {
                        *editing_number = None;
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() => {
                        buf.push(c);
                    }
                    KeyCode::Enter => {
                        let text = buf.clone();
                        let idx = *index;
                        *editing_number = None;
                        match idx {
                            7 => {
                                if let Ok(n) = text.parse::<u64>() {
                                    state.config.exec.yield_after_secs = n;
                                    let _ = codeapp_config::update_config_value(
                                        &state.paths,
                                        "exec.yield_after_secs",
                                        codeapp_config::toml::Value::Integer(n as i64),
                                    );
                                    state.push_notice(format!(
                                        "saved exec.yield_after_secs = {n} (restart the daemon for daemon/exec settings)"
                                    ));
                                }
                            }
                            8 => {
                                if let Ok(n) = text.parse::<usize>() {
                                    state.config.agents.max_live = n;
                                    let _ = codeapp_config::update_config_value(
                                        &state.paths,
                                        "agents.max_live",
                                        codeapp_config::toml::Value::Integer(n as i64),
                                    );
                                    state.push_notice(format!(
                                        "saved agents.max_live = {n} (restart the daemon for daemon/exec settings)"
                                    ));
                                }
                            }
                            9 => {
                                if let Ok(n) = text.parse::<u64>() {
                                    state.config.daemon.idle_timeout_secs = n;
                                    let _ = codeapp_config::update_config_value(
                                        &state.paths,
                                        "daemon.idle_timeout_secs",
                                        codeapp_config::toml::Value::Integer(n as i64),
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

            match key.code {
                KeyCode::Esc => {
                    state.focus = Focus::Normal;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if *index > 0 {
                        *index -= 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if *index + 1 < 10 {
                        *index += 1;
                    }
                }
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
                                let cur = codeapp_types::Effort::ALL
                                    .iter()
                                    .position(|e| *e == state.effort())
                                    .unwrap_or(1);
                                state.focus = Focus::Overlay(Overlay::EffortPicker { index: cur });
                            } else {
                                let next = match state.effort() {
                                    codeapp_types::Effort::Low => codeapp_types::Effort::Medium,
                                    codeapp_types::Effort::Medium => codeapp_types::Effort::High,
                                    codeapp_types::Effort::High => codeapp_types::Effort::Max,
                                    codeapp_types::Effort::Max => codeapp_types::Effort::Low,
                                };
                                state.config.provider.effort = next;
                                let _ = codeapp_config::update_config_value(
                                    &state.paths,
                                    "provider.effort",
                                    codeapp_config::toml::Value::String(next.as_str().to_string()),
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
                                let cur = codeapp_types::Mode::CYCLE
                                    .iter()
                                    .position(|m| *m == state.mode())
                                    .unwrap_or(0);
                                state.focus = Focus::Overlay(Overlay::ModePicker { index: cur });
                            } else {
                                let next = state.mode().next();
                                state.config.permissions.default_mode = next;
                                let _ = codeapp_config::update_config_value(
                                    &state.paths,
                                    "permissions.default_mode",
                                    codeapp_config::toml::Value::String(next.as_str().to_string()),
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
                            let _ = codeapp_config::update_config_value(
                                &state.paths,
                                "ui.mouse",
                                codeapp_config::toml::Value::Boolean(new_val),
                            );
                            state.push_notice(format!(
                                "saved ui.mouse = {new_val} (restart the daemon for daemon/exec settings)"
                            ));
                        }
                        4 => {
                            let new_val = !state.config.ui.ascii_only;
                            state.config.ui.ascii_only = new_val;
                            let _ = codeapp_config::update_config_value(
                                &state.paths,
                                "ui.ascii_only",
                                codeapp_config::toml::Value::Boolean(new_val),
                            );
                            state.push_notice(format!(
                                "saved ui.ascii_only = {new_val} (restart the daemon for daemon/exec settings)"
                            ));
                        }
                        5 => {
                            let new_val = !state.config.ui.hints.effort;
                            state.config.ui.hints.effort = new_val;
                            let _ = codeapp_config::update_config_value(
                                &state.paths,
                                "ui.hints.effort",
                                codeapp_config::toml::Value::Boolean(new_val),
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
                            let _ = codeapp_config::update_config_value(
                                &state.paths,
                                "ui.color",
                                codeapp_config::toml::Value::String(new_val.clone()),
                            );
                            state.push_notice(format!(
                                "saved ui.color = \"{new_val}\" (restart the daemon for daemon/exec settings)"
                            ));
                        }
                        7 => {
                            *editing_number = Some(state.config.exec.yield_after_secs.to_string());
                        }
                        8 => {
                            *editing_number = Some(state.config.agents.max_live.to_string());
                        }
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
        _ => vec![],
    }
}
