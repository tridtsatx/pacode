//! Folding the client's event stream into state: connection transitions,
//! seq-tracked session events (and the buffer that holds them while a
//! snapshot is awaited), snapshots, and the seq-gap resync barrier.

use std::time::Instant;

use pacode_client::{ClientEvent, ClientEventContext};
use pacode_types::{Event, ToastLevel, TranscriptKind};

use super::{AppState, Connection, Focus, GitState, Overlay, SessionContext, events};

/// Minimum spacing between two `GetSnapshot` resync sends — a burst of
/// dropped daemon events arms one request, not one per detected seq gap.
pub const RESYNC_DEBOUNCE_MS: u64 = 500;

impl AppState {
    fn event_context_can_buffer(&self, context: &ClientEventContext) -> bool {
        if context.connection_epoch == 0 || context.connection_epoch != self.event_connection_epoch
        {
            return false;
        }
        if let Some(pending) = &self.pending_attach {
            if context.session_epoch != pending.client_session_epoch() {
                return false;
            }
            return match &pending.attach {
                pacode_types::Attach::Resume { session } => {
                    context.session.as_ref() == Some(session)
                }
                pacode_types::Attach::New { .. } | pacode_types::Attach::Latest { .. } => {
                    context.session.is_some()
                }
            };
        }
        self.meta.as_ref().is_some_and(|meta| {
            context.session.as_ref() == Some(&meta.id)
                && context.session_epoch == self.event_session_epoch
        })
    }

    fn apply_tracked_session_event(&mut self, seq: u64, event: Event, now: Instant) -> bool {
        if let Some(last) = self.last_event_seq
            && seq <= last
        {
            return false;
        }
        if self
            .last_event_seq
            .is_some_and(|last| seq != last.wrapping_add(1))
        {
            self.resync_want = true;
        }
        self.last_event_seq = Some(seq);
        self.observe_background_completion(&event);
        self.apply_event(seq, event, now);
        self.dirty = true;
        true
    }

    fn replay_buffered_session_events(&mut self, now: Instant) {
        let expected = ClientEventContext {
            connection_epoch: self.event_connection_epoch,
            session_epoch: self.event_session_epoch,
            session: self.meta.as_ref().map(|meta| meta.id.clone()),
        };
        let (mut buffered, overflowed) = self.session.take_buffered_events();
        buffered.sort_by_key(|buffered| (buffered.first_seq, buffered.seq));
        for buffered in buffered {
            if buffered.context != expected {
                continue;
            }
            if self.last_event_seq.is_some_and(|last| buffered.seq <= last) {
                continue;
            }
            if let Some(event) = buffered.event {
                match self.last_event_seq {
                    None => {
                        self.last_event_seq = Some(buffered.seq);
                        self.observe_background_completion(&event);
                        self.apply_event(buffered.seq, event, now);
                        self.dirty = true;
                    }
                    Some(last) if buffered.first_seq <= last => {
                        self.last_event_seq = Some(buffered.seq);
                        self.resync_want = true;
                    }
                    Some(last) if buffered.first_seq == last.wrapping_add(1) => {
                        self.last_event_seq = Some(buffered.seq);
                        self.observe_background_completion(&event);
                        self.apply_event(buffered.seq, event, now);
                        self.dirty = true;
                    }
                    Some(_) => {
                        self.apply_tracked_session_event(buffered.seq, event, now);
                    }
                }
            } else {
                self.last_event_seq = Some(buffered.seq);
            }
        }
        if overflowed {
            self.resync_want = true;
            self.dirty = true;
        }
    }

    /// Fold a client event into the state (spec §5 state table, §7 follow rules).
    /// Returns true when a redraw is needed (almost always).
    pub fn apply_client_event(&mut self, event: ClientEvent, now: Instant) -> bool {
        match event {
            ClientEvent::Connected { context, .. } => {
                if !self.observe_connection_context(&context) {
                    return false;
                }
                self.awaiting_snapshot = self.awaiting_snapshot
                    || self.meta.is_none()
                    || self.reconnect_snapshot_pending;
                self.connection = if self.meta.is_some() {
                    Connection::Connected
                } else {
                    Connection::Connecting
                };
                self.dirty = true;
                true
            }
            ClientEvent::Disconnected { context, reason } => {
                if !self.observe_connection_context(&context) {
                    return false;
                }
                self.cancel_global_refresh();
                self.reconnect_snapshot_pending = false;
                self.session.advance_deferred_epoch();
                self.session.cancel_deferred_work();
                self.session.fail_in_flight_user_messages();
                self.session.drop_pending_detach_turns();
                self.session.clear_buffered_events();
                self.invalidate_pending_attach_after_disconnect();
                self.resync_want = false;
                self.lists.git.in_flight = false;
                self.awaiting_snapshot = true;
                self.snapshot_rebase_allowed = false;
                self.connection = Connection::Disconnected { reason };
                self.dirty = true;
                true
            }
            ClientEvent::Reconnecting { context, attempt } => {
                if !self.observe_connection_context(&context) {
                    return false;
                }
                self.cancel_global_refresh();
                self.session.fail_in_flight_user_messages();
                self.session.drop_pending_detach_turns();
                self.session.drop_request_barriers();
                self.reconnect_snapshot_pending =
                    self.meta.is_some() && self.pending_attach.is_none();
                self.awaiting_snapshot = true;
                self.snapshot_rebase_allowed = false;
                self.connection = Connection::Reconnecting { attempt };
                self.dirty = true;
                true
            }
            ClientEvent::AttachFailed {
                context,
                attach,
                reason,
            } => self.apply_attach_failed(&context, &attach, reason),
            ClientEvent::Snapshot { context, snapshot } => {
                if !self.accepts_event_snapshot(&context, &snapshot) {
                    return false;
                }
                self.apply_snapshot(snapshot, now)
            }
            ClientEvent::ResyncNeeded { context } => {
                if !self.observe_connection_context(&context)
                    || self.pending_attach.is_some()
                    || !self.client_context_matches_current_session(&context)
                {
                    return false;
                }
                if !self.resync_in_flight
                    && (self.awaiting_snapshot || !matches!(self.connection, Connection::Connected))
                {
                    return false;
                }
                self.resync_want = true;
                self.dirty = true;
                true
            }
            ClientEvent::Event {
                context,
                seq,
                event,
            } => {
                if !self.observe_connection_context(&context) {
                    return false;
                }
                if event.is_global() {
                    self.apply_event(seq, event, now);
                    self.dirty = true;
                    return true;
                }
                if self.awaiting_snapshot {
                    if !self.event_context_can_buffer(&context) {
                        return false;
                    }
                    if self.event_is_interesting(&event) {
                        self.session.buffer_event(context, seq, event);
                    } else {
                        self.session.buffer_filtered_event(context, seq);
                    }
                    if self.resync_in_flight {
                        self.resync_want = true;
                    }
                    return false;
                }
                if !self.event_context_is_current(&context) {
                    return false;
                }
                self.apply_tracked_session_event(seq, event, now)
            }
        }
    }

    pub(crate) fn apply_snapshot(
        &mut self,
        snapshot: pacode_types::SessionSnapshot,
        now: Instant,
    ) -> bool {
        if snapshot.seq < self.last_event_seq.unwrap_or(0) && !self.snapshot_rebase_allowed {
            return false;
        }
        self.snapshot_rebase_allowed = false;
        let retry_resync = self.resync_want && self.resync_in_flight;
        self.connection = Connection::Connected;
        self.last_event_seq = Some(snapshot.seq);
        self.resync_want = false;
        self.resync_in_flight = false;
        self.resync_sent_at = None;
        if self.pending_attach.is_none() {
            self.attach_in_flight = false;
        }
        if let Some(msg) = pacode_config::take_migration_notice(&self.paths) {
            self.push_toast(ToastLevel::Info, msg, None, now);
        }
        let project_changed = self.cwd() != snapshot.meta.cwd;
        if project_changed {
            self.git = GitState::default();
            self.lists.git = crate::cache::ListHealth::default();
            self.files.clear_preview();
            self.input.at_comp = Default::default();
            self.input.bash_comp = Default::default();
        }
        self.started_at = now;
        self.meta = Some(snapshot.meta.clone());
        self.restore_stashed_queue(&snapshot.meta.id);
        self.record_slot(self.active_slot, &snapshot);
        self.settle_lost_clear(snapshot.transcript.is_empty(), now);
        self.transcript
            .reset(snapshot.transcript.clone(), snapshot.has_more_history);
        let header = crate::state::transcript::HeaderInfo {
            version: self.app_version.clone(),
            day: crate::ui::phrases::day_index(pacode_types::time::now_ms()),
            mascot: self.mascot,
            truecolor: self.truecolor,
        };
        self.transcript.set_header(header);
        for item in &snapshot.transcript {
            if let TranscriptKind::ToolCall {
                name,
                title,
                call_id,
                diff,
                ..
            } = &item.kind
            {
                let arg = crate::state::files::title_path_arg(name, title);
                let input = serde_json::json!({ "path": arg });
                self.files.observe_tool_item(name, &input, item.ts_ms);
                if let Some(diff) = diff {
                    self.files
                        .apply_diff(name, &input, call_id, *diff, item.ts_ms);
                }
            }
        }
        let active_turn = snapshot.active_turn.clone();
        let terminal_turn = snapshot.terminal_turn.clone();
        self.session.reconcile_turn_fence(
            snapshot.turn_active,
            active_turn.as_ref(),
            terminal_turn.as_ref(),
        );
        if snapshot.turn_active {
            if self.turn_started_at.is_none() {
                self.turn_started_at = Some(now);
            }
        } else {
            self.turn_started_at = None;
        }
        self.rail.plan = snapshot.plan;
        self.rail.replace_agents(snapshot.agents);
        self.rail.replace_tasks(snapshot.tasks);
        self.rail.cron_jobs = snapshot.cron_jobs;
        self.rail.monitors = snapshot.monitors;
        self.rail.usage = snapshot.usage;
        let turn_active = self.turn_active;
        self.rail.update_idle(turn_active, now);
        for (i, perm) in snapshot.pending_permissions.into_iter().enumerate() {
            let ts_ms = perm.created_at_ms;
            self.transcript.upsert_permission(
                perm,
                ts_ms,
                snapshot.seq.wrapping_add(1).wrapping_add(i as u64),
            );
        }
        if self.questions.replace(snapshot.pending_questions) {
            self.focus = Focus::Overlay(Overlay::QuestionPicker);
        } else if matches!(self.focus, Focus::Overlay(Overlay::QuestionPicker)) {
            self.focus = Focus::Normal;
        }
        self.awaiting_snapshot = false;
        self.replay_buffered_session_events(now);
        self.resync_want |= retry_resync;
        self.dirty = true;
        true
    }

    /// Fold a daemon event (`ClientEvent::Event`) into transcript/rail/panel/toasts.
    pub fn apply_event(&mut self, seq: u64, event: Event, now: Instant) {
        let main_status = match &event {
            Event::AgentUpdated(info) if info.id.is_main() => Some(info.status),
            _ => None,
        };
        events::apply_event(self, seq, event, now);
        if let Some(status) = main_status {
            self.session.observe_main_agent_status(status, now);
        }
    }

    pub fn resync_reply_is_current(
        &self,
        context: &SessionContext,
        client_context: &ClientEventContext,
    ) -> bool {
        self.resync_in_flight
            && self.pending_attach.is_none()
            && matches!(self.connection, Connection::Connected)
            && self.context_is_current(context)
            && self.client_context_matches_current_session(client_context)
    }

    pub fn release_resync_barrier(&mut self, retry: bool, now: Instant) {
        if self.pending_attach.is_some() {
            self.cancel_resync_for_attach();
            self.dirty = true;
            return;
        }
        self.resync_in_flight = false;
        self.awaiting_snapshot = false;
        self.snapshot_rebase_allowed = false;
        // No snapshot tells whether a clear that lost its reply went through:
        // the queued prompts stay.
        self.session.clear_outcome_unknown = false;
        self.session.reconcile_turn_boundary(self.turn_active);
        self.replay_buffered_session_events(now);
        self.resync_want |= retry && matches!(self.connection, Connection::Connected);
        if !retry || !matches!(self.connection, Connection::Connected) {
            self.resync_sent_at = None;
        }
        self.dirty = true;
    }

    /// Drain a seq-gap resync want into a `Request::GetSnapshot` — at most
    /// one in flight, and no resend within `RESYNC_DEBOUNCE_MS` of the last
    /// send. While no session is attached or the link is down the want is
    /// dropped instead: the (re)attach snapshot supersedes any resync.
    pub fn take_resync_request(&mut self, now: Instant) -> Option<pacode_types::Request> {
        if !self.resync_want {
            return None;
        }
        if self.pending_attach.is_some() {
            self.cancel_resync_for_attach();
            return None;
        }
        if self.meta.is_none() || !matches!(self.connection, Connection::Connected) {
            self.resync_want = false;
            self.resync_in_flight = false;
            self.resync_sent_at = None;
            return None;
        }
        if self.awaiting_snapshot {
            if self.resync_in_flight || !self.resync_want {
                return None;
            }
            self.awaiting_snapshot = false;
            self.snapshot_rebase_allowed = false;
        }
        if self.resync_in_flight
            || self.resync_sent_at.is_some_and(|t| {
                (now.saturating_duration_since(t).as_millis() as u64) < RESYNC_DEBOUNCE_MS
            })
        {
            return None;
        }
        self.resync_want = false;
        self.resync_from_prompt = false;
        self.session.fail_in_flight_user_messages();
        self.session.drop_pending_detach_turns();
        self.cancel_global_refresh();
        self.session.advance_deferred_epoch();
        self.session.cancel_deferred_work();
        self.awaiting_snapshot = true;
        self.snapshot_rebase_allowed = false;
        self.resync_in_flight = true;
        self.resync_sent_at = Some(now);
        Some(pacode_types::Request::GetSnapshot)
    }
}
