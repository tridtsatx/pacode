//! Navigation helpers: agent selection, session slot switching, panel target changes, and rail navigation.

use pacode_types::{AgentId, Request};

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

    vec![
        Action::Send(Request::Detach),
        Action::Send(Request::Attach(attach)),
    ]
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
        Focus::Overlay(_) => {
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
            | Overlay::ConfigPicker { .. }
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
    vec![]
}
