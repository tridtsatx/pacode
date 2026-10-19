//! Event-stream continuity: detect lost events and ask for a fresh snapshot.
//!
//! The daemon numbers session events with a monotonic `seq`. A gap in that sequence, an
//! `Event::Resync` from the daemon, or `ClientEvent::Desynced` from the client all mean
//! events were dropped. The fix is one `GetSnapshot`, debounced to a single request in
//! flight. Events with `seq <= snapshot.seq` that arrive after a snapshot are stale.
//!
//! Owner: `AppState`. Four scalars, nothing grows, reset on disconnect.

use pacode_types::{Event, Request};

/// What to do with an incoming session event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Apply,
    /// Covered by a snapshot already applied.
    Stale,
    /// Connection-level notice, nothing to fold into the transcript.
    Consumed,
}

#[derive(Debug, Default)]
pub struct ResyncState {
    /// Highest session seq seen, or the snapshot's seq right after one. `None` until
    /// the first snapshot, when there is no baseline to compare against.
    max_seen: Option<u64>,
    /// `seq` of the last snapshot applied; events at or below it are stale.
    snapshot_seq: u64,
    /// A `GetSnapshot` was sent and its reply has not been applied yet.
    in_flight: bool,
    /// A resync is needed and has not been sent yet.
    wanted: bool,
}

/// Events whose `seq` does not come from the attached session's counter.
fn is_untracked(seq: u64, event: &Event) -> bool {
    seq == 0 || matches!(event, Event::LoginProgress { .. } | Event::AuthUpdated(_))
}

impl ResyncState {
    pub fn classify(&mut self, seq: u64, event: &Event) -> Verdict {
        if matches!(event, Event::Resync { .. }) {
            self.wanted = true;
            return Verdict::Consumed;
        }
        if is_untracked(seq, event) {
            return Verdict::Apply;
        }
        if seq <= self.snapshot_seq {
            return Verdict::Stale;
        }
        if let Some(max) = self.max_seen {
            if seq > max.saturating_add(1) {
                self.wanted = true;
            }
            self.max_seen = Some(max.max(seq));
        }
        Verdict::Apply
    }

    /// The client reported that it dropped events.
    pub fn note_desynced(&mut self) {
        self.wanted = true;
    }

    /// Next `GetSnapshot` to send, at most one in flight.
    pub fn take_request(&mut self) -> Option<Request> {
        if self.wanted && !self.in_flight {
            self.wanted = false;
            self.in_flight = true;
            Some(Request::GetSnapshot)
        } else {
            None
        }
    }

    /// A snapshot (requested or from a reconnect) replaced the state.
    pub fn on_snapshot(&mut self, seq: u64) {
        self.in_flight = false;
        // Events beyond the snapshot were applied before it and then wiped by it.
        if self.max_seen.is_some_and(|m| m > seq) {
            self.wanted = true;
        }
        self.snapshot_seq = seq;
        self.max_seen = Some(seq);
    }

    /// The `GetSnapshot` request failed; a later gap tries again.
    pub fn on_request_failed(&mut self) {
        self.in_flight = false;
    }

    /// Connection lost: sequences restart with the next attach.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
#[path = "resync_tests.rs"]
mod resync_tests;
