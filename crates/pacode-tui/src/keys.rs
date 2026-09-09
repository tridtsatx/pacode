//! Key and mouse handling (spec §6, §7 and the mockup).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::state::PermissionDecision;
use pacode_types::{PermissionRequest, Request, TranscriptKind};

use crate::binding::Action as KeyAction;
use crate::commands;
use crate::state::transcript::CellKind;
use crate::state::{AppState, Focus, Overlay, PanelTarget};

#[cfg(test)]
#[path = "keys_tests.rs"]
mod keys_tests;

/// Side effects requested by a key press.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Send(Request),
    /// Ask the daemon for the panel's content (agent transcript or task output).
    LoadPanel,
    /// Ask for an older history page for the dialog.
    LoadHistory,
    Quit,
}

pub fn handle_key(state: &mut AppState, key: KeyEvent, now: Instant) -> Vec<Action> {
    let was_dirty = state.dirty;
    state.dirty = true;

    // Dismiss remote clipboard hint popup on any key.
    state
        .toasts
        .retain(|t| t.title != crate::ui::popup::POPUP_TOAST_TITLE);

    // 1. ctrl+c / ctrl+d (Cyrillic layout letters are mapped to their Latin key).
    let key = normalize_cyrillic_ctrl(key);

    // Any key clears pending chord.
    let pending = state.pending_chord.take();
    if let Some(prev) = pending
        && prev.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(prev.code, KeyCode::Char('x'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('e'))
        && state.focus == Focus::Normal
    {
        crate::editor::open_editor(state);
        return vec![];
    }

    // ctrl+shift+c: copy current selection
    if state.keymap.action_for(key) == Some(KeyAction::CopySelection) {
        if state.selection.is_active() && !state.selection.is_empty() {
            state.selection.request_explicit_copy();
        }
        return vec![];
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('d')) {
        state.quit = true;
        return vec![Action::Quit];
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        if !state.input.is_empty() {
            state.input.text.clear();
            state.input.cursor = 0;
            state.input.history_index = None;
            state.input.draft.clear();
            state.ctrl_c_at = None;
            return vec![];
        }
        if state.turn_active {
            state.ctrl_c_at = None;
            return vec![Action::Send(Request::Interrupt)];
        }
        if let Some(prev) = state.ctrl_c_at
            && now.saturating_duration_since(prev) <= Duration::from_secs(2)
        {
            state.quit = true;
            return vec![Action::Quit];
        }
        state.ctrl_c_at = Some(now);
        return vec![];
    }

    // 1.5 While an overlay is open, ALL keys go to it (fixes focus leak to input).
    if matches!(state.focus, Focus::Overlay(Overlay::Import(_))) {
        // The handler needs `&mut AppState` too, so take the overlay out and put
        // it back unless it asked to close.
        let taken = std::mem::replace(&mut state.focus, Focus::Normal);
        let Focus::Overlay(Overlay::Import(mut overlay)) = taken else {
            return vec![];
        };
        let actions = crate::ui::import::handle_key(state, &mut overlay, key, now);
        if !overlay.closed {
            state.focus = Focus::Overlay(Overlay::Import(overlay));
        }
        return actions;
    }
    if let Focus::Overlay(_) = state.focus {
        return crate::keys_picker::handle_picker_key(state, key);
    }

    // Readline chord ctrl+x ctrl+e: start chord when ctrl+x arrives with prompt focused.
    if state.focus == Focus::Normal
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('x'))
    {
        state.pending_chord = Some(key);
        return vec![];
    }

    let action = state.keymap.action_for(key);

    // 2. Session picker via ctrl+p
    if action == Some(KeyAction::SessionPicker) {
        state.focus = Focus::Overlay(Overlay::SessionPicker {
            query: String::new(),
            index: 0,
        });
        return vec![Action::Send(Request::ListSessions { limit: 50 })];
    }

    // 3. Shift+Tab: cycle permission mode; plain Tab: autocomplete slash command
    if action == Some(KeyAction::CycleMode) {
        let next_mode = state.mode().next();
        return vec![Action::Send(Request::SetMode(next_mode))];
    }
    if key.code == KeyCode::Tab && !key.modifiers.contains(KeyModifiers::SHIFT) {
        if state.input.text.starts_with('/') && !state.input.text.contains(' ') {
            let query = &state.input.text[1..];
            let matches = commands::matching(query);
            if !matches.is_empty() {
                let selected = state.input.slash_index % matches.len();
                let cmd = &matches[selected];
                let name = &cmd.name;
                state.input.text = format!("/{name} ");
                state.input.cursor = state.input.text.chars().count();
                state.input.slash_index = 0;
            }
        }
        return vec![];
    }

    // 4. Escape: peel layers one by one
    if action == Some(KeyAction::Cancel) {
        if state.config.ui.vim && state.focus == Focus::Normal {
            // Handled by vim::handle on the normal prompt
        } else {
            return handle_esc(state);
        }
    }

    // 5. Follow shortcut: alt+f
    if action == Some(KeyAction::FollowAgent) {
        return handle_follow(state);
    }

    // 5.1 Direct agent selection: alt+1..9
    if let Some(agent_idx) = match action {
        Some(KeyAction::SelectAgent1) => Some(0),
        Some(KeyAction::SelectAgent2) => Some(1),
        Some(KeyAction::SelectAgent3) => Some(2),
        Some(KeyAction::SelectAgent4) => Some(3),
        Some(KeyAction::SelectAgent5) => Some(4),
        Some(KeyAction::SelectAgent6) => Some(5),
        Some(KeyAction::SelectAgent7) => Some(6),
        Some(KeyAction::SelectAgent8) => Some(7),
        Some(KeyAction::SelectAgent9) => Some(8),
        _ => None,
    } {
        let agents = selectable_agents(state);
        if let Some(id) = agents.get(agent_idx).cloned() {
            return select_agent_by_id(state, &id);
        } else {
            state.dirty = was_dirty;
            return vec![];
        }
    }

    // 5.2 Direct session slot switching: ctrl+alt+1..9
    if let Some(slot_idx) = match action {
        Some(KeyAction::SelectSession1) => Some(0),
        Some(KeyAction::SelectSession2) => Some(1),
        Some(KeyAction::SelectSession3) => Some(2),
        Some(KeyAction::SelectSession4) => Some(3),
        Some(KeyAction::SelectSession5) => Some(4),
        Some(KeyAction::SelectSession6) => Some(5),
        Some(KeyAction::SelectSession7) => Some(6),
        Some(KeyAction::SelectSession8) => Some(7),
        Some(KeyAction::SelectSession9) => Some(8),
        _ => None,
    } {
        return switch_to_session_slot(state, slot_idx);
    }

    // 5.5 Files overlay: alt+b
    if action == Some(KeyAction::FilesOverlay) {
        state.focus = Focus::Overlay(Overlay::Files { index: 0 });
        return vec![];
    }

    // 6. Navigation: alt+down/ctrl+j (next agent/task), alt+up/ctrl+k (prev agent/task)
    if action == Some(KeyAction::NextAgent) {
        return handle_navigate_down(state);
    }
    if action == Some(KeyAction::PrevAgent) {
        return handle_navigate_up(state);
    }

    // 7. Scrolling: PgUp, PgDn, Home, End
    if let Some(
        scroll_act @ (KeyAction::ScrollUp
        | KeyAction::ScrollDown
        | KeyAction::ScrollTop
        | KeyAction::ScrollBottom),
    ) = action
    {
        return handle_scroll(state, scroll_act);
    }

    // 8. Interactive permission prompt when input is empty: y / a / n
    if state.input.is_empty()
        && let Some(pending_req) = find_pending_permission(state)
    {
        let decision = match key.code {
            KeyCode::Char('y') => Some(PermissionDecision::AllowOnce),
            KeyCode::Char('a') => Some(PermissionDecision::AllowSession),
            KeyCode::Char('n') => Some(PermissionDecision::Deny),
            _ => None,
        };
        if let Some(decision) = decision {
            return vec![Action::Send(Request::PermissionReply {
                permission: pending_req.id,
                decision,
            })];
        }
    }

    // 8.5 Vim mode handling on normal prompt
    if state.config.ui.vim && state.focus == Focus::Normal {
        match crate::state::vim::handle(&mut state.vim, &mut state.input, key) {
            crate::state::vim::VimEffect::Consumed => return vec![],
            crate::state::vim::VimEffect::Submit => return submit_prompt(state),
            crate::state::vim::VimEffect::PassThrough => {}
        }
    }

    // 9. Single keys in empty prompt: '.', 's', 'k'
    if state.input.is_empty() && (!state.config.ui.vim || state.focus != Focus::Normal) {
        if action == Some(KeyAction::BgList) {
            state.focus = Focus::BgList { index: 0 };
            return vec![];
        }
        if action == Some(KeyAction::StopAgent)
            && let Focus::Panel {
                target: PanelTarget::Agent(ref id),
                ..
            } = state.focus
        {
            return vec![Action::Send(Request::StopAgent(id.clone()))];
        }
        if action == Some(KeyAction::KillTask) {
            if let Focus::Panel {
                target: PanelTarget::Task(ref id),
                ..
            } = state.focus
            {
                return vec![Action::Send(Request::KillTask(id.clone()))];
            } else if let Focus::BgList { index } = state.focus
                && let Some(task) = state.rail.tasks.get(index)
            {
                return vec![Action::Send(Request::KillTask(task.id.clone()))];
            }
        }
    }

    // 10. Enter key
    if action == Some(KeyAction::Newline) {
        state.input.insert_char('\n');
        return vec![];
    }

    if action == Some(KeyAction::Submit) {
        if !state.input.is_empty() {
            return submit_prompt(state);
        }

        // Enter with empty prompt: open selection in panel
        match &state.focus {
            Focus::SelectAgent { index } => {
                let agents = selectable_agents(state);
                if let Some(id) = agents.get(*index).cloned() {
                    return select_agent_by_id(state, &id);
                }
            }
            Focus::BgList { index } => {
                if let Some(task) = state.rail.tasks.get(*index) {
                    state.focus = Focus::Panel {
                        target: PanelTarget::Task(task.id.clone()),
                        follow: false,
                        follow_paused: false,
                    };
                    state.panel.target = Some(PanelTarget::Task(task.id.clone()));
                    return vec![Action::LoadPanel];
                }
            }
            Focus::Overlay(Overlay::ModelPicker { query, index }) => {
                let q_lower = query.to_lowercase();
                let filtered: Vec<_> = state
                    .models
                    .iter()
                    .filter(|m| {
                        q_lower.is_empty()
                            || m.route.to_string().to_lowercase().contains(&q_lower)
                            || m.display_name.to_lowercase().contains(&q_lower)
                    })
                    .collect();
                if let Some(model) = filtered.get(*index) {
                    let route = model.route.clone();
                    state.focus = Focus::Normal;
                    return vec![Action::Send(Request::SetModel(route))];
                }
            }
            Focus::Overlay(Overlay::EffortPicker { index }) => {
                if let Some(effort) = pacode_types::model::Effort::ALL.get(*index) {
                    let eff = *effort;
                    state.focus = Focus::Normal;
                    return vec![Action::Send(Request::SetEffort(eff))];
                }
            }
            Focus::Overlay(Overlay::SessionPicker { query, index }) => {
                let q_lower = query.to_lowercase();
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
            Focus::Normal
            | Focus::Panel { .. }
            | Focus::Overlay(
                Overlay::Files { .. }
                | Overlay::ModePicker { .. }
                | Overlay::ConfigPicker { .. }
                | Overlay::McpPicker { .. }
                | Overlay::PluginsPicker { .. }
                | Overlay::Import(_)
                | Overlay::RailOverlay
                | Overlay::Help
                | Overlay::ThemePicker { .. }
                | Overlay::KeysPicker { .. },
            ) => {}
        }
        return vec![];
    }

    // 11. Up / Down arrows for history / slash popup
    if key.code == KeyCode::Up {
        if state.input.text.starts_with('/') && !state.input.text.contains(' ') {
            let matches = commands::matching(&state.input.text[1..]);
            if !matches.is_empty() {
                let n = matches.len();
                state.input.slash_index = (state.input.slash_index + n - 1) % n;
            }
        } else if state.input.is_empty() || state.input.history_index.is_some() {
            state.input.history_up();
        }
        return vec![];
    }
    if key.code == KeyCode::Down {
        if state.input.text.starts_with('/') && !state.input.text.contains(' ') {
            let matches = commands::matching(&state.input.text[1..]);
            if !matches.is_empty() {
                let n = matches.len();
                state.input.slash_index = (state.input.slash_index + 1) % n;
            }
        } else if state.input.history_index.is_some() {
            state.input.history_down();
        }
        return vec![];
    }

    // 12. Standard editing bindings
    if action == Some(KeyAction::ClearInput) {
        state.input.text.clear();
        state.input.cursor = 0;
        return vec![];
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('w')) {
        state.input.delete_word();
        return vec![];
    }

    match key.code {
        KeyCode::Left => state.input.move_left(),
        KeyCode::Right => state.input.move_right(),
        KeyCode::Backspace => state.input.backspace(),
        KeyCode::Delete => state.input.delete(),
        KeyCode::Char(c) => state.input.insert_char(c),
        _ => {}
    }

    vec![]
}

pub use crate::nav::{
    handle_esc, handle_follow, handle_navigate_down, handle_navigate_up, select_agent_at_index,
    select_agent_by_id, selectable_agents, switch_to_session_slot,
};

fn submit_prompt(state: &mut AppState) -> Vec<Action> {
    if !state.input.is_empty() {
        let text = state.input.take();
        if text.starts_with('/') {
            return commands::execute(state, &text);
        }
        return vec![Action::Send(Request::UserMessage { text })];
    }
    vec![]
}

fn handle_scroll(state: &mut AppState, action: KeyAction) -> Vec<Action> {
    let in_panel = matches!(state.focus, Focus::Panel { .. });
    if in_panel {
        if let Focus::Panel {
            ref mut follow_paused,
            follow: true,
            ..
        } = state.focus
        {
            if action == KeyAction::ScrollUp {
                *follow_paused = true;
            } else if action == KeyAction::ScrollBottom {
                *follow_paused = false;
                state.panel.agent_transcript.scroll_to_bottom();
                return vec![];
            }
        }
        match action {
            KeyAction::ScrollUp => state.panel.agent_transcript.scroll_by(10, 100, 20),
            KeyAction::ScrollDown => state.panel.agent_transcript.scroll_by(-10, 100, 20),
            KeyAction::ScrollTop => state.panel.agent_transcript.scroll_by(1000, 100, 20),
            KeyAction::ScrollBottom => state.panel.agent_transcript.scroll_to_bottom(),
            KeyAction::FollowAgent
            | KeyAction::FilesOverlay
            | KeyAction::NextAgent
            | KeyAction::PrevAgent
            | KeyAction::SessionPicker
            | KeyAction::BgList
            | KeyAction::Cancel
            | KeyAction::Submit
            | KeyAction::Newline
            | KeyAction::ClearInput
            | KeyAction::StopAgent
            | KeyAction::KillTask
            | KeyAction::CycleMode
            | KeyAction::CopySelection
            | KeyAction::SelectAgent1
            | KeyAction::SelectAgent2
            | KeyAction::SelectAgent3
            | KeyAction::SelectAgent4
            | KeyAction::SelectAgent5
            | KeyAction::SelectAgent6
            | KeyAction::SelectAgent7
            | KeyAction::SelectAgent8
            | KeyAction::SelectAgent9
            | KeyAction::SelectSession1
            | KeyAction::SelectSession2
            | KeyAction::SelectSession3
            | KeyAction::SelectSession4
            | KeyAction::SelectSession5
            | KeyAction::SelectSession6
            | KeyAction::SelectSession7
            | KeyAction::SelectSession8
            | KeyAction::SelectSession9 => {}
        }
    } else {
        match action {
            KeyAction::ScrollUp => state.transcript.scroll_by(10, 1000, 25),
            KeyAction::ScrollDown => state.transcript.scroll_by(-10, 1000, 25),
            KeyAction::ScrollTop => state.transcript.scroll_by(10000, 1000, 25),
            KeyAction::ScrollBottom => state.transcript.scroll_to_bottom(),
            KeyAction::FollowAgent
            | KeyAction::FilesOverlay
            | KeyAction::NextAgent
            | KeyAction::PrevAgent
            | KeyAction::SessionPicker
            | KeyAction::BgList
            | KeyAction::Cancel
            | KeyAction::Submit
            | KeyAction::Newline
            | KeyAction::ClearInput
            | KeyAction::StopAgent
            | KeyAction::KillTask
            | KeyAction::CycleMode
            | KeyAction::CopySelection
            | KeyAction::SelectAgent1
            | KeyAction::SelectAgent2
            | KeyAction::SelectAgent3
            | KeyAction::SelectAgent4
            | KeyAction::SelectAgent5
            | KeyAction::SelectAgent6
            | KeyAction::SelectAgent7
            | KeyAction::SelectAgent8
            | KeyAction::SelectAgent9
            | KeyAction::SelectSession1
            | KeyAction::SelectSession2
            | KeyAction::SelectSession3
            | KeyAction::SelectSession4
            | KeyAction::SelectSession5
            | KeyAction::SelectSession6
            | KeyAction::SelectSession7
            | KeyAction::SelectSession8
            | KeyAction::SelectSession9 => {}
        }
        if (action == KeyAction::ScrollUp || action == KeyAction::ScrollTop)
            && state.transcript.is_at_top(1000, 25)
            && state.can_load_history()
        {
            state.transcript.loading_history = true;
            return vec![Action::LoadHistory];
        }
    }
    vec![]
}

fn find_pending_permission(state: &AppState) -> Option<PermissionRequest> {
    state.transcript.cells.iter().find_map(|c| {
        if let CellKind::Item(TranscriptKind::Permission(ref req)) = c.kind {
            Some(req.clone())
        } else {
            None
        }
    })
}

pub use crate::mouse::handle_mouse;

/// Map a Cyrillic (ЙЦУКЕН) letter pressed with CONTROL to the Latin letter on the same
/// key, so `ctrl+в` acts as `ctrl+d`. Legacy terminals already send the control byte;
/// this covers the kitty keyboard protocol where the Unicode letter arrives.
pub fn normalize_cyrillic_ctrl(key: KeyEvent) -> KeyEvent {
    if !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return key;
    }
    let KeyCode::Char(c) = key.code else {
        return key;
    };
    let mapped = match c.to_lowercase().next().unwrap_or(c) {
        'й' => 'q',
        'ц' => 'w',
        'у' => 'e',
        'к' => 'r',
        'е' => 't',
        'н' => 'y',
        'г' => 'u',
        'ш' => 'i',
        'щ' => 'o',
        'з' => 'p',
        'ф' => 'a',
        'ы' => 's',
        'в' => 'd',
        'а' => 'f',
        'п' => 'g',
        'р' => 'h',
        'о' => 'j',
        'л' => 'k',
        'д' => 'l',
        'я' => 'z',
        'ч' => 'x',
        'с' => 'c',
        'м' => 'v',
        'и' => 'b',
        'т' => 'n',
        'ь' => 'm',
        _ => return key,
    };
    KeyEvent {
        code: KeyCode::Char(mapped),
        ..key
    }
}
