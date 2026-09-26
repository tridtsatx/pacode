//! The notice lock (`alt+l`): hold the notifications the orchestrator would
//! otherwise receive, then let them out one at a time.
//!
//! A subagent finishing mid-turn re-drives its parent, and a finished
//! background task injects its result. Release five of those into a turn that
//! has already moved on and the model answers five questions nobody asked.
//! The lock parks them; releasing it paces them back in.
//!
//! # What is holdable
//!
//! [`Notice::AgentFinished`], [`Notice::TaskFinished`] and
//! [`Notice::SubagentReport`] — a subagent's turn ended, a background task
//! reached a terminal state, a subagent reported to its parent. Each names a
//! [`Source`], and that source is what the per-source cap counts.
//!
//! # What is never holdable
//!
//! [`Notice::PermissionRequest`], [`Notice::QuestionAsked`] and
//! [`Notice::InterruptAck`]. Each is something the turn — or the daemon — is
//! *waiting* on: the user is blocked until they answer, and a cancelled turn
//! stays a zombie until its ack lands. Holding a delivery of any of them hangs
//! the client, so [`Notice::is_holdable`] is the single definition and
//! [`NoticeLock::offer`] hands them straight back whatever the lock says.
//!
//! # Order and caps
//!
//! FIFO by arrival. Two caps stop one source from starving the rest:
//!
//! * [`PER_SOURCE_CAP`] entries per [`Source`]. On overflow the source's
//!   *oldest* pending entry is evicted — a newer notice from the same agent
//!   supersedes it — and the arrival is kept, so a burst that was going to be
//!   summarised anyway costs the queue one entry, not the notice.
//! * [`QUEUE_CAP`] entries overall. On overflow the *arrival* is dropped: the
//!   backlog is the stale half and it is about to be delivered.
//!
//! Both paths count into [`NoticeLock::dropped`], which the badge and the
//! stats line report. A quietly lossy queue is worse than a visible one.
//!
//! # Coalescing
//!
//! An arrival merges into a pending entry with the same [`Notice::slot`] —
//! same kind, same source, same payload. Ten identical "agent finished"
//! notices are one delivery carrying `count: 10`, not ten. The merge keeps
//! the entry's original queue position, so a merge never reorders the
//! backlog. Two different tasks' results carry two different sources and stay
//! two deliveries, and so do two agents that happen to report the same words:
//! the id is the part the orchestrator needs, so nothing merges across
//! sources.
//!
//! # Drain
//!
//! The first release is immediate — the common case is a single held notice
//! and the user has just unlocked, so added latency would be pure cost. Every
//! release after it waits [`DRAIN_INTERVAL_MS`] (250 ms). A release is not a
//! log line, it is input to a turn: 20 held notices landing together queue 20
//! re-drives. 250 ms keeps them visually sequential — the count changes
//! faster than a reader tracks it, slowly enough that the turn absorbs each
//! one — and clears a 20-notice backlog in five seconds.
//!
//! Re-locking pauses a drain in progress; unlocking again resumes whatever
//! part of the wait was left. While the lock is on, [`NoticeLock::wants_work`]
//! is false and no deadline is armed: an idle lock costs nothing.
//!
//! # State
//!
//! [`NoticeLock`] holds the switch itself — on/off, when it was toggled, the
//! backlog, the drop counter and the next release deadline. It does not decide
//! what a notice does once released: [`NoticeLock::offer`] and
//! [`NoticeLock::tick`] hand back what to deliver and the caller sends it.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use pacode_types::{AgentId, QuestionId, TaskId};
use ratatui::text::Span;

mod badge;

pub use badge::LockBadge;

#[cfg(test)]
#[path = "lock_tests.rs"]
mod lock_tests;

/// Delay between two releases after the first. See the module doc.
pub const DRAIN_INTERVAL_MS: u64 = 250;

/// Pending notices overall, across every source.
pub const QUEUE_CAP: usize = 64;

/// Pending notices from one [`Source`].
pub const PER_SOURCE_CAP: usize = 8;

/// The kind of a notice, without its payload. Coalescing keys on it, so two
/// payloads of different kinds never merge even from the same source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoticeKind {
    AgentFinished,
    TaskFinished,
    SubagentReport,
    PermissionRequest,
    QuestionAsked,
    InterruptAck,
}

/// Who a holdable notice came from. Borrowed, so comparing a backlog entry
/// against an arrival allocates nothing.
#[derive(Clone, Copy, Debug)]
pub enum Source<'a> {
    Agent(&'a AgentId),
    Task(&'a TaskId),
}

impl Source<'_> {
    /// Cross-lifetime equality — a derived `PartialEq` would tie both borrows
    /// to one lifetime, and the comparisons the queue needs span two.
    fn same_as(self, other: Source<'_>) -> bool {
        match self {
            Source::Agent(a) => match other {
                Source::Agent(b) => a == b,
                Source::Task(_) => false,
            },
            Source::Task(a) => match other {
                Source::Agent(_) => false,
                Source::Task(b) => a == b,
            },
        }
    }
}

/// One notification bound for the orchestrator.
///
/// The three holdable variants are what the lock queues; the other three are
/// what the user or the daemon is blocked on, and pass through untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    AgentFinished {
        agent: AgentId,
        name: String,
        answer: String,
    },
    TaskFinished {
        task: TaskId,
        label: String,
        exit_code: i32,
        tail: String,
    },
    SubagentReport {
        agent: AgentId,
        text: String,
    },
    PermissionRequest {
        title: String,
    },
    QuestionAsked {
        question: QuestionId,
        text: String,
    },
    InterruptAck {
        reason: String,
    },
}

/// The identity two notices must share to coalesce: same kind, same source,
/// same payload. `subject` is the payload itself, borrowed — it is what makes
/// "identical" mean identical rather than merely same-shaped.
#[derive(Clone, Copy, Debug)]
pub struct NoticeSlot<'a> {
    pub kind: NoticeKind,
    pub source: Source<'a>,
    pub subject: &'a str,
}

impl NoticeSlot<'_> {
    fn same_as(&self, other: &NoticeSlot<'_>) -> bool {
        self.kind == other.kind
            && self.source.same_as(other.source)
            && self.subject == other.subject
    }
}

impl Notice {
    pub fn kind(&self) -> NoticeKind {
        match self {
            Self::AgentFinished { .. } => NoticeKind::AgentFinished,
            Self::TaskFinished { .. } => NoticeKind::TaskFinished,
            Self::SubagentReport { .. } => NoticeKind::SubagentReport,
            Self::PermissionRequest { .. } => NoticeKind::PermissionRequest,
            Self::QuestionAsked { .. } => NoticeKind::QuestionAsked,
            Self::InterruptAck { .. } => NoticeKind::InterruptAck,
        }
    }

    /// Whether the lock may park this notice. False for everything the turn
    /// or the daemon is waiting on — see the module doc.
    pub fn is_holdable(&self) -> bool {
        match self {
            Self::AgentFinished { .. }
            | Self::TaskFinished { .. }
            | Self::SubagentReport { .. } => true,
            Self::PermissionRequest { .. }
            | Self::QuestionAsked { .. }
            | Self::InterruptAck { .. } => false,
        }
    }

    /// The queue entry a holdable notice came from; `None` for the kinds the
    /// lock never parks.
    pub fn source(&self) -> Option<Source<'_>> {
        match self {
            Self::AgentFinished { agent, .. } | Self::SubagentReport { agent, .. } => {
                Some(Source::Agent(agent))
            }
            Self::TaskFinished { task, .. } => Some(Source::Task(task)),
            Self::PermissionRequest { .. }
            | Self::QuestionAsked { .. }
            | Self::InterruptAck { .. } => None,
        }
    }

    /// The coalescing identity, or `None` when the notice is not holdable.
    pub fn slot(&self) -> Option<NoticeSlot<'_>> {
        match self {
            Self::AgentFinished { agent, answer, .. } => Some(NoticeSlot {
                kind: NoticeKind::AgentFinished,
                source: Source::Agent(agent),
                subject: answer,
            }),
            Self::TaskFinished { task, tail, .. } => Some(NoticeSlot {
                kind: NoticeKind::TaskFinished,
                source: Source::Task(task),
                subject: tail,
            }),
            Self::SubagentReport { agent, text, .. } => Some(NoticeSlot {
                kind: NoticeKind::SubagentReport,
                source: Source::Agent(agent),
                subject: text,
            }),
            Self::PermissionRequest { .. }
            | Self::QuestionAsked { .. }
            | Self::InterruptAck { .. } => None,
        }
    }
}

/// One notice on its way out, plus how many identical ones it stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub notice: Notice,
    /// 1 unless identical notices coalesced into this one.
    pub count: u32,
}

/// The lock: the switch, the backlog it holds, and the drain deadline.
#[derive(Debug, Default)]
pub struct NoticeLock {
    locked: bool,
    toggled_at: Option<Instant>,
    queue: VecDeque<Held>,
    dropped: u64,
    next_due: Option<Instant>,
}

impl NoticeLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Deliver or park one notice. `Some` is what the caller must send now;
    /// `None` means the lock kept it — or a cap refused it, which shows up in
    /// [`Self::dropped`].
    ///
    /// An unlocked lock never queues: the pacing exists to soften the release
    /// of a *held backlog*, not to reorder live traffic, so a notice arriving
    /// mid-drain goes straight out.
    pub fn offer(&mut self, notice: Notice) -> Option<Held> {
        if !self.locked {
            return Some(Held { notice, count: 1 });
        }
        match self.hold(notice) {
            Ok(()) => None,
            // The slot is what proved it holdable; without one the turn or the
            // daemon is waiting on this notice and it goes straight out.
            Err(notice) => Some(Held { notice, count: 1 }),
        }
    }

    /// Flip the switch; returns whether it ended up on.
    pub fn toggle(&mut self, now: Instant) -> bool {
        self.set_locked(!self.locked, now);
        self.locked
    }

    /// Set the switch. Locking pauses a drain in progress rather than
    /// cancelling it — the backlog survives, and so does its remaining wait.
    pub fn set_locked(&mut self, locked: bool, now: Instant) {
        if locked == self.locked {
            return;
        }
        self.locked = locked;
        self.toggled_at = locked.then_some(now);
        if !locked {
            self.on_unlock(now);
        }
    }

    /// Release the next held notice once its turn comes. `None` while locked,
    /// while the queue is empty, or before [`DRAIN_INTERVAL_MS`] elapsed — so
    /// an idle lock costs one branch per tick.
    pub fn tick(&mut self, now: Instant) -> Option<Held> {
        if self.locked {
            return None;
        }
        if self.next_due.is_some_and(|due| now < due) {
            return None;
        }
        let held = self.queue.pop_front()?;
        self.next_due =
            (!self.queue.is_empty()).then(|| now + Duration::from_millis(DRAIN_INTERVAL_MS));
        Some(held)
    }

    /// Drop the backlog and unlock — the notices belonged to the session that
    /// just closed, and delivering them into another one would be worse than
    /// losing them. The drop is counted, not hidden.
    pub fn reset(&mut self) {
        self.dropped += self.queue.len() as u64;
        self.queue.clear();
        self.locked = false;
        self.toggled_at = None;
        self.next_due = None;
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// When the switch last changed.
    pub fn locked_since(&self) -> Option<Instant> {
        self.toggled_at
    }

    /// Notices waiting to be delivered, coalesced ones counted once.
    pub fn held(&self) -> usize {
        self.queue.len()
    }

    /// Notices a cap or a session switch refused since start.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Whether a tick could still release something — false whenever the lock
    /// is on, so an idle lock arms no timer at all.
    pub fn wants_work(&self) -> bool {
        !self.locked && !self.queue.is_empty()
    }

    /// How long until the next release, `None` when none is scheduled.
    pub fn next_release_in(&self, now: Instant) -> Option<Duration> {
        self.next_due.map(|due| due.saturating_duration_since(now))
    }

    /// What the indicator should say, `None` when there is nothing to say.
    pub fn badge(&self) -> Option<LockBadge> {
        if self.locked {
            Some(LockBadge::Holding {
                held: self.queue.len(),
                dropped: self.dropped,
            })
        } else if self.queue.is_empty() {
            None
        } else {
            Some(LockBadge::Draining {
                left: self.queue.len(),
            })
        }
    }

    /// The badge as spans for the footer's right side; empty when there is
    /// nothing to show.
    pub fn footer_spans(&self, theme: &pacode_render::Theme) -> Vec<Span<'static>> {
        self.badge()
            .map_or_else(Vec::new, |badge| badge.spans(theme))
    }

    /// One deterministic line for `pacode stats` and the debug view.
    pub fn stats_line(&self) -> String {
        let plural = |n: usize| if n == 1 { "notice" } else { "notices" };
        match self.badge() {
            Some(LockBadge::Holding { held, dropped }) if dropped > 0 => {
                format!(
                    "notice lock: on, holding {held} {}, {dropped} dropped",
                    plural(held)
                )
            }
            Some(LockBadge::Holding { held, .. }) => {
                format!("notice lock: on, holding {held} {}", plural(held))
            }
            Some(LockBadge::Draining { left }) => {
                format!("notice lock: off, draining {left} {}", plural(left))
            }
            None => "notice lock: off".to_string(),
        }
    }

    /// Park a holdable notice. `Err` hands the notice back when it turns out not
    /// to be holdable — the queue's caps are per source, so a notice with no
    /// source has nowhere to sit.
    fn hold(&mut self, notice: Notice) -> Result<(), Notice> {
        let Some(slot) = notice.slot() else {
            return Err(notice);
        };
        if let Some(entry) = self
            .queue
            .iter_mut()
            .find(|entry| entry.notice.slot().is_some_and(|s| s.same_as(&slot)))
        {
            entry.count = entry.count.saturating_add(1);
            return Ok(());
        }
        let same_source = |entry: &Held| {
            entry
                .notice
                .source()
                .is_some_and(|s| s.same_as(slot.source))
        };
        if self.queue.iter().filter(|e| same_source(e)).count() >= PER_SOURCE_CAP {
            if let Some(idx) = self.queue.iter().position(same_source) {
                self.queue.remove(idx);
                self.dropped += 1;
            }
        } else if self.queue.len() >= QUEUE_CAP {
            self.dropped += 1;
            return Ok(());
        }
        self.queue.push_back(Held { notice, count: 1 });
        Ok(())
    }

    fn on_unlock(&mut self, now: Instant) {
        self.next_due = if self.queue.is_empty() {
            None
        } else {
            // The first release is immediate — a drain paused by a re-lock
            // keeps the part of its wait that is left, never less than now.
            Some(self.next_due.map_or(now, |due| due.max(now)))
        };
    }
}
