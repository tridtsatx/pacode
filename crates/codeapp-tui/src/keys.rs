//! Key and mouse handling (spec §6, §7 and the mockup).

use std::time::{Duration, Instant};

use codeapp_types::state::PermissionDecision;
use codeapp_types::{PermissionRequest, Request, TranscriptKind};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

use crate::commands;
use crate::layout::ScreenLayout;
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
    state.dirty = true;

    // 1. ctrl+c / ctrl+d (Cyrillic layout letters are mapped to their Latin key).
    let key = normalize_cyrillic_ctrl(key);
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

    // 2. Session picker via ctrl+p
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('p')) {
        state.focus = Focus::Overlay(Overlay::SessionPicker {
            query: String::new(),
            index: 0,
        });
        return vec![Action::Send(Request::ListSessions { limit: 50 })];
    }

    // 3. Shift+Tab: cycle permission mode; plain Tab: autocomplete slash command
    if key.code == KeyCode::BackTab
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
    {
        let next_mode = state.mode().next();
        return vec![Action::Send(Request::SetMode(next_mode))];
    }
    if key.code == KeyCode::Tab && !key.modifiers.contains(KeyModifiers::SHIFT) {
        if state.input.text.starts_with('/') && !state.input.text.contains(' ') {
            let query = &state.input.text[1..];
            let matches = commands::matching(query);
            if !matches.is_empty() {
                let selected = state.input.slash_index % matches.len();
                let cmd = matches[selected];
                state.input.text = format!("/{} ", cmd.name);
                state.input.cursor = state.input.text.chars().count();
                state.input.slash_index = 0;
            }
        }
        return vec![];
    }

    // 4. Escape: peel layers one by one
    if key.code == KeyCode::Esc {
        return handle_esc(state);
    }

    // 5. Follow shortcut: alt+b (alias alt+f)
    if key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char('b') | KeyCode::Char('f'))
    {
        return handle_follow(state);
    }

    // 6. Navigation: alt+down/ctrl+j (next agent/task), alt+up/ctrl+k (prev agent/task)
    let is_down = (key.modifiers.contains(KeyModifiers::ALT) && matches!(key.code, KeyCode::Down))
        || (key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('j')));
    let is_up = (key.modifiers.contains(KeyModifiers::ALT) && matches!(key.code, KeyCode::Up))
        || (key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('k')));

    if is_down {
        return handle_navigate_down(state);
    }
    if is_up {
        return handle_navigate_up(state);
    }

    // 7. Scrolling: PgUp, PgDn, Home, End
    if matches!(
        key.code,
        KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
    ) {
        return handle_scroll(state, key.code);
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

    // 9. Single keys in empty prompt: '.', 's', 'k'
    if state.input.is_empty() {
        match key.code {
            KeyCode::Char('.') => {
                state.focus = Focus::BgList { index: 0 };
                return vec![];
            }
            KeyCode::Char('s') => {
                if let Focus::Panel {
                    target: PanelTarget::Agent(ref id),
                    ..
                } = state.focus
                {
                    return vec![Action::Send(Request::StopAgent(id.clone()))];
                }
            }
            KeyCode::Char('k') => {
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
            _ => {}
        }
    }

    // 10. Enter key
    if key.code == KeyCode::Enter {
        // Shift+Enter / Alt+Enter -> newline
        if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT)
        {
            state.input.insert_char('\n');
            return vec![];
        }

        if !state.input.is_empty() {
            let text = state.input.take();
            if text.starts_with('/') {
                return commands::execute(state, &text);
            }
            return vec![Action::Send(Request::UserMessage { text })];
        }

        // Enter with empty prompt: open selection in panel
        match &state.focus {
            Focus::SelectAgent { index } => {
                let subagents: Vec<_> = state
                    .rail
                    .agents
                    .iter()
                    .filter(|a| !a.id.is_main())
                    .collect();
                if let Some(agent) = subagents.get(*index) {
                    state.focus = Focus::Panel {
                        target: PanelTarget::Agent(agent.id.clone()),
                        follow: false,
                        follow_paused: false,
                    };
                    state.panel.target = Some(PanelTarget::Agent(agent.id.clone()));
                    return vec![Action::LoadPanel];
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
                if let Some(effort) = codeapp_types::model::Effort::ALL.get(*index) {
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
                        codeapp_types::Attach::Resume { session: id },
                    ))];
                }
            }
            _ => {}
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
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('w') => {
                state.input.delete_word();
                return vec![];
            }
            KeyCode::Char('u') => {
                state.input.text.clear();
                state.input.cursor = 0;
                return vec![];
            }
            _ => {}
        }
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

fn handle_esc(state: &mut AppState) -> Vec<Action> {
    match state.focus {
        Focus::Panel {
            follow: true,
            ref target,
            ..
        } => {
            state.focus = Focus::Panel {
                target: target.clone(),
                follow: false,
                follow_paused: false,
            };
        }
        Focus::Panel { .. } => {
            state.focus = Focus::Normal;
            state.panel.target = None;
        }
        Focus::SelectAgent { .. } => {
            state.focus = Focus::Normal;
        }
        Focus::BgList { .. } => {
            state.focus = Focus::Normal;
        }
        Focus::Overlay(_) => {
            state.focus = Focus::Normal;
        }
        Focus::Normal => {}
    }
    vec![]
}

fn handle_follow(state: &mut AppState) -> Vec<Action> {
    match &state.focus {
        Focus::SelectAgent { index } => {
            let subagents: Vec<_> = state
                .rail
                .agents
                .iter()
                .filter(|a| !a.id.is_main())
                .collect();
            if let Some(agent) = subagents.get(*index) {
                state.focus = Focus::Panel {
                    target: PanelTarget::Agent(agent.id.clone()),
                    follow: true,
                    follow_paused: false,
                };
                state.panel.target = Some(PanelTarget::Agent(agent.id.clone()));
                return vec![Action::LoadPanel];
            }
        }
        Focus::Panel {
            target: PanelTarget::Agent(id),
            follow,
            follow_paused,
        } => {
            let new_follow = if *follow && *follow_paused {
                true // unpause
            } else {
                !follow
            };
            state.focus = Focus::Panel {
                target: PanelTarget::Agent(id.clone()),
                follow: new_follow,
                follow_paused: false,
            };
        }
        _ => {}
    }
    vec![]
}

fn handle_navigate_down(state: &mut AppState) -> Vec<Action> {
    match &mut state.focus {
        Focus::SelectAgent { index } => {
            let count = state.rail.agents.iter().filter(|a| !a.id.is_main()).count();
            if count > 0 && *index + 1 < count {
                *index += 1;
            }
        }
        Focus::BgList { index } => {
            let count = state.rail.tasks.len();
            if count > 0 && *index + 1 < count {
                *index += 1;
            }
        }
        Focus::Overlay(Overlay::ModelPicker { index, .. })
        | Focus::Overlay(Overlay::EffortPicker { index })
        | Focus::Overlay(Overlay::SessionPicker { index, .. }) => {
            *index += 1;
        }
        _ => {
            // Anywhere -> select first agent
            let count = state.rail.agents.iter().filter(|a| !a.id.is_main()).count();
            if count > 0 {
                state.focus = Focus::SelectAgent { index: 0 };
            }
        }
    }
    vec![]
}

fn handle_navigate_up(state: &mut AppState) -> Vec<Action> {
    match &mut state.focus {
        Focus::SelectAgent { index } => {
            if *index > 0 {
                *index -= 1;
            }
        }
        Focus::BgList { index } => {
            if *index > 0 {
                *index -= 1;
            }
        }
        Focus::Overlay(Overlay::ModelPicker { index, .. })
        | Focus::Overlay(Overlay::EffortPicker { index })
        | Focus::Overlay(Overlay::SessionPicker { index, .. })
            if *index > 0 =>
        {
            *index -= 1;
        }
        _ => {}
    }
    vec![]
}

fn handle_scroll(state: &mut AppState, code: KeyCode) -> Vec<Action> {
    let in_panel = matches!(state.focus, Focus::Panel { .. });
    if in_panel {
        if let Focus::Panel {
            ref mut follow_paused,
            follow: true,
            ..
        } = state.focus
        {
            if code == KeyCode::PageUp {
                *follow_paused = true;
            } else if code == KeyCode::End {
                *follow_paused = false;
                state.panel.agent_transcript.scroll_to_bottom();
                return vec![];
            }
        }
        match code {
            KeyCode::PageUp => state.panel.agent_transcript.scroll_by(10, 100, 20),
            KeyCode::PageDown => state.panel.agent_transcript.scroll_by(-10, 100, 20),
            KeyCode::Home => state.panel.agent_transcript.scroll_by(1000, 100, 20),
            KeyCode::End => state.panel.agent_transcript.scroll_to_bottom(),
            _ => {}
        }
    } else {
        match code {
            KeyCode::PageUp => state.transcript.scroll_by(10, 1000, 25),
            KeyCode::PageDown => state.transcript.scroll_by(-10, 1000, 25),
            KeyCode::Home => state.transcript.scroll_by(10000, 1000, 25),
            KeyCode::End => state.transcript.scroll_to_bottom(),
            _ => {}
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

pub fn handle_mouse(state: &mut AppState, mouse: MouseEvent, layout: &ScreenLayout) -> Vec<Action> {
    state.dirty = true;
    let col = mouse.column;
    let row = mouse.row;

    let delta = match mouse.kind {
        MouseEventKind::ScrollDown => -3,
        MouseEventKind::ScrollUp => 3,
        _ => return vec![],
    };

    if layout.dialog.contains((col, row).into()) {
        state
            .transcript
            .scroll_by(delta, 1000, layout.dialog.height as usize);
    } else if layout.input.contains((col, row).into()) {
        let max_scroll = state
            .input
            .wrap_lines(layout.input.width)
            .len()
            .saturating_sub(layout.input.height as usize);
        if delta < 0 {
            state.input.input_scroll = (state.input.input_scroll + 1).min(max_scroll);
        } else {
            state.input.input_scroll = state.input.input_scroll.saturating_sub(1);
        }
    } else if let Some(panel) = layout.panel {
        if panel.contains((col, row).into()) {
            state
                .panel
                .agent_transcript
                .scroll_by(delta, 1000, panel.height as usize);
        }
    } else if layout.rail.contains((col, row).into()) {
        if delta < 0 {
            handle_navigate_down(state);
        } else {
            handle_navigate_up(state);
        }
    }

    vec![]
}

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
