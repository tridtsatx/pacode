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
    /// Run bash command captured asynchronously.
    RunBashCaptured {
        command: String,
    },
    /// Run interactive full-screen bash command suspended.
    RunBashInteractive {
        command: String,
    },
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
        if let Some(cancel_tx) = state.running_bash.take() {
            let _ = cancel_tx.send(());
            return vec![];
        }
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
    log::trace!("key event {key:?} resolved to action {action:?}");

    // Paste image from clipboard via ctrl+alt+v
    if action == Some(KeyAction::PasteImage) {
        return crate::paste::handle_paste_image(state, true, now);
    }

    // ctrl+v / ctrl+shift+v: image when the clipboard holds one, otherwise text.
    // Terminals that translate ctrl+shift+v into a bracketed paste never reach this,
    // and that path already handles both cases.
    if action == Some(KeyAction::PasteClipboard) {
        return crate::paste::handle_paste_clipboard(state, now);
    }

    // 2. Session picker via ctrl+p
    if action == Some(KeyAction::SessionPicker) {
        log::debug!("open overlay: SessionPicker");
        state.focus = Focus::Overlay(Overlay::SessionPicker {
            query: String::new(),
            index: 0,
        });
        return vec![Action::Send(Request::ListSessions { limit: 50 })];
    }

    // 3. Shift+Tab: cycle permission mode; plain Tab: autocomplete slash command / @ / bash
    if action == Some(KeyAction::CycleMode) {
        let next_mode = state.mode().next();
        return vec![Action::Send(Request::SetMode(next_mode))];
    }
    if key.code == KeyCode::Tab && !key.modifiers.contains(KeyModifiers::SHIFT) {
        // First check @ completion
        let byte_cursor =
            crate::state::input::char_to_byte_index(&state.input.text, state.input.cursor);
        if !state.input.at_closed
            && let Some(q) = pacode_types::at_ref::find_active_query(&state.input.text, byte_cursor)
        {
            let cwd = state.cwd();
            let candidates = crate::at_complete::complete_at_path(&q.query, &cwd);
            if !candidates.is_empty() {
                let sel = state.input.at_index % candidates.len();
                let cand = candidates[sel].clone();
                let prefix = state.input.text[..q.at_byte_index].to_string();
                let suffix = state.input.text[byte_cursor..].to_string();
                let insert = format!("@{}", cand.path);
                let new_cursor = prefix.chars().count() + insert.chars().count();
                state.input.text = format!("{prefix}{insert}{suffix}");
                state.input.cursor = new_cursor;
                if cand.is_dir {
                    state.input.at_closed = false;
                    state.input.at_index = 0;
                } else {
                    state.input.insert_char(' ');
                    state.input.at_closed = true;
                }
                return vec![];
            }
        }

        // Second check bash mode Tab completion
        if state.input.text.starts_with('!') {
            let cwd = state.cwd();
            let char_cursor = state.input.cursor.saturating_sub(1);
            let candidates = crate::bash::complete::complete(
                &state.input.text[1..],
                char_cursor,
                &state.input.bash_history,
                &cwd,
            );
            if !candidates.is_empty() {
                let sel = state.input.bash_complete_index % candidates.len();
                let cand = &candidates[sel];
                state.input.text = format!("!{}", cand.replacement);
                state.input.cursor = state.input.text.chars().count();
                return vec![];
            }
        }

        if complete_slash_command(state) {
            return vec![];
        }
        return vec![];
    }

    // 4. Escape: peel layers one by one
    if action == Some(KeyAction::Cancel) {
        if !state.input.at_closed {
            let byte_cursor =
                crate::state::input::char_to_byte_index(&state.input.text, state.input.cursor);
            if pacode_types::at_ref::find_active_query(&state.input.text, byte_cursor).is_some() {
                state.input.at_closed = true;
                return vec![];
            }
        }
        if state.input.text.starts_with('!') && !state.input.bash_complete_closed {
            state.input.bash_complete_closed = true;
            return vec![];
        }
        if state.config.ui.vim && state.focus == Focus::Normal {
            // Handled by vim::handle on the normal prompt
        } else {
            return handle_esc(state);
        }
    }

    // 5. Follow shortcut: alt+b
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

    // 5.5 Files overlay: alt+f
    if action == Some(KeyAction::FilesOverlay) {
        log::debug!("open overlay: Files");
        state.focus = Focus::Overlay(Overlay::Files { index: 0 });
        return vec![];
    }

    // 5.6 Plan & agents overlay. Below 80 columns the rail is not drawn at all
    // (spec §8), so this overlay is the only way to reach the plan and the agent
    // list there; above that width it is a shortcut to the same content.
    if action == Some(KeyAction::RailOverlay) {
        state.focus = Focus::Overlay(Overlay::RailOverlay);
        state.dirty = true;
        return vec![];
    }

    // 6. Navigation: alt+down/ctrl+j (next agent/task), alt+up/ctrl+k (prev agent/task)
    // With the rail hidden there is nothing on screen to move a selection through,
    // so agent navigation opens the overlay that holds it instead.
    if action == Some(KeyAction::NextAgent) {
        if rail_is_hidden(state) {
            state.focus = Focus::Overlay(Overlay::RailOverlay);
            state.dirty = true;
            return vec![];
        }
        return handle_navigate_down(state);
    }
    if action == Some(KeyAction::PrevAgent) {
        if rail_is_hidden(state) {
            state.focus = Focus::Overlay(Overlay::RailOverlay);
            state.dirty = true;
            return vec![];
        }
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

    // 9.5 Prompt queue manipulation actions
    if action == Some(KeyAction::RemoveQueued) {
        if state.input.queue_pop_back().is_some() {
            state.dirty = true;
        }
        return vec![];
    }
    if action == Some(KeyAction::ClearQueue) {
        if !state.input.prompt_queue.is_empty() {
            state.input.queue_clear();
            state.dirty = true;
        }
        return vec![];
    }
    if action == Some(KeyAction::SubmitNow) {
        if state.input.is_empty() {
            return vec![];
        }
        if !state.turn_active {
            return submit_prompt(state);
        }
        // Push the running turn out to a background agent first, so it keeps
        // working on its own transcript, then start the new message on the main
        // agent that takes its place. Nothing in flight is discarded.
        let text = state.input.take();
        state.pasted_images.mark_submitted(&text);
        return vec![
            Action::Send(Request::DetachTurn),
            Action::Send(Request::UserMessage { text }),
        ];
    }

    // 10. Enter key
    if action == Some(KeyAction::Newline) {
        state.input.insert_char('\n');
        return vec![];
    }

    if action == Some(KeyAction::Submit) {
        let byte_cursor =
            crate::state::input::char_to_byte_index(&state.input.text, state.input.cursor);
        if !state.input.at_closed
            && let Some(q) = pacode_types::at_ref::find_active_query(&state.input.text, byte_cursor)
        {
            let cwd = state.cwd();
            let candidates = crate::at_complete::complete_at_path(&q.query, &cwd);
            if !candidates.is_empty() {
                let sel = state.input.at_index % candidates.len();
                let cand = candidates[sel].clone();
                let prefix = state.input.text[..q.at_byte_index].to_string();
                let suffix = state.input.text[byte_cursor..].to_string();
                let insert = format!("@{}", cand.path);
                let new_cursor = prefix.chars().count() + insert.chars().count();
                state.input.text = format!("{prefix}{insert}{suffix}");
                state.input.cursor = new_cursor;
                if cand.is_dir {
                    state.input.at_closed = false;
                    state.input.at_index = 0;
                } else {
                    state.input.insert_char(' ');
                    state.input.at_closed = true;
                }
                return vec![];
            }
        }

        if state.input.text.starts_with('/') && !state.input.text.contains(' ') {
            let query = &state.input.text[1..];
            let is_complete = commands::find_command(query).is_some();
            if !is_complete {
                let matches = commands::matching(query);
                if !matches.is_empty() {
                    complete_slash_command(state);
                    return vec![];
                }
            }
        }

        if !state.input.is_empty() {
            if state.turn_active {
                if state.input.queue_is_full() {
                    state.push_toast(
                        pacode_types::ToastLevel::Warn,
                        "Prompt queue full".to_string(),
                        Some(format!(
                            "Maximum {} prompts queued",
                            crate::state::input::PROMPT_QUEUE_CAP
                        )),
                        now,
                    );
                    return vec![];
                }
                let text = state.input.take();
                state.pasted_images.mark_submitted(&text);
                state.input.queue_push(text);
                return vec![];
            }
            return submit_prompt(state);
        }

        if !state.turn_active
            && !state.input.prompt_queue.is_empty()
            && let Some(req) = state.drain_prompt_queue()
        {
            return vec![Action::Send(req)];
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
                    log::debug!("close overlay: ModelPicker");
                    state.focus = Focus::Normal;
                    return vec![Action::Send(Request::SetModel(route))];
                }
            }
            Focus::Overlay(Overlay::EffortPicker { index }) => {
                if let Some(effort) = pacode_types::model::Effort::ALL.get(*index) {
                    let eff = *effort;
                    log::debug!("close overlay: EffortPicker");
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
                    log::debug!("close overlay: SessionPicker");
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
        return vec![];
    }

    // 11. Up / Down arrows for history / slash popup / @ popup / bash completion
    if key.code == KeyCode::Up {
        let byte_cursor =
            crate::state::input::char_to_byte_index(&state.input.text, state.input.cursor);
        if !state.input.at_closed
            && let Some(q) = pacode_types::at_ref::find_active_query(&state.input.text, byte_cursor)
        {
            let cwd = state.cwd();
            let candidates = crate::at_complete::complete_at_path(&q.query, &cwd);
            if !candidates.is_empty() {
                let n = candidates.len();
                state.input.at_index = (state.input.at_index + n - 1) % n;
                return vec![];
            }
        }
        if state.input.text.starts_with('!') {
            let cwd = state.cwd();
            let char_cursor = state.input.cursor.saturating_sub(1);
            let candidates = crate::bash::complete::complete(
                &state.input.text[1..],
                char_cursor,
                &state.input.bash_history,
                &cwd,
            );
            if !state.input.bash_complete_closed && !candidates.is_empty() {
                let n = candidates.len();
                state.input.bash_complete_index = (state.input.bash_complete_index + n - 1) % n;
                return vec![];
            }
            state.input.bash_history_up();
            return vec![];
        }
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
        let byte_cursor =
            crate::state::input::char_to_byte_index(&state.input.text, state.input.cursor);
        if !state.input.at_closed
            && let Some(q) = pacode_types::at_ref::find_active_query(&state.input.text, byte_cursor)
        {
            let cwd = state.cwd();
            let candidates = crate::at_complete::complete_at_path(&q.query, &cwd);
            if !candidates.is_empty() {
                let n = candidates.len();
                state.input.at_index = (state.input.at_index + 1) % n;
                return vec![];
            }
        }
        if state.input.text.starts_with('!') {
            let cwd = state.cwd();
            let char_cursor = state.input.cursor.saturating_sub(1);
            let candidates = crate::bash::complete::complete(
                &state.input.text[1..],
                char_cursor,
                &state.input.bash_history,
                &cwd,
            );
            if !state.input.bash_complete_closed && !candidates.is_empty() {
                let n = candidates.len();
                state.input.bash_complete_index = (state.input.bash_complete_index + 1) % n;
                return vec![];
            }
            state.input.bash_history_down();
            return vec![];
        }
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

    if action == Some(KeyAction::DeleteWordBack) {
        state.input.delete_word();
        return vec![];
    }

    if action == Some(KeyAction::DeleteWordForward) {
        state.input.delete_word_forward();
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

    crate::paste::cleanup_unreferenced_images(state);

    vec![]
}

pub use crate::nav::{
    handle_esc, handle_follow, handle_navigate_down, handle_navigate_up, handle_scroll,
    normalize_cyrillic_ctrl, select_agent_at_index, select_agent_by_id, selectable_agents,
    switch_to_session_slot,
};

fn complete_slash_command(state: &mut AppState) -> bool {
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
            return true;
        }
    }
    false
}

fn submit_prompt(state: &mut AppState) -> Vec<Action> {
    if !state.input.is_empty() {
        let text = state.input.take();
        state.pasted_images.mark_submitted(&text);
        if text.starts_with('/') {
            return commands::execute(state, &text);
        }
        if let Some(rest) = text.strip_prefix('!') {
            let cmd = rest.trim().to_string();
            if cmd.is_empty() {
                return vec![];
            }
            crate::bash::record_history(&mut state.input.bash_history, &cmd);
            if crate::bash::is_interactive(&cmd) {
                return vec![Action::RunBashInteractive { command: cmd }];
            } else {
                return vec![Action::RunBashCaptured { command: cmd }];
            }
        }
        return vec![Action::Send(Request::UserMessage { text })];
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

/// Whether the rail is off screen, which below 80 columns it always is (spec §8).
fn rail_is_hidden(state: &AppState) -> bool {
    crate::layout::WidthTier::for_width(state.cols).rail_width() == 0
}
