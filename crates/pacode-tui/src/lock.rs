//! The client's view of the notice lock (`alt+l`).
//!
//! The switch, the backlog and the pacing all live in the daemon: an
//! orchestrator-bound notification is an `Injection` in the core, and the
//! client never sees one. So this module is a *view* of what the daemon
//! reports, plus the badge the user reads it from. It holds no queue and
//! makes no decision — [`LockView::apply`] is the only way into it, and the
//! only way out is the badge.
//!
//! The policy that matters — which notices may be held, the caps, the
//! coalescing, the paced release at a turn boundary — belongs next to the
//! injections it governs, in `pacode-core`, and is tested there. Nothing here
//! decides anything; a second switch in the client would be a lock that holds
//! nothing.
//!
//! The badge: `LOCK 3` in yellow while the daemon is holding, `… 3` in cyan
//! while it drains, ` (+N dropped)` in faint when a cap refused something. The
//! footer is the right home — the one piece of chrome on screen in every focus
//! and at every width; the rail disappears below 80 columns and the header
//! belongs to the session banner. A word rather than a glyph, so it survives
//! an ASCII terminal, a colourblind reader and a screen reader.

use pacode_types::SessionId;
use ratatui::text::Span;

mod badge;

pub use badge::LockBadge;

#[cfg(test)]
#[path = "lock_tests.rs"]
mod lock_tests;

/// What the daemon last reported for the attached session.
///
/// The default is a lock that is off and holding nothing: a client that has
/// seen no report yet agrees with the daemon, which starts unlocked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LockView {
    locked: bool,
    held: usize,
    dropped: u64,
}

impl LockView {
    /// Fold in one `Event::NoticeLockChanged`. The daemon is the only writer:
    /// `alt+l` sends the state it wants and waits to hear what happened, so a
    /// toggle the daemon refuses leaves the badge exactly as it was.
    ///
    /// `session` is unused on purpose — the client is attached to one session
    /// and only ever receives events for it, so a report is that session's.
    pub fn apply(&mut self, _session: SessionId, locked: bool, held: usize, dropped: u64) {
        self.locked = locked;
        self.held = held;
        self.dropped = dropped;
    }

    /// The state to ask the daemon for on the next `alt+l`.
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Injections the daemon is holding.
    pub fn held(&self) -> usize {
        self.held
    }

    /// Injections a cap refused since the daemon started. The daemon's
    /// `RuntimeStats` carries the same counter for a headless run.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// What the indicator should say, `None` when there is nothing to say —
    /// unlocked with an empty backlog is the state the user is in almost all
    /// the time, and a permanent badge would be noise.
    pub fn badge(&self) -> Option<LockBadge> {
        if !self.locked && self.held == 0 {
            return None;
        }
        Some(LockBadge {
            locked: self.locked,
            held: self.held,
            dropped: self.dropped,
        })
    }

    /// The badge as spans for the footer's right side; empty when there is
    /// nothing to show.
    pub fn footer_spans(&self, theme: &pacode_render::Theme) -> Vec<Span<'static>> {
        self.badge()
            .map_or_else(Vec::new, |badge| badge.spans(theme))
    }

    /// One deterministic line for the debug view.
    pub fn stats_line(&self) -> String {
        let plural = |n: usize| if n == 1 { "notice" } else { "notices" };
        let count = format!("{} {}", self.held, plural(self.held));
        match self.badge() {
            Some(badge) if badge.locked && self.dropped > 0 => {
                format!("notice lock: on, holding {count}, {} dropped", self.dropped)
            }
            Some(badge) if badge.locked => format!("notice lock: on, holding {count}"),
            Some(_) => format!("notice lock: off, draining {count}"),
            None => "notice lock: off".to_string(),
        }
    }
}
