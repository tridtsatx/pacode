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
    let prev_overlay = match &state.focus {
        Focus::Overlay(o) => Some(crate::nav::overlay_name(o)),
        _ => None,
    };
    let actions = handle_picker_key_inner(state, key);
    if let Some(name) = prev_overlay
        && !matches!(state.focus, Focus::Overlay(_))
    {
        log::debug!("close overlay: {name}");
    }
    actions
}

fn handle_picker_key_inner(state: &mut AppState, key: KeyEvent) -> Vec<Action> {
    match &mut state.focus {
        Focus::Overlay(Overlay::QuestionPicker {
            question,
            index,
            selected,
            typed,
            typing,
        }) => {
            let count = question.options.len();
            let multi = question.multi_select;
            let id = question.id.clone();

            if *typing {
                match key.code {
                    KeyCode::Esc => {
                        *typing = false;
                        typed.clear();
                    }
                    KeyCode::Enter => {
                        let answer = pacode_types::QuestionAnswer::typed(typed.clone());
                        state.focus = Focus::Normal;
                        return vec![Action::Send(pacode_types::Request::AnswerQuestion {
                            question: id,
                            answer,
                        })];
                    }
                    KeyCode::Backspace => {
                        typed.pop();
                    }
                    KeyCode::Char(c) => typed.push(c),
                    _ => {}
                }
                state.dirty = true;
                return vec![];
            }

            match key.code {
                // Dismissing answers the question as dismissed: the turn is
                // waiting on it, so leaving it unanswered would hang the model.
                KeyCode::Esc => {
                    state.focus = Focus::Normal;
                    return vec![Action::Send(pacode_types::Request::AnswerQuestion {
                        question: id,
                        answer: pacode_types::QuestionAnswer::cancelled(),
                    })];
                }
                KeyCode::Up | KeyCode::Char('k') => *index = index.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *index = (*index + 1).min(count.saturating_sub(1));
                }
                KeyCode::Char('t') => *typing = true,
                KeyCode::Char(' ') if multi => {
                    if let Some(pos) = selected.iter().position(|i| i == index) {
                        selected.remove(pos);
                    } else {
                        selected.push(*index);
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                    let picked = (c as usize) - ('0' as usize) - 1;
                    if picked < count {
                        state.focus = Focus::Normal;
                        return vec![Action::Send(pacode_types::Request::AnswerQuestion {
                            question: id,
                            answer: pacode_types::QuestionAnswer::choice(picked),
                        })];
                    }
                }
                KeyCode::Enter => {
                    let answer = if multi && !selected.is_empty() {
                        pacode_types::QuestionAnswer {
                            selected: selected.clone(),
                            free_text: None,
                            cancelled: false,
                        }
                    } else {
                        pacode_types::QuestionAnswer::choice(*index)
                    };
                    state.focus = Focus::Normal;
                    return vec![Action::Send(pacode_types::Request::AnswerQuestion {
                        question: id,
                        answer,
                    })];
                }
                _ => {}
            }
            state.dirty = true;
            vec![]
        }
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
        Focus::Overlay(Overlay::LoginPicker { query, index }) => {
            let q_lower = query.to_lowercase();
            let filtered_count = state
                .auth_providers
                .iter()
                .filter(|p| {
                    q_lower.is_empty()
                        || p.id.to_lowercase().contains(&q_lower)
                        || p.display_name.to_lowercase().contains(&q_lower)
                        || p.auth_kind.to_lowercase().contains(&q_lower)
                        || p.detail.to_lowercase().contains(&q_lower)
                })
                .count();

            if is_prev(&key) && *index > 0 {
                *index -= 1;
                state.dirty = true;
                return vec![];
            }
            if is_next(&key) && filtered_count > 0 && *index + 1 < filtered_count {
                *index += 1;
                state.dirty = true;
                return vec![];
            }

            match key.code {
                KeyCode::Esc => {
                    state.focus = Focus::Normal;
                    state.dirty = true;
                }
                KeyCode::Enter => {
                    let filtered: Vec<_> = state
                        .auth_providers
                        .iter()
                        .filter(|p| {
                            q_lower.is_empty()
                                || p.id.to_lowercase().contains(&q_lower)
                                || p.display_name.to_lowercase().contains(&q_lower)
                                || p.auth_kind.to_lowercase().contains(&q_lower)
                                || p.detail.to_lowercase().contains(&q_lower)
                        })
                        .collect();
                    if let Some(p) = filtered.get(*index) {
                        let provider = p.id.clone();
                        return vec![Action::Send(Request::Login { provider })];
                    }
                }
                KeyCode::Char('a')
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    let filtered: Vec<_> = state
                        .auth_providers
                        .iter()
                        .filter(|p| {
                            q_lower.is_empty()
                                || p.id.to_lowercase().contains(&q_lower)
                                || p.display_name.to_lowercase().contains(&q_lower)
                                || p.auth_kind.to_lowercase().contains(&q_lower)
                                || p.detail.to_lowercase().contains(&q_lower)
                        })
                        .collect();
                    if let Some(p) = filtered.get(*index)
                        && p.accounts.len() > 1
                    {
                        let cur = p
                            .active
                            .as_ref()
                            .and_then(|act| p.accounts.iter().position(|acc| acc == act))
                            .unwrap_or(0);
                        let next = (cur + 1) % p.accounts.len();
                        let label = p.accounts[next].clone();
                        return vec![Action::Send(Request::SetAuthAccount {
                            provider: p.id.clone(),
                            label,
                        })];
                    }
                    query.push('a');
                    *index = 0;
                    state.dirty = true;
                }
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let filtered: Vec<_> = state
                        .auth_providers
                        .iter()
                        .filter(|p| {
                            q_lower.is_empty()
                                || p.id.to_lowercase().contains(&q_lower)
                                || p.display_name.to_lowercase().contains(&q_lower)
                                || p.auth_kind.to_lowercase().contains(&q_lower)
                                || p.detail.to_lowercase().contains(&q_lower)
                        })
                        .collect();
                    if let Some(p) = filtered.get(*index) {
                        return vec![Action::Send(Request::Logout {
                            provider: p.id.clone(),
                            label: p.active.clone(),
                        })];
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    *index = 0;
                    state.dirty = true;
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    query.push(c);
                    *index = 0;
                    state.dirty = true;
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
        Focus::Overlay(Overlay::PluginsPicker {
            index,
            plugins,
            tab,
            market,
            query,
            loading,
            stale,
        }) => {
            use crate::state::PluginsTab;

            let count = match tab {
                PluginsTab::Installed => plugins.len(),
                PluginsTab::Discover => market.len(),
            };
            let source = state.config.plugins.marketplace.clone();

            if is_overlay_up(&key) && *index > 0 {
                *index -= 1;
                return vec![];
            }
            if is_overlay_down(&key) && count > 0 && *index + 1 < count {
                *index += 1;
                return vec![];
            }

            match key.code {
                KeyCode::Esc => {
                    state.focus = Focus::Normal;
                    state.dirty = true;
                    return vec![];
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    *tab = tab.next();
                    *index = 0;
                    state.dirty = true;
                    // The Discover tab is empty until the marketplace answers.
                    if *tab == PluginsTab::Discover && market.is_empty() && !source.is_empty() {
                        *loading = true;
                        let query = query.clone();
                        return vec![Action::Send(Request::BrowseMarketplace { source, query })];
                    }
                }
                // Typing filters Discover; the request goes out on every keystroke
                // but the index is served from cache, so it costs no network.
                KeyCode::Char(c) if *tab == PluginsTab::Discover && !source.is_empty() => match c {
                    'i' if !market.is_empty() => {
                        let name = market[(*index).min(market.len() - 1)].name.clone();
                        *loading = true;
                        state.dirty = true;
                        // The install reply refreshes Discover; ListPlugins
                        // makes the new plugin show up under Installed.
                        return vec![
                            Action::Send(Request::InstallPlugin { source, name }),
                            Action::Send(Request::ListPlugins),
                        ];
                    }
                    _ => {
                        query.push(c);
                        *index = 0;
                        *loading = true;
                        state.dirty = true;
                        let query = query.clone();
                        return vec![Action::Send(Request::BrowseMarketplace { source, query })];
                    }
                },
                KeyCode::Backspace if *tab == PluginsTab::Discover && !source.is_empty() => {
                    query.pop();
                    *index = 0;
                    *loading = true;
                    state.dirty = true;
                    let query = query.clone();
                    return vec![Action::Send(Request::BrowseMarketplace { source, query })];
                }
                KeyCode::Char('x') if *tab == PluginsTab::Installed && !plugins.is_empty() => {
                    let p = &plugins[(*index).min(plugins.len() - 1)];
                    // Uninstall removes exactly what an install record tracks, so
                    // only marketplace-installed plugins are removable here.
                    if p.is_installed() {
                        let name = p.name.clone();
                        state.dirty = true;
                        // ListPlugins refreshes the Installed tab; the uninstall
                        // reply itself refreshes the Discover tab.
                        return vec![
                            Action::Send(Request::UninstallPlugin { name }),
                            Action::Send(Request::ListPlugins),
                        ];
                    }
                }
                _ => {}
            }
            let _ = stale;
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
                    log::warn!("failed to update config value '{dotted}': {err}");
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
                                log::warn!("failed to remove config value '{dotted}': {err}");
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
                                if let Err(e) = pacode_config::update_config_value(
                                    &state.paths,
                                    "theme.name",
                                    pacode_config::toml::Value::String(name),
                                ) {
                                    log::warn!("failed to update config value 'theme.name': {e}");
                                }
                                state.focus = Focus::Normal;
                            } else if *index < builtins.len() + user_themes.len() {
                                let name = user_themes[*index - builtins.len()].clone();
                                state.config.theme.name = name.clone();
                                if let Err(e) = pacode_config::update_config_value(
                                    &state.paths,
                                    "theme.name",
                                    pacode_config::toml::Value::String(name),
                                ) {
                                    log::warn!("failed to update config value 'theme.name': {e}");
                                }
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
                                    if let Err(e) = pacode_config::update_config_value(
                                        &state.paths,
                                        "theme.name",
                                        pacode_config::toml::Value::String(custom_name.clone()),
                                    ) {
                                        log::warn!(
                                            "failed to update config value 'theme.name': {e}"
                                        );
                                    }
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
        Focus::Overlay(Overlay::ConfigPicker) => crate::ui::config_view::handle_key(state, key),
        Focus::Normal | Focus::SelectAgent { .. } | Focus::Panel { .. } | Focus::BgList { .. } => {
            vec![]
        }
    }
}
