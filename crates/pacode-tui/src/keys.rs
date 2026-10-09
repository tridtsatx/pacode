//! Key and mouse handling (spec §6, §7 and the mockup).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use pacode_types::state::PermissionDecision;
use pacode_types::{AgentId, PermissionRequest, Request, TranscriptKind};

use crate::binding::Action as KeyAction;
use crate::commands;
use crate::state::transcript::{CellKind, PermExplain};
use crate::state::{AppState, Focus, Overlay, PanelTarget, PendingCopy, PromptBatch};
use crate::ui::popup::{AcceptedCompletion, PopupId};

#[cfg(test)]
#[path = "keys_tests.rs"]
mod keys_tests;

/// Side effects requested by a key press.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Send(Request),
    SendSequence {
        requests: Vec<Request>,
    },
    ContinuePromptBatch {
        id: u64,
    },
    SwitchSession {
        attach: pacode_types::Attach,
    },
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
    /// Page of git history: `after` is the last held commit hash, `None` asks
    /// for the first page. Marks `state.git` as loading before the request
    /// flies, which a bare `Send` cannot do.
    GitLog {
        after: Option<String>,
    },
    /// `git show --stat` of one commit for the submenu's detail pane.
    GitShow {
        hash: String,
    },
    /// Patch of one commit for the submenu's diff panel.
    GitDiff {
        hash: String,
    },
    Quit,
}

pub fn handle_key(state: &mut AppState, key: KeyEvent, now: Instant) -> Vec<Action> {
    // Feed the double-Shift detector on the raw stream — a kitty tap arrives
    // as Press + Release, and the drop below removes the Release (§4.2). The
    // gate folds `Focus::Normal` and the `keys.double_shift` switch in.
    let armed = state.focus == Focus::Normal && state.config.keys.double_shift_enabled();
    let threshold = state.config.keys.double_shift_threshold_ms();
    let hub_fired = state.double_shift.observe(key, now, threshold, armed);

    // REPORT_EVENT_TYPES reports Release and Repeat alongside Press (§4.2).
    // Release is dropped — lifting a key must not re-run its action — while
    // Repeat is fed through on purpose: a held key auto-repeats its action,
    // which is how held arrows auto-scroll and held chars auto-type.
    if matches!(key.kind, KeyEventKind::Release) {
        return vec![];
    }

    let was_dirty = state.dirty;
    state.dirty = true;

    // 1. ctrl+c / ctrl+d (Cyrillic layout letters are mapped to their Latin key).
    let key = normalize_cyrillic_ctrl(key);

    // The second Shift tap — opens the menu hub (§4.2).
    if hub_fired {
        return crate::keys_menu::open(state, crate::ui::menu::MenuId::Root);
    }

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

    // ctrl+shift+c: copy the current selection; with no selection the prompt
    // text is what the reader most likely wants on the clipboard. Both paths
    // only stash the text — the event loop performs the copy off-thread.
    if state.keymap.action_for(key) == Some(KeyAction::CopySelection) {
        if state.selection.is_active() && !state.selection.is_empty() {
            state.selection.request_explicit_copy();
        } else if !state.input.is_empty() {
            state.pending_copy = Some(PendingCopy {
                text: state.input.text.clone(),
                auto: false,
            });
        }
        return vec![];
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('d')) {
        state.quit = true;
        return vec![Action::Quit];
    }
    // The menu base owns the keyboard while open (§3.4): `esc` peels a level
    // and `ctrl+c` closes the whole stack — it must not reach the interrupt
    // ladder below. `ctrl+d` above still quits, same as in legacy overlays.
    if matches!(state.focus, Focus::Overlay(Overlay::Menu { .. })) {
        let taken = std::mem::replace(&mut state.focus, Focus::Normal);
        let Focus::Overlay(Overlay::Menu { mut stack, mut ui }) = taken else {
            return vec![];
        };
        let actions = crate::keys_menu::handle_menu_key(state, &mut stack, &mut ui, key);
        if matches!(state.focus, Focus::Normal) && !stack.is_empty() {
            state.focus = Focus::Overlay(Overlay::Menu { stack, ui });
        }
        return actions;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        if let Some(cancel_tx) = state.running_bash.take() {
            let _ = cancel_tx.send(());
            return vec![];
        }
        if !state.input.is_empty() {
            state.input.text.clear();
            state.input.cursor = 0;
            state.input.vert_goal = None;
            state.input.history_index = None;
            state.input.draft.clear();
            state.ctrl_c_at = None;
            return vec![];
        }
        // Staged escalation (spec §2): while anything is still live, the first
        // press interrupts — the main turn via Interrupt, subagent-only work via
        // InterruptAll. A second press inside the 2 s window while something is
        // still live escalates to InterruptAll (idempotent, safe to repeat).
        // Double-tap exits only once nothing is running.
        let subs_live = state.rail.live_agents().next().is_some();
        if state.turn_active || subs_live {
            let in_window = state
                .ctrl_c_at
                .is_some_and(|prev| now.saturating_duration_since(prev) <= Duration::from_secs(2));
            let req = if in_window || !state.turn_active {
                Request::InterruptAll
            } else {
                Request::Interrupt
            };
            state.ctrl_c_at = Some(now);
            // Optimistic: the spinner stops now rather than when TurnEnded lands.
            state.turn_active = false;
            return vec![Action::Send(req)];
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

    // 1.9 The menu hub via ctrl+, — double-Shift is the other entry (§4.2).
    if action == Some(KeyAction::Menu) {
        log::debug!("open overlay: menu hub");
        return crate::keys_menu::open(state, crate::ui::menu::MenuId::Root);
    }

    // 2. Sessions menu via ctrl+p
    if action == Some(KeyAction::SessionPicker) {
        log::debug!("open menu: sessions");
        return crate::keys_menu::open(state, crate::ui::menu::MenuId::Sessions);
    }

    // 3. Shift+Tab: cycle permission mode; plain Tab: autocomplete slash command / @ / bash
    if action == Some(KeyAction::CycleMode) {
        let next_mode = state.mode().next();
        return vec![Action::Send(Request::SetMode(next_mode))];
    }

    // Alt+Z: session ZDR toggle (no-op unless `zdr = "session"`).
    if action == Some(KeyAction::ToggleZdr) {
        return toggle_zdr(state);
    }

    // Alt+L: the notice lock. The switch lives in the daemon — the queue it
    // guards is the daemon's injection queue — so the key sends a request and
    // waits for `Event::NoticeLockChanged` to say what happened. Flipping a
    // local flag here would draw a badge for a lock that holds nothing.
    if action == Some(KeyAction::ToggleLock) {
        let on = !state.notice_lock.locked();
        log::debug!("notice lock: asking for {}", if on { "on" } else { "off" });
        return vec![Action::Send(Request::SetNoticeLock(on))];
    }
    if key.code == KeyCode::Tab && !key.modifiers.contains(KeyModifiers::SHIFT) {
        if let Some(popup) = crate::ui::popup::resolve_current(state)
            && let Some(accepted) = popup.accepted()
        {
            accept_completion(state, accepted);
        }
        return vec![];
    }

    // 4. Escape: peel layers one by one
    if action == Some(KeyAction::Cancel) {
        if let Some(popup) = crate::ui::popup::resolve_current(state) {
            match popup.id() {
                PopupId::At => {
                    state.input.at_closed = true;
                    return vec![];
                }
                PopupId::Bash => {
                    state.input.bash_complete_closed = true;
                    return vec![];
                }
                // A reading, not a chooser: Escape closes the page and there is
                // nothing underneath to peel back to.
                PopupId::Analyze | PopupId::Settings | PopupId::ModelPalette => {
                    state.focus = Focus::Normal;
                    state.dirty = true;
                    return vec![];
                }
                PopupId::Slash
                | PopupId::PromptQueue
                | PopupId::Menu
                | PopupId::EffortPicker
                | PopupId::ModePicker
                | PopupId::QuestionPicker
                | PopupId::ThemePicker
                | PopupId::RailOverlay => {}
            }
        }
        if state.config.ui.vim && state.focus == Focus::Normal {
            // Vim owns esc on the prompt (insert → normal, drop a pending
            // count); a mouse selection is not the prompt's and clears on any
            // esc. A panel left open under vim's normal mode is closed by the
            // layer ladder — and esc still does vim's part, dropping a pending
            // count or operator.
            state.selection.clear();
            if state.vim.mode == crate::state::VimMode::Normal && state.panel.target.is_some() {
                state.vim.pending.clear();
                state.vim.count = None;
                return handle_esc(state);
            }
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
        return crate::keys_menu::open(state, crate::ui::menu::MenuId::Files);
    }

    // 5.6 Plan & agents overlay. Below 80 columns the rail is not drawn at all
    // (spec §8), so this overlay is the only way to reach the plan and the agent
    // list there; above that width it is a shortcut to the same content.
    if action == Some(KeyAction::RailOverlay) {
        open_rail_overlay(state);
        state.dirty = true;
        return vec![];
    }

    // 6. Navigation: alt+down/ctrl+j (next agent/task), alt+up/ctrl+k (prev agent/task)
    // With the rail hidden there is nothing on screen to move a selection through,
    // so agent navigation opens the overlay that holds it instead.
    if action == Some(KeyAction::NextAgent) {
        if rail_is_hidden(state) {
            open_rail_overlay(state);
            state.dirty = true;
            return vec![];
        }
        return handle_navigate_down(state);
    }
    if action == Some(KeyAction::PrevAgent) {
        if rail_is_hidden(state) {
            open_rail_overlay(state);
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

    // 7.5 ctrl+o — expand the newest tool cell whose body hides lines, or
    // collapse it back. A click does the same for the cell under the pointer.
    if action == Some(KeyAction::ExpandOutput) {
        let transcript = if state.agent_replaces_dialog() {
            &mut state.panel.agent_transcript
        } else {
            &mut state.transcript
        };
        if transcript.toggle_last_expandable() {
            state.dirty = true;
        }
        return vec![];
    }

    // 8. Interactive permission prompt when input is empty: y / a / n / e
    if state.input.is_empty()
        && let Some(pending_req) = find_pending_permission(state)
    {
        // 'e' asks the daemon's detached explainer about the live request.
        // It never resolves the permission, so the verdict keys below stay
        // armed while the answer is in flight — the prompt flips to
        // "explain requested…" immediately for feedback.
        if key.code == KeyCode::Char('e') {
            let explain = PermExplain::Requested;
            state.transcript.set_perm_explain(&pending_req.id, &explain);
            state
                .panel
                .agent_transcript
                .set_perm_explain(&pending_req.id, &explain);
            return vec![Action::Send(Request::ExplainPermission {
                permission: pending_req.id,
            })];
        }
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
                // 'n' with no comment: a plain user refusal.
                reason: None,
            })];
        }
    }

    // 8.5 Vim mode handling on normal prompt
    if state.config.ui.vim && state.focus == Focus::Normal {
        match crate::state::vim::handle(&mut state.session.vim, &mut state.session.input, key) {
            crate::state::vim::VimEffect::Consumed => {
                // Vim moves/edits write `input.cursor` directly, bypassing
                // the InputState methods that reset the sticky goal column.
                state.input.vert_goal = None;
                return vec![];
            }
            crate::state::vim::VimEffect::Submit => return submit_prompt(state, now),
            crate::state::vim::VimEffect::PassThrough => {}
        }
    }

    // 9. Single keys in empty prompt: '.', 's', 'k'
    if state.input.is_empty() && (!state.config.ui.vim || state.focus != Focus::Normal) {
        if action == Some(KeyAction::BgList) {
            return crate::keys_menu::open(state, crate::ui::menu::MenuId::Background);
        }
        if action == Some(KeyAction::Help) {
            return crate::keys_menu::open(state, crate::ui::menu::MenuId::Help);
        }
        if action == Some(KeyAction::StopAgent)
            && let Focus::Panel {
                target: PanelTarget::Agent(ref id),
                ..
            } = state.focus
        {
            return vec![Action::Send(Request::StopAgent(id.clone()))];
        }
        if action == Some(KeyAction::KillTask)
            && let Focus::Panel {
                target: PanelTarget::Task(ref id),
                ..
            } = state.focus
        {
            return vec![Action::Send(Request::KillTask(id.clone()))];
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
        if state.turn_boundary_pending || state.user_msgs_in_flight > 0 {
            if state.input.is_empty() {
                return vec![];
            }
            return queue_prompt(state, now);
        }
        if state.input.is_empty() {
            return submit_prompt(state, now);
        }
        if !state.turn_active {
            return submit_prompt(state, now);
        }
        // Push the running turn out to a background agent first, so it keeps
        // working on its own transcript, then start the new message on the main
        // agent that takes its place. Nothing in flight is discarded.
        let text = state.input.take();
        state.pasted_images.mark_submitted(&text);
        return vec![Action::SendSequence {
            requests: vec![
                Request::DetachTurn,
                Request::UserMessage {
                    text,
                    images: Vec::new(),
                    message_id: None,
                },
            ],
        }];
    }

    // 10. Enter key
    if action == Some(KeyAction::Newline) {
        state.input.insert_char('\n');
        return vec![];
    }

    if action == Some(KeyAction::Submit) {
        if let Some(popup) = crate::ui::popup::resolve_current(state) {
            if popup.id() == PopupId::At
                && let Some(accepted) = popup.accepted()
            {
                accept_completion(state, accepted);
                return vec![];
            }
            if popup.id() == PopupId::Slash {
                let query = state.input.text.strip_prefix('/').unwrap_or("");
                if commands::find_command(query).is_none()
                    && let Some(accepted) = popup.accepted()
                {
                    accept_completion(state, accepted);
                    return vec![];
                }
            }
        }

        if !state.input.is_empty() {
            if state.turn_active || state.turn_boundary_pending || state.user_msgs_in_flight > 0 {
                return queue_prompt(state, now);
            }
            return submit_prompt(state, now);
        }

        if !state.turn_active
            && !state.turn_boundary_pending
            && state.user_msgs_in_flight == 0
            && state.has_retry_request()
        {
            return submit_prompt(state, now);
        }

        if !state.turn_active
            && !state.turn_boundary_pending
            && state.user_msgs_in_flight == 0
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
            Focus::Overlay(Overlay::EffortPicker { index }) => {
                if let Some(effort) = pacode_types::model::Effort::ALL.get(*index) {
                    let eff = *effort;
                    // Greyed levels are unselectable: refuse and keep the
                    // picker open (spec §6).
                    if !state.effort_supported(eff) {
                        state.push_notice_with_level(
                            pacode_types::ToastLevel::Warn,
                            format!("effort {eff} is not supported by the current model"),
                        );
                        return vec![];
                    }
                    log::debug!("close overlay: EffortPicker");
                    state.focus = Focus::Normal;
                    return vec![Action::Send(Request::SetEffort(eff))];
                }
            }
            Focus::Normal
            | Focus::Panel { .. }
            | Focus::Overlay(
                Overlay::ModePicker { .. }
                | Overlay::QuestionPicker
                | Overlay::RailOverlay { .. }
                | Overlay::ThemePicker { .. }
                // A reading has no selection to confirm, and the settings page
                // and the model palette handle their own keys; Enter does
                // nothing here.
                | Overlay::Analyze { .. }
                | Overlay::Settings { .. }
                | Overlay::ModelPalette { .. }
                // Unreachable — an open menu intercepts Enter earlier — but
                // the match stays exhaustive.
                | Overlay::Menu { .. },
            ) => {}
        }
        return vec![];
    }

    // 10.9 Shift+Up/Down scroll the multi-line prompt itself (spec §4); plain
    // arrows below still drive history and popups.
    if state.focus == Focus::Normal && key.modifiers.contains(KeyModifiers::SHIFT) {
        match key.code {
            KeyCode::Up => {
                state.input.input_scroll = state.input.input_scroll.saturating_sub(1);
                return vec![];
            }
            KeyCode::Down => {
                let max = input_max_scroll(state);
                state.input.input_scroll = (state.input.input_scroll + 1).min(max);
                return vec![];
            }
            _ => {}
        }
    }

    // 10.95 Readline-style prompt editing on modifier chords: word jumps,
    // whole-input boundaries, logical-line delete. These resolve before the
    // plain-arrow handlers below — ctrl+arrow shares the Up/Down/Left/Right
    // key codes with history browsing and single-step movement.
    if action == Some(KeyAction::MoveWordLeft) {
        state.input.move_word_left();
        return vec![];
    }
    if action == Some(KeyAction::MoveWordRight) {
        state.input.move_word_right();
        return vec![];
    }
    if action == Some(KeyAction::MoveStart) {
        state.input.move_to_start();
        return vec![];
    }
    if action == Some(KeyAction::MoveEnd) {
        state.input.move_to_end();
        return vec![];
    }
    if action == Some(KeyAction::DeleteLine) {
        state.input.delete_line();
        return vec![];
    }

    // 11. Up / Down arrows for history / slash popup / @ popup / bash completion
    if key.code == KeyCode::Up {
        if move_popup_selection(state, -1) {
            return vec![];
        }
        if state.input.text.starts_with('!') {
            state.input.bash_history_up();
            return vec![];
        }
        if !(state.input.text.starts_with('/') && !state.input.text.contains(' ')) {
            if state.input.is_empty() || state.input.history_index.is_some() {
                state.input.history_up();
            } else {
                let (input_w, _) = input_view_size(state);
                if !state.input.move_up(input_w) {
                    state.input.history_up();
                }
            }
        }
        return vec![];
    }
    if key.code == KeyCode::Down {
        if move_popup_selection(state, 1) {
            return vec![];
        }
        if state.input.text.starts_with('!') {
            state.input.bash_history_down();
            return vec![];
        }
        if !(state.input.text.starts_with('/') && !state.input.text.contains(' ')) {
            if state.input.history_index.is_some() {
                state.input.history_down();
            } else if !state.input.is_empty() {
                let (input_w, _) = input_view_size(state);
                state.input.move_down(input_w);
            }
        }
        return vec![];
    }

    // 12. Standard editing bindings
    if action == Some(KeyAction::ClearInput) {
        state.input.text.clear();
        state.input.cursor = 0;
        state.input.vert_goal = None;
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

/// Alt+Z. Only `zdr = "session"` toggles; `always`/`off` are config-forced.
/// Enabling requires the current route to be `zdr_capable` — otherwise the
/// model palette opens filtered to capable routes, and Enter there sends
/// `SetModel` then `SetZdr(true)` in order; Esc keeps ZDR off.
fn toggle_zdr(state: &mut AppState) -> Vec<Action> {
    if state.config.zdr != pacode_types::ZdrMode::Session {
        return vec![];
    }
    let Some(meta) = state.meta.as_ref() else {
        return vec![];
    };
    // No optimistic `meta.zdr` write: a refused `SetZdr` emits no
    // `SessionUpdated` to undo it, so the badge waits for the daemon's.
    if meta.zdr {
        return vec![Action::Send(Request::SetZdr(false))];
    }
    let capable = state
        .model_flags
        .get(&meta.model.to_string())
        .is_some_and(|f| f.zdr_capable);
    if capable {
        vec![Action::Send(Request::SetZdr(true))]
    } else {
        crate::keys_palette::open(state, crate::keys_palette::Opening::Zdr)
    }
}

fn move_popup_selection(state: &mut AppState, delta: i32) -> bool {
    let Some((id, total)) =
        crate::ui::popup::resolve_current(state).map(|popup| (popup.id(), popup.window().total))
    else {
        return false;
    };
    if total == 0 {
        return false;
    }
    let current = match id {
        PopupId::At => state.input.at_index % total,
        PopupId::Bash => state.input.bash_complete_index % total,
        PopupId::Slash => state.input.slash_index % total,
        PopupId::PromptQueue
        | PopupId::Menu
        | PopupId::EffortPicker
        | PopupId::ModePicker
        | PopupId::QuestionPicker
        | PopupId::ThemePicker
        | PopupId::Analyze
        | PopupId::Settings
        | PopupId::ModelPalette
        | PopupId::RailOverlay => return false,
    };
    let next = if delta < 0 {
        (current + total - 1) % total
    } else {
        (current + 1) % total
    };
    match id {
        PopupId::At => state.input.at_index = next,
        PopupId::Bash => state.input.bash_complete_index = next,
        PopupId::Slash => state.input.slash_index = next,
        _ => {}
    }
    true
}

fn accept_completion(state: &mut AppState, accepted: AcceptedCompletion) {
    match accepted {
        AcceptedCompletion::At {
            at_byte_index,
            cursor_byte,
            candidate,
        } => {
            let prefix = state.input.text[..at_byte_index].to_string();
            let suffix = state.input.text[cursor_byte..].to_string();
            let insert = format!("@{}", candidate.path);
            state.input.text = format!("{prefix}{insert}{suffix}");
            state.input.cursor = prefix.chars().count() + insert.chars().count();
            state.input.vert_goal = None;
            if candidate.is_dir {
                state.input.at_closed = false;
                state.input.at_index = 0;
            } else {
                state.input.insert_char(' ');
                state.input.at_closed = true;
            }
        }
        AcceptedCompletion::Bash { replacement } => {
            state.input.text = format!("!{replacement}");
            state.input.cursor = state.input.text.chars().count();
            state.input.vert_goal = None;
        }
        AcceptedCompletion::Slash { name } => {
            state.input.text = format!("/{name} ");
            state.input.cursor = state.input.text.chars().count();
            state.input.vert_goal = None;
            state.input.slash_index = 0;
        }
    }
}

fn queue_prompt(state: &mut AppState, now: Instant) -> Vec<Action> {
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
    vec![]
}

fn submit_prompt(state: &mut AppState, now: Instant) -> Vec<Action> {
    if state.turn_boundary_pending || state.user_msgs_in_flight > 0 {
        if state.input.is_empty() {
            return vec![];
        }
        return queue_prompt(state, now);
    }
    if state.input.is_empty() {
        return state
            .take_retry_batch()
            .map(prompt_batch_action)
            .into_iter()
            .collect();
    }
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
        }
        return vec![Action::RunBashCaptured { command: cmd }];
    }
    vec![Action::Send(Request::UserMessage {
        text,
        images: Vec::new(),
        message_id: None,
    })]
}

pub(crate) fn prompt_batch_action(batch: PromptBatch) -> Action {
    match batch.requests.as_slice() {
        [request] => Action::Send(request.clone()),
        _ => Action::SendSequence {
            requests: batch.requests,
        },
    }
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

fn open_rail_overlay(state: &mut AppState) {
    let agents = selectable_agents(state);
    let selected = state
        .panel_agent_target()
        .and_then(|target| match target {
            PanelTarget::Agent(id) => Some(id),
            PanelTarget::Task(_)
            | PanelTarget::McpServer(_)
            | PanelTarget::FileDiff { .. }
            | PanelTarget::CommitDiff { .. } => None,
        })
        .filter(|id| agents.contains(id))
        .or_else(|| agents.first().cloned())
        .unwrap_or_else(AgentId::main);
    state.focus = Focus::Overlay(Overlay::RailOverlay { selected });
}

/// Whether the rail is off screen, which below 80 columns it always is (spec §8).
fn rail_is_hidden(state: &AppState) -> bool {
    crate::layout::WidthTier::for_width(state.cols).rail_width() == 0
}

/// (width, height) of the prompt input rect, recomputed from `state.cols`/`rows`
/// the same way `layout::compute` derives it — key handling has no frame layout.
fn input_view_size(state: &AppState) -> (u16, u16) {
    let input = crate::layout::current(state).input;
    (input.width, input.height)
}

/// Max vertical scroll offset for a multi-line prompt (spec §4): total wrapped
/// lines minus the rows actually visible in the input rect.
fn input_max_scroll(state: &AppState) -> usize {
    let (w, h) = input_view_size(state);
    state.input.wrap_lines(w).len().saturating_sub(h as usize)
}
