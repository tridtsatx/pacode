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

/// The next rung the cursor may land on, skipping levels the current model does
/// not accept.
///
/// `None` from [`AppState::supported_efforts`] means "nothing is known", and
/// every level is then allowed — the cursor moves exactly as it did before.
/// The walk is bounded by the length of the ladder, so a model that accepts
/// nothing leaves the cursor where it is instead of spinning.
pub(crate) fn next_allowed_index(
    supported: Option<&[pacode_types::Effort]>,
    from: usize,
    forward: bool,
) -> usize {
    let ladder = pacode_types::Effort::ALL;
    let last = ladder.len().saturating_sub(1);
    if ladder.is_empty() {
        return from;
    }
    // `None` means nothing is known about this model, so every rung is allowed.
    let allowed = |index: usize| {
        ladder
            .get(index)
            .is_some_and(|effort| supported.is_none_or(|levels| levels.contains(effort)))
    };
    let mut index = from;
    for _ in 0..=last {
        index = if forward {
            if index >= last { 0 } else { index + 1 }
        } else if index == 0 {
            last
        } else {
            index - 1
        };
        if allowed(index) {
            return index;
        }
    }
    from
}

pub(crate) fn handle_picker_key(state: &mut AppState, key: KeyEvent) -> Vec<Action> {
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

/// Send `answer` for the question the queue is focused on NOW. The id is
/// read off the queue at send time — never from a key handler's earlier
/// view of it — so a resolve or a new ask that shifted the focus between
/// keypresses can never stamp the stale question. The copy is dropped
/// optimistically: the `QuestionResolved` echo just finds it already gone.
fn submit_answer(state: &mut AppState, answer: pacode_types::QuestionAnswer) -> Vec<Action> {
    let Some(id) = state.questions.current().map(|q| q.id.clone()) else {
        state.focus = Focus::Normal;
        return vec![];
    };
    state.questions.resolve(&id);
    if state.questions.is_empty() {
        state.focus = Focus::Normal;
    }
    state.dirty = true;
    vec![Action::Send(pacode_types::Request::AnswerQuestion {
        question: id,
        answer,
    })]
}

fn handle_rail_overlay_key(state: &mut AppState, key: KeyEvent) -> Vec<Action> {
    let Focus::Overlay(Overlay::RailOverlay { selected }) = &state.focus else {
        return vec![];
    };
    let selected = selected.clone();
    let agents = crate::nav::selectable_agents(state);
    if agents.is_empty() {
        if key.code == KeyCode::Esc {
            state.focus = Focus::Normal;
        }
        return vec![];
    }
    let current = agents
        .iter()
        .position(|agent| agent == &selected)
        .unwrap_or(0);
    if is_overlay_up(&key) {
        state.focus = Focus::Overlay(Overlay::RailOverlay {
            selected: agents[(current + agents.len() - 1) % agents.len()].clone(),
        });
        return vec![];
    }
    if is_overlay_down(&key) {
        state.focus = Focus::Overlay(Overlay::RailOverlay {
            selected: agents[(current + 1) % agents.len()].clone(),
        });
        return vec![];
    }
    match key.code {
        KeyCode::Esc => state.focus = Focus::Normal,
        KeyCode::Enter => {
            if !agents.contains(&selected) {
                return vec![];
            }
            state.focus = Focus::Normal;
            return crate::nav::select_agent_by_id(state, &selected);
        }
        _ => {}
    }
    vec![]
}

fn handle_picker_key_inner(state: &mut AppState, key: KeyEvent) -> Vec<Action> {
    // The effort ladder needs the model's supported set, and the focus match
    // below holds `state` mutably. Copied out first, so the two never overlap.
    let supported_efforts = state
        .supported_efforts()
        .map(<[pacode_types::Effort]>::to_vec);
    if matches!(state.focus, Focus::Overlay(Overlay::RailOverlay { .. })) {
        return handle_rail_overlay_key(state, key);
    }
    match &mut state.session.focus {
        Focus::Overlay(Overlay::QuestionPicker) => {
            let Some((count, multi)) = state
                .questions
                .current()
                .map(|q| (q.options.len(), q.multi_select))
            else {
                // The queue ran dry without the picker noticing — a resolve
                // folded while the layer was up. Drop the layer.
                state.focus = Focus::Normal;
                return vec![];
            };

            if state.questions.focus().is_some_and(|f| f.typing) {
                match key.code {
                    KeyCode::Esc => {
                        let f = state.questions.focus_mut().expect("current exists");
                        f.typing = false;
                        f.typed.clear();
                    }
                    KeyCode::Enter => {
                        let typed = state
                            .questions
                            .focus()
                            .map(|f| f.typed.clone())
                            .unwrap_or_default();
                        return submit_answer(state, pacode_types::QuestionAnswer::typed(typed));
                    }
                    // Arrows still walk the queue while typing: `typed` and
                    // `typing` are stored per question, so a half-written
                    // answer is right there when the cursor comes back.
                    KeyCode::Left => state.questions.move_cursor(-1),
                    KeyCode::Right => state.questions.move_cursor(1),
                    KeyCode::Backspace => {
                        state
                            .questions
                            .focus_mut()
                            .expect("current exists")
                            .typed
                            .pop();
                    }
                    KeyCode::Char(c) => {
                        state
                            .questions
                            .focus_mut()
                            .expect("current exists")
                            .typed
                            .push(c);
                    }
                    _ => {}
                }
                state.dirty = true;
                return vec![];
            }

            match key.code {
                // Dismissing answers the question as dismissed: the turn is
                // waiting on it, so leaving it unanswered would hang the
                // model. More questions may still wait behind it.
                KeyCode::Esc => {
                    return submit_answer(state, pacode_types::QuestionAnswer::cancelled());
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(f) = state.questions.focus_mut() {
                        f.index = f.index.saturating_sub(1);
                    }
                }
                // The free-text plaque sits at `index == count` — one row past
                // the last option.
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(f) = state.questions.focus_mut() {
                        f.index = (f.index + 1).min(count);
                    }
                }
                // Several questions can wait at once: walk the queue.
                // Per-question state is keyed by id, so a half-typed answer
                // survives the round trip.
                KeyCode::Left | KeyCode::Char('h') => state.questions.move_cursor(-1),
                KeyCode::Right | KeyCode::Char('l') => state.questions.move_cursor(1),
                KeyCode::Char('t') => {
                    if let Some(f) = state.questions.focus_mut() {
                        f.typing = true;
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(f) = state.questions.focus_mut() {
                        if f.index == count {
                            // Space on the plaque means "type here" (in typing
                            // mode the Char arm above appends it to `typed`).
                            f.typing = true;
                        } else if multi {
                            if let Some(pos) = f.selected.iter().position(|i| *i == f.index) {
                                f.selected.remove(pos);
                            } else {
                                f.selected.push(f.index);
                            }
                        }
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                    let picked = (c as usize) - ('0' as usize) - 1;
                    if picked == count {
                        // `N+1` jumps to the free-text plaque rather than
                        // answering — typing there starts with enter.
                        if let Some(f) = state.questions.focus_mut() {
                            f.index = count;
                        }
                    } else if picked < count {
                        return submit_answer(state, pacode_types::QuestionAnswer::choice(picked));
                    }
                }
                // On the plaque any other printable key starts typing
                // immediately — the "Type something" row IS the input, so a
                // letter there should not fall dead.
                KeyCode::Char(c)
                    if state.questions.focus().is_some_and(|f| f.index == count)
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(f) = state.questions.focus_mut() {
                        f.typing = true;
                        f.typed.push(c);
                    }
                }
                KeyCode::Enter => {
                    let index = state.questions.focus().map(|f| f.index).unwrap_or(0);
                    if index == count {
                        if let Some(f) = state.questions.focus_mut() {
                            f.typing = true;
                        }
                    } else {
                        let selected = state
                            .questions
                            .focus()
                            .map(|f| f.selected.clone())
                            .unwrap_or_default();
                        let answer = if multi && !selected.is_empty() {
                            pacode_types::QuestionAnswer {
                                selected,
                                free_text: None,
                                cancelled: false,
                            }
                        } else {
                            pacode_types::QuestionAnswer::choice(index)
                        };
                        return submit_answer(state, answer);
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
                // The cursor skips levels this model does not accept and lands
                // on the next one that does, wrapping to the first *available*
                // rather than to the bottom of the ladder. Stepping onto a greyed
                // level is not a greyed level: the user pressed "faster" and
                // nothing happened, which reads as a dead key.
                KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => {
                    *index = next_allowed_index(supported_efforts.as_deref(), *index, false);
                }
                KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') => {
                    *index = next_allowed_index(supported_efforts.as_deref(), *index, true);
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
            let zdr_only = for_zdr_flag
                || crate::state::zdr_active(&state.config, state.session.meta.as_ref());
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

            // `ctrl+r` by default: force-refresh the catalog server-side. The
            // reply lands like `ListModels` — dispatch marks the section
            // in-flight, so the picker badges `loading…` while it runs.
            if keymap_has(
                &state.keymap,
                crate::binding::Action::ModelPickerRefresh,
                key,
            ) {
                return vec![Action::Send(Request::RefreshModels)];
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
        // The page owns only its scroll: every other key is the app's, so a
        // reader can copy from it and the usual chords keep working.
        Focus::Overlay(Overlay::Analyze { .. }) => {
            let (rows, visible) = state.analyze_geometry();
            let delta = match key.code {
                KeyCode::PageDown => i32::try_from(visible).unwrap_or(1),
                KeyCode::PageUp => -(i32::try_from(visible).unwrap_or(1)),
                KeyCode::Down | KeyCode::Char('j') => 1,
                KeyCode::Up | KeyCode::Char('k') => -1,
                KeyCode::Home => {
                    if let Focus::Overlay(Overlay::Analyze { view }) = &mut state.focus {
                        view.scroll_to_top();
                    }
                    state.dirty = true;
                    return vec![];
                }
                _ => return vec![],
            };
            if let Focus::Overlay(Overlay::Analyze { view }) = &mut state.focus {
                view.scroll_by(delta, rows, visible);
            }
            state.dirty = true;
            vec![]
        }
        // The settings page owns every key it recognises and reports the rest
        // as unhandled, so a chord that belongs to the app still works with the
        // page open.
        Focus::Overlay(Overlay::Settings { ui: _ }) => {
            // The area is read before the mutable borrow of the focus the match
            // is holding: the view lives inside it.
            let area = crate::ui::popup::resolve_current(state)
                .map_or_else(ratatui::layout::Rect::default, |popup| popup.area());
            // Taken out and put back: the view is part of the state the commit
            // needs mutably, and holding both borrows at once is not allowed.
            let Focus::Overlay(Overlay::Settings { ui }) = &mut state.session.focus else {
                return vec![];
            };
            let mut view = std::mem::take(ui);
            #[allow(clippy::mem_replace_with_default)]
            let result = crate::ui::settings_ui::commit(state, &mut view, key, area);
            if let Focus::Overlay(Overlay::Settings { ui }) = &mut state.session.focus {
                *ui = view;
            }
            match result {
                crate::ui::settings_ui::SettingsResult::Stay(actions) => {
                    state.dirty = true;
                    actions
                }
                crate::ui::settings_ui::SettingsResult::Close => {
                    state.focus = Focus::Normal;
                    state.dirty = true;
                    vec![]
                }
            }
        }
        Focus::Overlay(Overlay::RailOverlay { .. }) => vec![],
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
            let truecolor = state.truecolor;
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
