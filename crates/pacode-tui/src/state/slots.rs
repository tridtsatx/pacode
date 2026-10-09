//! Session slot table (spec §feature 2).
//!
//! Linux virtual terminals model: 9 numbered slots over ONE daemon connection,
//! one attached at a time. Inactive slots drop heavy runtime structures (transcript cells,
//! panel transcripts, caches) and retain only summary metadata and small counters for UI display.

use pacode_types::{SessionId, SessionSnapshot, SessionStatusKind};

use super::{AppState, Connection};

/// Total number of numbered session slots (1..=9).
pub const NUM_SLOTS: usize = 9;

/// Retained summary for an inactive session slot.
///
/// Memory cap: capped at small scalar fields (SessionId, title String <= 40 chars,
/// small scalar counters ~64 bytes total per slot). 9 slots max in `AppState.slots`,
/// taking < 1 KiB total retained memory across all inactive slots.
/// Invalidation / freeing: when leaving a slot, `leave_active_slot()` drops transcript cells,
/// panel transcript, task output lines, and caches, saving only this summary.
/// The summary is replaced on subsequent switches or freed when the client quits.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSlot {
    pub id: SessionId,
    pub title: String,
    pub turns: u32,
    pub context_tokens: u32,
    pub agents_count: usize,
    pub tasks_count: usize,
    /// Last daemon-reported status (`Event::SessionStatus` on the daemon-wide
    /// stream). `Done` persists until the user views the slot — the next
    /// attach snapshot reseeds it via [`status_from_snapshot`].
    pub status: SessionStatusKind,
}

/// Status seeded from an attach snapshot: waiting input wins over a running
/// turn; anything else is `Idle` — `Done` never survives an attach since the
/// session is on screen now. A live-but-idle subagent (spawned, waiting on
/// `TurnStarted`) still reads `Working`; the main agent is covered by
/// `snapshot.turn_active` alone.
pub fn status_from_snapshot(snapshot: &SessionSnapshot) -> SessionStatusKind {
    if !snapshot.pending_permissions.is_empty() || !snapshot.pending_questions.is_empty() {
        SessionStatusKind::NeedsInput
    } else if snapshot.turn_active
        || snapshot
            .agents
            .iter()
            .any(|a| a.status.is_live() && !a.id.is_main())
    {
        SessionStatusKind::Working
    } else {
        SessionStatusKind::Idle
    }
}

impl AppState {
    pub(crate) fn apply_session_status(
        &mut self,
        session: pacode_types::SessionId,
        status: pacode_types::SessionStatusKind,
    ) {
        for slot in self.slots.iter_mut().flatten() {
            if slot.id == session {
                slot.status = status;
                self.dirty = true;
            }
        }
    }

    /// Refresh the slot's retained summary from an attach snapshot. The status
    /// is reseeded too: an attach means the session is on screen, so a stale
    /// `Done` clears and live pending/turn state wins.
    pub fn record_slot(&mut self, index: usize, snapshot: &pacode_types::SessionSnapshot) {
        if index < NUM_SLOTS {
            self.slots[index] = Some(SessionSlot {
                id: snapshot.meta.id.clone(),
                title: snapshot.meta.title(),
                turns: snapshot.usage.turns,
                context_tokens: snapshot.usage.context_tokens,
                agents_count: snapshot.agents.len(),
                tasks_count: snapshot.tasks.len(),
                status: crate::state::slots::status_from_snapshot(snapshot),
            });
        }
    }

    /// The status the ACTIVE slot shows. Derived live: `SessionStatus` events
    /// describe sessions nobody is attached to, while this session's asks and
    /// turn are on screen — `Done` cannot apply to the slot being viewed.
    pub fn active_slot_status(&self) -> pacode_types::SessionStatusKind {
        use pacode_types::SessionStatusKind as Kind;
        if !self.questions.is_empty() || self.has_pending_permission() {
            Kind::NeedsInput
        } else if self.turn_active || self.rail.has_live_agents() {
            Kind::Working
        } else {
            Kind::Idle
        }
    }

    pub fn leave_active_slot(&mut self) {
        let current = self.active_slot;
        // Computed while the leaving session's asks/turn are still live state
        // — and quiet lands on `Idle`, not `Done`: the session was on screen,
        // so there is nothing unseen.
        let status = self.active_slot_status();
        if let Some(meta) = &self.meta {
            self.slots[current] = Some(SessionSlot {
                id: meta.id.clone(),
                title: meta.title(),
                turns: self.rail.usage.turns,
                context_tokens: self.rail.usage.context_tokens,
                agents_count: self.rail.agents.len(),
                tasks_count: self.rail.tasks.len(),
                status,
            });
        }
        self.session.reset(self.config.ui.transcript_cells);
        self.cancel_global_refresh();
        self.reconnect_snapshot_pending = false;
        self.event_session_epoch = 0;
        self.awaiting_snapshot = true;
        self.snapshot_rebase_allowed = false;
        self.pending_image_escapes.clear();
        self.ctrl_c_at = None;
        self.pending_attach = None;
        self.attach_in_flight = false;
        self.lists.git = crate::cache::ListHealth::default();
        self.connection = Connection::Connecting;
        self.dirty = true;
    }
}
