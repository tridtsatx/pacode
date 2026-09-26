//! Navigation helpers: agent selection, session slot switching, panel target changes, and rail navigation.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::AgentId;

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

    let cwd = state.cwd();
    state.leave_active_slot();
    state.active_slot = slot_idx;

    let attach = if let Some(ref slot) = state.slots[slot_idx] {
        pacode_types::Attach::Resume {
            session: slot.id.clone(),
        }
    } else {
        pacode_types::Attach::New {
            cwd,
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
    state.begin_attach(attach.clone());

    vec![Action::SwitchSession { attach }]
}

pub fn overlay_name(o: &Overlay) -> &'static str {
    match o {
        Overlay::EffortPicker { .. } => "EffortPicker",
        Overlay::ModePicker { .. } => "ModePicker",
        Overlay::ModelPicker { .. } => "ModelPicker",
        Overlay::LoginPicker { .. } => "LoginPicker",
        Overlay::RailOverlay { .. } => "RailOverlay",
        Overlay::ThemePicker { .. } => "ThemePicker",
        Overlay::QuestionPicker => "QuestionPicker",
        // The level on screen names the menu in the log (§3.1).
        Overlay::Menu { stack, .. } => stack.current().map_or("Menu", |l| l.id.title()),
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
            state.panel.diff = None;
        }
        Focus::SelectAgent { .. } => {
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
            target:
                PanelTarget::Task(_)
                | PanelTarget::McpServer(_)
                | PanelTarget::FileDiff { .. }
                | PanelTarget::CommitDiff { .. },
            ..
        }
        | Focus::Normal
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
        Focus::Overlay(Overlay::ModelPicker { index, .. })
        | Focus::Overlay(Overlay::LoginPicker { index, .. })
        | Focus::Overlay(Overlay::EffortPicker { index }) => {
            *index += 1;
        }
        Focus::Normal
        | Focus::Panel { .. }
        | Focus::Overlay(
            Overlay::ModePicker { .. }
            | Overlay::QuestionPicker
            | Overlay::RailOverlay { .. }
            | Overlay::ThemePicker { .. }
            // Unreachable — an open menu intercepts keys before `nav` — but
            // the match stays exhaustive.
            | Overlay::Menu { .. },
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
        Focus::Overlay(Overlay::ModelPicker { index, .. })
        | Focus::Overlay(Overlay::LoginPicker { index, .. })
        | Focus::Overlay(Overlay::EffortPicker { index })
            if *index > 0 =>
        {
            *index -= 1;
        }
        Focus::Normal
        | Focus::Panel { .. }
        | Focus::Overlay(
            Overlay::ModelPicker { .. }
            | Overlay::LoginPicker { .. }
            | Overlay::EffortPicker { .. }
            | Overlay::ModePicker { .. }
            | Overlay::QuestionPicker
            | Overlay::RailOverlay { .. }
            | Overlay::ThemePicker { .. }
            | Overlay::Menu { .. },
        ) => {}
    }
    vec![]
}

pub fn handle_scroll(state: &mut AppState, action: KeyAction) -> Vec<Action> {
    let in_panel = matches!(state.focus, Focus::Panel { .. });
    if in_panel {
        // The MCP log and file-diff panels have no transcript —
        // `panel.scroll` is the offset from the tail (ui::panel); a diff
        // opens at `usize::MAX`, pinning its head.
        if matches!(
            state.focus,
            Focus::Panel {
                target: PanelTarget::McpServer(_)
                    | PanelTarget::FileDiff { .. }
                    | PanelTarget::CommitDiff { .. }
                    | PanelTarget::Task(_),
                ..
            }
        ) {
            if action == KeyAction::ScrollUp {
                state.panel.scroll = state.panel.scroll.saturating_add(10);
            } else if action == KeyAction::ScrollDown {
                state.panel.scroll = state.panel.scroll.saturating_sub(10);
            } else if action == KeyAction::ScrollTop {
                state.panel.scroll = usize::MAX;
            } else if action == KeyAction::ScrollBottom {
                state.panel.scroll = 0;
            }
            return vec![];
        }
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
            | KeyAction::MoveWordLeft
            | KeyAction::MoveWordRight
            | KeyAction::MoveStart
            | KeyAction::MoveEnd
            | KeyAction::DeleteLine
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
            | KeyAction::PasteClipboard
            | KeyAction::RailOverlay
            | KeyAction::ToggleZdr
            | KeyAction::ToggleLock
            | KeyAction::Menu
            | KeyAction::Help
            | KeyAction::ExpandOutput
            // Picker-scoped (§9.1): never reach the shared ladder — an open
            // picker overlay swallows its keys before this dispatch.
            | KeyAction::ModelPickerFavorite
            | KeyAction::ModelPickerZdr
            | KeyAction::ModelPickerRefresh => {}
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
            | KeyAction::MoveWordLeft
            | KeyAction::MoveWordRight
            | KeyAction::MoveStart
            | KeyAction::MoveEnd
            | KeyAction::DeleteLine
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
            | KeyAction::PasteClipboard
            | KeyAction::RailOverlay
            | KeyAction::ToggleZdr
            | KeyAction::ToggleLock
            | KeyAction::Menu
            | KeyAction::Help
            | KeyAction::ExpandOutput
            | KeyAction::ModelPickerFavorite
            | KeyAction::ModelPickerZdr
            | KeyAction::ModelPickerRefresh => {}
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
