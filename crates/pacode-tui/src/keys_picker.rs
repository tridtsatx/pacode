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

/// Whether `key` matches any chord currently bound to `action`. The pickers
/// look their own actions up directly instead of `Keymap::action_for`:
/// that resolver scans every scope and a global action spelled with the
/// same chord (`files_overlay` on `alt+f`) would win the shared entry (§9.1).
fn keymap_has(
    keymap: &crate::binding::Keymap,
    action: crate::binding::Action,
    key: KeyEvent,
) -> bool {
    keymap
        .bindings_for(action)
        .iter()
        .any(|b| crate::binding::matches_key(b, key))
}

/// The per-model flag a mark key toggles in the model picker (§9.1).
#[derive(Clone, Copy)]
enum ModelMark {
    Zdr,
    Favorite,
}

/// Toggle `mark` on the filtered row at `index`, persist it, and send
/// `SetModelFlag` so the daemon enforces the same flag server-side. The ZDR
/// gate mirrors the daemon's: the active route cannot lose its mark while
/// ZDR is on.
fn toggle_model_mark(
    state: &mut AppState,
    index: usize,
    q_lower: &str,
    zdr_only: bool,
    mark: ModelMark,
) -> Vec<Action> {
    let route_key = {
        let rows =
            crate::state::picker_models(&state.models, &state.model_flags, q_lower, zdr_only);
        rows.get(index).map(|m| m.route.to_string())
    };
    let Some(route_key) = route_key else {
        return vec![];
    };
    let mut flags = state
        .model_flags
        .get(&route_key)
        .copied()
        .unwrap_or_default();
    match mark {
        ModelMark::Zdr => {
            // The daemon refuses to unmark the active route while ZDR is
            // on; gate here so the local flags don't diverge from what it
            // enforces.
            let active_route = state
                .meta
                .as_ref()
                .is_some_and(|m| m.model.to_string() == route_key);
            if flags.zdr_capable
                && active_route
                && crate::state::zdr_active(&state.config, state.meta.as_ref())
            {
                state.push_notice_with_level(
                    pacode_types::ToastLevel::Warn,
                    "can't unmark the active route while ZDR is on".to_string(),
                );
                return vec![];
            }
            flags.zdr_capable = !flags.zdr_capable;
        }
        ModelMark::Favorite => {
            flags.favorite = !flags.favorite;
        }
    }
    if flags == pacode_types::ModelFlags::default() {
        state.model_flags.remove(&route_key);
    } else {
        state.model_flags.insert(route_key.clone(), flags);
    }
    state.save_pref_model_flags();
    vec![Action::Send(Request::SetModelFlag {
        model: route_key,
        flags,
    })]
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
                // The free-text plaque sits at `index == count` — one row past
                // the last option.
                KeyCode::Down | KeyCode::Char('j') => *index = (*index + 1).min(count),
                KeyCode::Char('t') => *typing = true,
                KeyCode::Char(' ') => {
                    if *index == count {
                        // Space on the plaque means "type here" (in typing mode
                        // the Char arm above already appends it to `typed`).
                        *typing = true;
                    } else if multi {
                        if let Some(pos) = selected.iter().position(|i| i == index) {
                            selected.remove(pos);
                        } else {
                            selected.push(*index);
                        }
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                    let picked = (c as usize) - ('0' as usize) - 1;
                    if picked == count {
                        // `N+1` jumps to the free-text plaque rather than
                        // answering — typing there starts with enter.
                        *index = count;
                    } else if picked < count {
                        state.focus = Focus::Normal;
                        return vec![Action::Send(pacode_types::Request::AnswerQuestion {
                            question: id,
                            answer: pacode_types::QuestionAnswer::choice(picked),
                        })];
                    }
                }
                KeyCode::Enter => {
                    if *index == count {
                        *typing = true;
                    } else {
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
                }
                // Any other printable key is ignored: the question is a closed
                // list — free text lives only on the plaque.
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
                    // Greyed levels are unselectable: refuse and keep the
                    // picker open (spec §6).
                    if !state.effort_supported(effort) {
                        state.push_notice_with_level(
                            pacode_types::ToastLevel::Warn,
                            format!("effort {effort} is not supported by the current model"),
                        );
                        return vec![];
                    }
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
        Focus::Overlay(Overlay::ModelPicker {
            query,
            index,
            for_zdr,
        }) => {
            let q_lower = query.to_lowercase();
            let for_zdr_flag = *for_zdr;
            let zdr_only =
                for_zdr_flag || crate::state::zdr_active(&state.config, state.meta.as_ref());
            let filtered_count =
                crate::state::picker_models(&state.models, &state.model_flags, &q_lower, zdr_only)
                    .len();

            if is_prev(&key) && *index > 0 {
                *index -= 1;
                return vec![];
            }
            if is_next(&key) && filtered_count > 0 && *index + 1 < filtered_count {
                *index += 1;
                return vec![];
            }

            // §9.1: the per-model marks live on modifier chords from the key
            // registry — `alt+f` favorite / `alt+z` ZDR, with `ctrl+g` /
            // `ctrl+y` as the fallback for terminals that eat alt. Letters
            // always reach the query, so a search can start with `s`.
            let mark = if keymap_has(&state.keymap, crate::binding::Action::ModelPickerZdr, key) {
                Some(ModelMark::Zdr)
            } else if keymap_has(
                &state.keymap,
                crate::binding::Action::ModelPickerFavorite,
                key,
            ) {
                Some(ModelMark::Favorite)
            } else {
                None
            };
            if let Some(mark) = mark {
                let row = *index;
                return toggle_model_mark(state, row, &q_lower, zdr_only, mark);
            }

            match key.code {
                KeyCode::Esc => state.focus = Focus::Normal,
                KeyCode::Enter => {
                    let route = {
                        let rows = crate::state::picker_models(
                            &state.models,
                            &state.model_flags,
                            &q_lower,
                            zdr_only,
                        );
                        rows.get(*index).map(|m| m.route.clone())
                    };
                    if let Some(route) = route {
                        state.focus = Focus::Normal;
                        state.save_pref_model(&route.to_string());
                        let mut actions = vec![Action::Send(Request::SetModel(route.clone()))];
                        // If the new route doesn't support the session effort,
                        // downgrade to the nearest supported level (spec §6).
                        actions.extend(state.clamp_effort_actions(&route));
                        // Alt+Z enable flow: the route switch must land before
                        // ZDR turns on, or `set_model` would hit the ZDR guard.
                        if for_zdr_flag {
                            if let Some(m) = state.meta.as_mut() {
                                m.zdr = true;
                            }
                            actions.push(Action::Send(Request::SetZdr(true)));
                        }
                        return actions;
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
        Focus::Overlay(Overlay::RailOverlay) => {
            if key.code == KeyCode::Esc {
                state.focus = Focus::Normal;
            }
            vec![]
        }
        // Unreachable — `keys::handle_key` routes a menu to `keys_menu` before
        // the overlay dispatch — but the match stays exhaustive.
        Focus::Overlay(Overlay::Menu { .. }) => vec![],
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
        Focus::Normal | Focus::SelectAgent { .. } | Focus::Panel { .. } => vec![],
    }
}
