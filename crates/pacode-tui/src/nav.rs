//! Navigation helpers: agent selection, session slot switching, panel target changes, and rail navigation.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::{AgentId, Request};

use crate::binding::Action as KeyAction;
use crate::keys::Action;
use crate::state::{AppState, Focus, Overlay, PanelTarget};

/// Returns all rail agents including main, in the order the rail displays them (main first).
pub fn selectable_agents(state: &AppState) -> Vec<AgentId> {
    let mut list = Vec::new();
    if let Some(main) = state.rail.agents.iter().find(|a| a.id.is_main()) {
        list.push(main.id.clone());
    } else if !state.rail.agents.is_empty() {
        list.push(AgentId::main());
    }
    for agent in &state.rail.agents {
        if !agent.id.is_main() {
            list.push(agent.id.clone());
        }
    }
    list
}

/// Selects an agent by ID, clearing panel if main or opening panel if subagent.
pub fn select_agent_by_id(state: &mut AppState, id: &AgentId) -> Vec<Action> {
    if id.is_main() {
        state.focus = Focus::Normal;
        state.panel.target = None;
        vec![]
    } else {
        state.focus = Focus::Panel {
            target: PanelTarget::Agent(id.clone()),
            follow: false,
            follow_paused: false,
        };
        state.panel.target = Some(PanelTarget::Agent(id.clone()));
        vec![Action::LoadPanel]
    }
}

/// Selects an agent by index in `selectable_agents(state)`.
pub fn select_agent_at_index(state: &mut AppState, index: usize) -> Vec<Action> {
    let agents = selectable_agents(state);
    if let Some(id) = agents.get(index).cloned() {
        select_agent_by_id(state, &id)
    } else {
        vec![]
    }
}

/// Switches session slot (1..=9, 0-indexed).
pub fn switch_to_session_slot(state: &mut AppState, slot_idx: usize) -> Vec<Action> {
    if slot_idx >= crate::state::NUM_SLOTS || slot_idx == state.active_slot {
        return vec![];
    }

    state.leave_active_slot();
    state.active_slot = slot_idx;

    let attach = if let Some(ref slot) = state.slots[slot_idx] {
        pacode_types::Attach::Resume {
            session: slot.id.clone(),
        }
    } else {
        pacode_types::Attach::New {
            cwd: state.cwd(),
            model: None,
            effort: None,
            mode: None,
        }
    };

    let session_id_str = match &attach {
        pacode_types::Attach::Resume { session } => session.as_str(),
        pacode_types::Attach::New { .. } => "new",
        pacode_types::Attach::Latest { .. } => "latest",
    };
    log::debug!("switch session slot to {slot_idx} (session: {session_id_str})");

    vec![
        Action::Send(Request::Detach),
        Action::Send(Request::Attach(attach)),
    ]
}

pub fn overlay_name(o: &Overlay) -> &'static str {
    match o {
        Overlay::EffortPicker { .. } => "EffortPicker",
        Overlay::ModePicker { .. } => "ModePicker",
        Overlay::ModelPicker { .. } => "ModelPicker",
        Overlay::SessionPicker { .. } => "SessionPicker",
        Overlay::Files { .. } => "Files",
        Overlay::RailOverlay => "RailOverlay",
        Overlay::Help => "Help",
        Overlay::Import(_) => "Import",
        Overlay::McpPicker { .. } => "McpPicker",
        Overlay::PluginsPicker { .. } => "PluginsPicker",
        Overlay::KeysPicker { .. } => "KeysPicker",
        Overlay::ThemePicker { .. } => "ThemePicker",
        Overlay::ConfigPicker => "ConfigPicker",
    }
}

pub fn handle_esc(state: &mut AppState) -> Vec<Action> {
    state.selection.clear();
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
        Focus::Overlay(ref overlay) => {
            log::debug!("close overlay: {}", overlay_name(overlay));
            state.focus = Focus::Normal;
        }
        Focus::Normal => {}
    }
    vec![]
}

pub fn handle_follow(state: &mut AppState) -> Vec<Action> {
    match &state.focus {
        Focus::SelectAgent { index } => {
            let agents = selectable_agents(state);
            if let Some(id) = agents.get(*index) {
                if id.is_main() {
                    state.focus = Focus::Normal;
                    state.panel.target = None;
                    return vec![];
                }
                state.focus = Focus::Panel {
                    target: PanelTarget::Agent(id.clone()),
                    follow: true,
                    follow_paused: false,
                };
                state.panel.target = Some(PanelTarget::Agent(id.clone()));
                return vec![Action::LoadPanel];
            }
        }
        Focus::Panel {
            target: PanelTarget::Agent(id),
            ..
        } if id.is_main() => {
            state.focus = Focus::Normal;
            state.panel.target = None;
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
        Focus::Panel {
            target: PanelTarget::Task(_),
            ..
        }
        | Focus::Normal
        | Focus::BgList { .. }
        | Focus::Overlay(_) => {}
    }
    vec![]
}

pub fn handle_navigate_down(state: &mut AppState) -> Vec<Action> {
    let agents = selectable_agents(state);
    match &mut state.focus {
        Focus::SelectAgent { index } => {
            if !agents.is_empty() {
                *index = (*index + 1) % agents.len();
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
        Focus::Normal
        | Focus::Panel { .. }
        | Focus::Overlay(
            Overlay::Files { .. }
            | Overlay::ModePicker { .. }
            | Overlay::ConfigPicker
            | Overlay::McpPicker { .. }
            | Overlay::PluginsPicker { .. }
            | Overlay::Import(_)
            | Overlay::RailOverlay
            | Overlay::Help
            | Overlay::ThemePicker { .. }
            | Overlay::KeysPicker { .. },
        ) => {
            // Anywhere -> select first agent
            if !agents.is_empty() {
                state.focus = Focus::SelectAgent { index: 0 };
            }
        }
    }
    vec![]
}

pub fn handle_navigate_up(state: &mut AppState) -> Vec<Action> {
    let agents = selectable_agents(state);
    match &mut state.focus {
        Focus::SelectAgent { index } => {
            if !agents.is_empty() {
                *index = (*index + agents.len() - 1) % agents.len();
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
        Focus::Normal
        | Focus::Panel { .. }
        | Focus::Overlay(
            Overlay::ModelPicker { .. }
            | Overlay::EffortPicker { .. }
            | Overlay::SessionPicker { .. }
            | Overlay::Files { .. }
            | Overlay::ModePicker { .. }
            | Overlay::ConfigPicker
            | Overlay::McpPicker { .. }
            | Overlay::PluginsPicker { .. }
            | Overlay::Import(_)
            | Overlay::RailOverlay
            | Overlay::Help
            | Overlay::ThemePicker { .. }
            | Overlay::KeysPicker { .. },
        ) => {}
    }
    vec![]
}

pub fn handle_scroll(state: &mut AppState, action: KeyAction) -> Vec<Action> {
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
            KeyAction::ScrollUp => state.panel.agent_transcript.scroll_by(10),
            KeyAction::ScrollDown => state.panel.agent_transcript.scroll_by(-10),
            KeyAction::ScrollTop => state.panel.agent_transcript.scroll_to_top(),
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
            | KeyAction::DeleteWordForward
            | KeyAction::DeleteWordBack
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
            | KeyAction::SelectSession9
            | KeyAction::SubmitNow
            | KeyAction::RemoveQueued
            | KeyAction::ClearQueue
            | KeyAction::PasteImage
            | KeyAction::PasteClipboard => {}
        }
    } else {
        match action {
            KeyAction::ScrollUp => state.transcript.scroll_by(10),
            KeyAction::ScrollDown => state.transcript.scroll_by(-10),
            KeyAction::ScrollTop => state.transcript.scroll_to_top(),
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
            | KeyAction::DeleteWordForward
            | KeyAction::DeleteWordBack
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
            | KeyAction::SelectSession9
            | KeyAction::SubmitNow
            | KeyAction::RemoveQueued
            | KeyAction::ClearQueue
            | KeyAction::PasteImage
            | KeyAction::PasteClipboard => {}
        }
        if (action == KeyAction::ScrollUp || action == KeyAction::ScrollTop)
            && state.transcript.is_at_top()
            && state.can_load_history()
        {
            log::debug!("requesting history page from scroll at top");
            state.transcript.loading_history = true;
            return vec![Action::LoadHistory];
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
