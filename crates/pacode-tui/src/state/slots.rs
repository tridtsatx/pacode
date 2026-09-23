//! Session slot table (spec §feature 2).
//!
//! Linux virtual terminals model: 9 numbered slots over ONE daemon connection,
//! one attached at a time. Inactive slots drop heavy runtime structures (transcript cells,
//! panel transcripts, caches) and retain only summary metadata and small counters for UI display.

use pacode_types::{SessionId, SessionSnapshot, SessionStatusKind};

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
