//! Attach lifecycle and connection/session contexts: the pending attach and
//! its retry, the epochs a reply or event must carry to still be current, and
//! the global list refresh bookkeeping that rides on them.

use std::sync::{Arc, atomic::AtomicU64};

use pacode_client::ClientEventContext;
use pacode_types::ToastLevel;

use super::{AppState, Connection, PendingAttach, SessionContext, UiContext};

fn attach_matches_snapshot(
    attach: &pacode_types::Attach,
    context: &ClientEventContext,
    snapshot: &pacode_types::SessionSnapshot,
) -> bool {
    if context.connection_epoch != 0 && context.session.as_ref() != Some(&snapshot.meta.id) {
        return false;
    }
    match attach {
        pacode_types::Attach::Resume { session } => &snapshot.meta.id == session,
        pacode_types::Attach::New { cwd, .. } | pacode_types::Attach::Latest { cwd } => {
            snapshot.meta.cwd == *cwd
        }
    }
}

impl AppState {
    pub fn session_context(&self) -> SessionContext {
        SessionContext {
            generation: self.session.generation,
            deferred_epoch: self.session.deferred_epoch,
            slot: self.active_slot,
            session: self.meta.as_ref().map(|meta| meta.id.clone()),
        }
    }

    pub fn ui_context(&self) -> UiContext {
        UiContext {
            generation: self.session.generation,
            deferred_epoch: self.session.deferred_epoch,
            slot: self.active_slot,
        }
    }

    pub fn ui_context_is_current(&self, context: &UiContext) -> bool {
        context.generation == self.session.generation
            && context.deferred_epoch == self.session.deferred_epoch
            && context.slot == self.active_slot
    }

    pub fn pending_attach_token(&self) -> Option<u64> {
        self.pending_attach.as_ref().map(|pending| pending.token)
    }

    pub fn set_pending_attach_client_epoch(&mut self, epoch: u64) -> Option<u64> {
        let pending = self.pending_attach.as_mut()?;
        pending.set_client_session_epoch(epoch);
        Some(pending.token)
    }

    pub fn pending_attach_epoch_signal(&self) -> Option<Arc<AtomicU64>> {
        self.pending_attach
            .as_ref()
            .map(PendingAttach::epoch_signal)
    }

    pub fn begin_global_refresh(&mut self, count: usize) -> bool {
        if count == 0 || self.global_refresh_in_flight {
            return false;
        }
        self.global_refresh_in_flight = true;
        self.global_refresh_remaining = count;
        true
    }

    pub fn finish_global_refresh_request(&mut self) {
        if !self.global_refresh_in_flight {
            return;
        }
        self.global_refresh_remaining = self.global_refresh_remaining.saturating_sub(1);
        if self.global_refresh_remaining == 0 {
            self.global_refresh_in_flight = false;
        }
    }

    pub fn cancel_global_refresh(&mut self) {
        self.global_refresh_in_flight = false;
        self.global_refresh_remaining = 0;
        self.global_refresh_context = None;
    }

    pub fn global_refresh_in_flight(&self) -> bool {
        self.global_refresh_in_flight
    }

    pub fn global_refresh_context_is_current(&self, context: UiContext) -> bool {
        self.global_refresh_context == Some(context)
    }

    pub fn record_global_refresh_context(&mut self, context: UiContext) {
        self.global_refresh_context = Some(context);
    }

    pub fn take_reconnect_snapshot_boundary(&mut self) -> bool {
        std::mem::take(&mut self.reconnect_snapshot_pending)
    }

    pub(super) fn cancel_resync_for_attach(&mut self) {
        self.resync_want = false;
        self.resync_from_prompt = false;
        self.resync_in_flight = false;
        self.resync_sent_at = None;
    }

    pub fn clear_pending_attach(&mut self, reason: String) {
        self.pending_attach = None;
        self.attach_in_flight = false;
        self.event_session_epoch = 0;
        self.awaiting_snapshot = false;
        self.snapshot_rebase_allowed = false;
        self.reconnect_snapshot_pending = false;
        self.resync_want = false;
        self.resync_from_prompt = false;
        self.resync_in_flight = false;
        self.resync_sent_at = None;
        self.cancel_global_refresh();
        self.session.clear_buffered_events();
        self.connection = Connection::Disconnected { reason };
        self.dirty = true;
    }

    pub fn request_attach_retry(&mut self) -> bool {
        if self.pending_attach.is_none() {
            self.attach_in_flight = false;
            return false;
        }
        self.cancel_resync_for_attach();
        if self.attach_in_flight {
            return false;
        }
        let Some(pending) = self.pending_attach.as_mut() else {
            return false;
        };
        if pending.retry_count >= 2 {
            pending.retry_requested = false;
            return false;
        }
        pending.retry_count += 1;
        pending.retry_requested = true;
        true
    }

    pub fn take_pending_attach_retry(&mut self) -> Option<PendingAttach> {
        if self.pending_attach.is_none() {
            self.attach_in_flight = false;
            return None;
        }
        self.cancel_resync_for_attach();
        if self.attach_in_flight {
            return None;
        }
        if !self
            .pending_attach
            .as_ref()
            .is_some_and(|pending| pending.retry_requested)
        {
            return None;
        }
        let client_connected = matches!(self.connection, Connection::Connected)
            || (matches!(self.connection, Connection::Connecting)
                && self.event_connection_epoch != 0);
        if !client_connected {
            if !matches!(self.connection, Connection::Connecting)
                && let Some(pending) = self.pending_attach.as_mut()
            {
                pending.retry_requested = false;
            }
            return None;
        }
        self.pending_attach.as_mut()?.retry_requested = false;
        self.attach_in_flight = true;
        self.pending_attach.clone()
    }

    pub fn context_is_current(&self, context: &SessionContext) -> bool {
        if self.meta.is_none() {
            return context.slot == self.active_slot
                && self
                    .pending_attach
                    .as_ref()
                    .is_some_and(|pending| pending.context == *context);
        }
        context.matches(
            self.session.generation,
            self.session.deferred_epoch,
            self.active_slot,
            self.meta.as_ref().map(|meta| &meta.id),
        )
    }

    pub fn observe_connection_context(&mut self, context: &ClientEventContext) -> bool {
        if context.connection_epoch == 0 {
            return true;
        }
        if context.connection_epoch < self.event_connection_epoch {
            return false;
        }
        if context.connection_epoch > self.event_connection_epoch {
            let initial_connection = self.event_connection_epoch == 0;
            self.event_connection_epoch = context.connection_epoch;
            self.session.clear_buffered_events();
            if !initial_connection {
                self.session.advance_deferred_epoch();
                self.session.cancel_deferred_work();
            }
            self.awaiting_snapshot = true;
            self.snapshot_rebase_allowed = false;
            self.resync_want = false;
            self.resync_from_prompt = false;
            self.resync_in_flight = false;
            self.resync_sent_at = None;
            if self.pending_attach.is_none() {
                self.attach_in_flight = false;
            }
            if !initial_connection {
                let attach_token = self
                    .pending_attach
                    .as_ref()
                    .map(|_| self.session.allocate_attach_token());
                if let (Some(token), Some(pending)) = (attach_token, &mut self.pending_attach) {
                    pending.token = token;
                    pending.retry_requested = true;
                }
            }
            self.reconnect_snapshot_pending =
                !initial_connection && self.meta.is_some() && self.pending_attach.is_none();
        }
        true
    }

    pub fn client_context_matches_current_session(&self, context: &ClientEventContext) -> bool {
        context.connection_epoch != 0
            && context.connection_epoch == self.event_connection_epoch
            && context.session_epoch == self.event_session_epoch
            && context.session.as_ref() == self.meta.as_ref().map(|meta| &meta.id)
    }

    pub fn event_context_is_current(&self, context: &ClientEventContext) -> bool {
        (context.connection_epoch == 0 && self.meta.is_none() && self.pending_attach.is_none())
            || (!self.awaiting_snapshot && self.client_context_matches_current_session(context))
    }

    pub(super) fn invalidate_pending_attach_after_disconnect(&mut self) {
        let Some(token) = self
            .pending_attach
            .as_ref()
            .map(|_| self.session.allocate_attach_token())
        else {
            self.attach_in_flight = false;
            return;
        };
        if let Some(pending) = &mut self.pending_attach {
            pending.token = token;
            pending.retry_requested = true;
        }
        self.attach_in_flight = false;
    }

    pub fn accepts_event_snapshot(
        &mut self,
        context: &ClientEventContext,
        snapshot: &pacode_types::SessionSnapshot,
    ) -> bool {
        if context.connection_epoch == 0 {
            if self.pending_attach.is_some() {
                return false;
            }
            return snapshot.seq >= self.last_event_seq.unwrap_or(0)
                && self
                    .meta
                    .as_ref()
                    .is_none_or(|meta| meta.id == snapshot.meta.id);
        }
        if !self.observe_connection_context(context) {
            return false;
        }
        if !self.awaiting_snapshot && snapshot.seq < self.last_event_seq.unwrap_or(0) {
            return false;
        }
        if let Some(pending) = &self.pending_attach {
            if pending.context.generation != self.session.generation
                || pending.context.slot != self.active_slot
                || context.session_epoch != pending.client_session_epoch()
                || !attach_matches_snapshot(&pending.attach, context, snapshot)
            {
                return false;
            }
            self.cancel_resync_for_attach();
            self.session.fail_in_flight_user_messages();
            self.pending_attach = None;
            self.attach_in_flight = false;

            self.accept_attach_context(context);
            return true;
        }
        let Some(meta) = &self.meta else {
            return false;
        };
        if context.session.as_ref() != Some(&meta.id)
            || context.session_epoch != self.event_session_epoch
        {
            return false;
        }
        self.accept_attach_context(context);
        true
    }

    pub fn accept_attach_context(&mut self, context: &ClientEventContext) {
        if context.connection_epoch != 0 {
            self.event_connection_epoch = context.connection_epoch;
        }
        self.event_session_epoch = context.session_epoch;
        self.awaiting_snapshot = false;
        self.snapshot_rebase_allowed = true;
        if self.pending_attach.is_none() {
            self.attach_in_flight = false;
        }
    }

    pub fn begin_attach(&mut self, attach: pacode_types::Attach) -> SessionContext {
        let context = self.session_context();
        let expected_epoch = self.event_session_epoch.wrapping_add(1);
        let token = self.session.allocate_attach_token();
        self.cancel_global_refresh();
        self.reconnect_snapshot_pending = false;
        self.session.clear_buffered_events();
        self.event_session_epoch = 0;
        self.awaiting_snapshot = true;
        self.snapshot_rebase_allowed = false;
        self.resync_want = false;
        self.resync_in_flight = false;
        self.resync_sent_at = None;
        self.attach_in_flight = false;
        self.pending_attach = Some(PendingAttach {
            context: context.clone(),
            attach,
            token,
            client_session_epoch: Arc::new(AtomicU64::new(expected_epoch)),
            retry_requested: false,
            retry_count: 0,
        });
        context
    }

    pub fn accepts_attach(
        &self,
        context: &SessionContext,
        client_context: &ClientEventContext,
        attach: &pacode_types::Attach,
        snapshot: &pacode_types::SessionSnapshot,
    ) -> bool {
        self.accepts_attach_with_token(
            context,
            self.pending_attach_token().unwrap_or_default(),
            client_context,
            attach,
            snapshot,
        )
    }

    pub fn accepts_attach_with_token(
        &self,
        context: &SessionContext,
        token: u64,
        client_context: &ClientEventContext,
        attach: &pacode_types::Attach,
        snapshot: &pacode_types::SessionSnapshot,
    ) -> bool {
        if !self.context_is_current(context) {
            return false;
        }
        let Some(pending) = &self.pending_attach else {
            return self
                .meta
                .as_ref()
                .is_some_and(|meta| meta.id == snapshot.meta.id)
                && attach_matches_snapshot(attach, client_context, snapshot);
        };
        if pending.token != token
            || pending.context != *context
            || pending.attach != *attach
            || !attach_matches_snapshot(attach, client_context, snapshot)
        {
            return false;
        }
        if client_context.connection_epoch != 0 {
            if client_context.connection_epoch < self.event_connection_epoch {
                return false;
            }
            if client_context.session_epoch != pending.client_session_epoch() {
                return false;
            }
        }
        true
    }

    pub(super) fn apply_attach_failed(
        &mut self,
        context: &ClientEventContext,
        attach: &pacode_types::Attach,
        reason: String,
    ) -> bool {
        if !self.observe_connection_context(context) {
            return false;
        }
        let relevant = self.pending_attach.as_ref().map_or_else(
            || {
                self.meta.as_ref().is_some_and(|meta| {
                    matches!(attach, pacode_types::Attach::Resume { session } if session == &meta.id)
                })
            },
            |pending| {
                let target_matches = match (&pending.attach, attach) {
                    (
                        pacode_types::Attach::New { .. } | pacode_types::Attach::Latest { .. },
                        pacode_types::Attach::Resume { .. },
                    ) => true,
                    _ => &pending.attach == attach,
                };
                target_matches
                    && (context.connection_epoch == 0
                        || (context.connection_epoch == self.event_connection_epoch
                            && context.session_epoch == pending.client_session_epoch()))
            },
        );
        if !relevant {
            return false;
        }

        self.clear_pending_attach(reason.clone());
        self.push_notice_with_level(ToastLevel::Error, format!("attach failed: {reason}"));
        self.attach_error = Some(pacode_client::ClientError::Daemon(reason));
        self.quit = true;
        true
    }
}
