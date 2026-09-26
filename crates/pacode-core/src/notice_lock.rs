//! The notice lock: hold the injections the orchestrator would otherwise
//! receive, then let them out one at a time.
//!
//! A subagent finishing mid-turn re-drives its parent, and a finished
//! background task injects its result. Release five of those into a turn that
//! has already moved on and the model answers five questions nobody asked. The
//! lock parks them; unlocking paces them back in.
//!
//! # What is holdable
//!
//! Everything that reports work *back* to the orchestrator: a subagent's turn
//! ended, a background task reached a terminal state or stalled, a subagent
//! reported to its parent, a monitor fired. Each names a [`Source`], and that
//! source is what the per-source cap counts.
//!
//! # What is never holdable
//!
//! [`Injection::UserSteer`] and [`Injection::StatusRequest`] — the user's own
//! input, and the orchestrator asking its own subagent a question. A held steer
//! is a turn the user believes is running and is not, so
//! [`Injection::is_holdable`] is the single definition and
//! [`NoticeLock::hold_if_locked`] hands them straight back whatever the switch
//! says.
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
//! Both paths count into [`NoticeLock::dropped`], which the badge and the stats
//! line report. A quietly lossy queue is worse than a visible one.
//!
//! # Coalescing
//!
//! An arrival merges into a pending entry with the same [`Notice::slot`] — same
//! kind, same source, same payload. Ten identical "agent finished" notices are
//! one delivery carrying the multiplicity, not ten. The merge keeps the entry's
//! original queue position, so a merge never reorders the backlog. Two
//! different tasks' results carry two different sources and stay two
//! deliveries, and so do two agents that happen to report the same words: the
//! id is the part the orchestrator needs, so nothing merges across sources.
//!
//! The *kind* is part of the key, not decoration: a task that stalls and a task
//! that then finishes share a source, and a model that never learns about the
//! stall gets `task_finished exit=0` instead.
//!
//! # Release
//!
//! The first release is immediate — the common case is a single held notice and
//! the user has just unlocked, so added latency would be pure cost. Every
//! release after it waits [`DRAIN_INTERVAL_MS`] (250 ms). A release is not a
//! log line, it is input to a turn: 20 held notices landing together queue 20
//! re-drives. 250 ms keeps them visually sequential and clears a 20-notice
//! backlog in five seconds. The deadline applies only to the *held* backlog;
//! live traffic is never paced.
//!
//! Re-locking pauses a release in progress; unlocking again resumes whatever
//! part of the wait was left. While the switch is on,
//! [`NoticeLock::wants_work`] is false and no deadline is armed: an idle lock
//! costs nothing.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pacode_types::{AgentId, TaskId};

use crate::inject::Injection;

#[cfg(test)]
#[path = "notice_lock_tests.rs"]
mod notice_lock_tests;

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
    TaskStalled,
    SubagentReport,
    SystemNotice,
}

/// Who a holdable notice came from. Borrowed, so comparing a backlog entry
/// against an arrival allocates nothing.
#[derive(Clone, Copy, Debug)]
pub enum Source<'a> {
    Agent(&'a AgentId),
    Task(&'a TaskId),
    /// Notices with no id of their own. One bucket, so a stream of system
    /// notices can still be capped — a chatty source must never be able to
    /// starve the queue, and "no source" is a source like any other.
    System,
}

impl Source<'_> {
    /// Cross-lifetime equality — a derived `PartialEq` would tie both borrows
    /// to one lifetime, and the comparisons the queue needs span two.
    fn same_as(self, other: Source<'_>) -> bool {
        match (self, other) {
            (Source::Agent(a), Source::Agent(b)) => a == b,
            (Source::Task(a), Source::Task(b)) => a == b,
            (Source::System, Source::System) => true,
            (Source::Agent(_), Source::Task(_))
            | (Source::Agent(_), Source::System)
            | (Source::Task(_), Source::Agent(_))
            | (Source::Task(_), Source::System)
            | (Source::System, Source::Agent(_))
            | (Source::System, Source::Task(_)) => false,
        }
    }
}

/// One notification bound for the orchestrator.
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
    TaskStalled {
        task: TaskId,
        label: String,
        tail: String,
    },
    SubagentReport {
        agent: AgentId,
        text: String,
    },
    SystemNotice(String),
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
    /// The policy view of an injection — what the lock keys, caps and counts
    /// on. `None` for the variants [`Injection::is_holdable`] refuses.
    pub fn from_injection(injection: &Injection) -> Option<Self> {
        match injection {
            Injection::TaskFinished { task, tail } => Some(Self::TaskFinished {
                task: task.id.clone(),
                label: task.label.clone(),
                exit_code: task.exit_code.unwrap_or_default(),
                tail: tail.clone(),
            }),
            Injection::TaskStalled { task, tail } => Some(Self::TaskStalled {
                task: task.id.clone(),
                label: task.label.clone(),
                tail: tail.clone(),
            }),
            Injection::AgentFinished { agent, answer } => Some(Self::AgentFinished {
                agent: agent.id.clone(),
                name: agent.name.clone(),
                answer: answer.clone(),
            }),
            Injection::AgentStatus { agent, text } => Some(Self::SubagentReport {
                agent: agent.id.clone(),
                text: text.clone(),
            }),
            Injection::SystemNotice(text) => Some(Self::SystemNotice(text.clone())),
            Injection::UserSteer { .. } | Injection::StatusRequest => None,
        }
    }

    pub fn kind(&self) -> NoticeKind {
        match self {
            Self::AgentFinished { .. } => NoticeKind::AgentFinished,
            Self::TaskFinished { .. } => NoticeKind::TaskFinished,
            Self::TaskStalled { .. } => NoticeKind::TaskStalled,
            Self::SubagentReport { .. } => NoticeKind::SubagentReport,
            Self::SystemNotice(_) => NoticeKind::SystemNotice,
        }
    }

    /// The queue entry this notice came from. Every holdable kind has one: the
    /// inverse rule on [`Injection::is_holdable`] is what guarantees it.
    pub fn source(&self) -> Source<'_> {
        match self {
            Self::AgentFinished { agent, .. } | Self::SubagentReport { agent, .. } => {
                Source::Agent(agent)
            }
            Self::TaskFinished { task, .. } | Self::TaskStalled { task, .. } => Source::Task(task),
            Self::SystemNotice(_) => Source::System,
        }
    }

    /// The coalescing identity: same kind, same source, same payload.
    pub fn slot(&self) -> NoticeSlot<'_> {
        match self {
            Self::AgentFinished { agent, answer, .. } => NoticeSlot {
                kind: NoticeKind::AgentFinished,
                source: Source::Agent(agent),
                subject: answer,
            },
            Self::TaskFinished { task, tail, .. } => NoticeSlot {
                kind: NoticeKind::TaskFinished,
                source: Source::Task(task),
                subject: tail,
            },
            Self::TaskStalled { task, tail, .. } => NoticeSlot {
                kind: NoticeKind::TaskStalled,
                source: Source::Task(task),
                subject: tail,
            },
            Self::SubagentReport { agent, text, .. } => NoticeSlot {
                kind: NoticeKind::SubagentReport,
                source: Source::Agent(agent),
                subject: text,
            },
            Self::SystemNotice(text) => NoticeSlot {
                kind: NoticeKind::SystemNotice,
                source: Source::System,
                subject: text,
            },
        }
    }
}

/// One parked injection, plus how many identical ones it stands for.
#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    pub injection: Injection,
    pub notice: Notice,
    /// 1 unless identical injections coalesced into this one.
    pub count: u32,
}

/// One drained injection, tagged with the deque it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct TakenItem {
    /// What the caller delivers. For a coalesced entry this carries the
    /// multiplicity the parked one stood for.
    pub injection: Injection,
    pub origin: DueOrigin,
    /// The parked entry this came from, kept so a restore rebuilds the backlog
    /// verbatim — count included — rather than re-parking a re-derived notice.
    pub(crate) entry: Option<Held>,
}

impl TakenItem {
    /// A live-queue item, which has no parked entry behind it.
    pub fn queued(injection: Injection) -> Self {
        Self {
            injection,
            origin: DueOrigin::Queue,
            entry: None,
        }
    }

    /// An item the backlog released, carrying the parked entry so a restore
    /// can rebuild it verbatim.
    pub fn held(entry: Held) -> Self {
        let injection = entry.release();
        Self {
            injection,
            origin: DueOrigin::Held,
            entry: Some(entry),
        }
    }
}

/// A batch [`Agent::take_due`](crate::agent::Agent::take_due) released, kept
/// whole so [`Agent::restore_due`](crate::agent::Agent::restore_due) can undo
/// it exactly: every item with the deque it came from, and the release
/// deadline as it stood before.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Taken {
    pub items: Vec<TakenItem>,
    pub prev_due: Option<Instant>,
}

impl Taken {
    pub fn new(items: Vec<TakenItem>, prev_due: Option<Instant>) -> Self {
        Self { items, prev_due }
    }
}

impl Held {
    /// What goes to the model when this entry is released. The multiplicity is
    /// folded into the payload, so a delivery standing for ten arrivals says so
    /// instead of silently swallowing nine of them.
    pub fn release(&self) -> Injection {
        let mut injection = self.injection.clone();
        if self.count <= 1 {
            return injection;
        }
        let mark = format!("[x{}] ", self.count);
        let prepend = |text: &mut String| {
            if !text.starts_with('[') {
                text.insert_str(0, &mark);
            }
        };
        match &mut injection {
            Injection::TaskFinished { tail, .. } | Injection::TaskStalled { tail, .. } => {
                prepend(tail)
            }
            Injection::AgentFinished { answer, .. } => prepend(answer),
            Injection::AgentStatus { text, .. } => prepend(text),
            Injection::SystemNotice(text) => prepend(text),
            Injection::UserSteer { .. } | Injection::StatusRequest => {}
        }
        injection
    }
}

/// Where a released injection came from, so a restore can put it back where it
/// was. The two deques drain in that order — see [`crate::agent::Agent::take_due`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DueOrigin {
    /// Parked by the lock; the release was paced.
    Held,
    /// Live traffic; released with no pacing.
    Queue,
}

/// What the indicator should say, `None` when there is nothing to say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockBadge {
    /// Switch on, with `held` notices parked. `dropped` is zero unless a cap
    /// threw notices away.
    Holding { held: usize, dropped: u64 },
    /// Switch off, with `left` notices still trickling out.
    Draining { left: usize },
}

impl LockBadge {
    /// `LOCK 3` while holding, `… 3` while draining. The glyph table has no
    /// padlock and the word survives an ASCII terminal, a colourblind reader
    /// and a screen reader alike — colour is never the only carrier here.
    pub fn label(self) -> String {
        match self {
            Self::Holding { held, .. } => format!("LOCK {held}"),
            Self::Draining { left } => format!("… {left}"),
        }
    }
}

#[derive(Debug, Default)]
struct State {
    locked: bool,
    toggled_at: Option<Instant>,
    queue: VecDeque<Held>,
    dropped: u64,
    next_due: Option<Instant>,
}

/// The lock: the switch, the backlog it holds, and the release deadline. It
/// does not decide what a notice does once released — the drain sites hand back
/// what to deliver and the caller sends it.
#[derive(Debug, Default)]
pub struct NoticeLock {
    state: Mutex<State>,
}

impl NoticeLock {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Park `injection` if the switch is on. `true` means the lock kept it and
    /// the caller must *not* push it to the live queue or start a turn for it.
    ///
    /// An unlocked lock never parks: the pacing exists to soften the release of
    /// a *held backlog*, not to reorder live traffic, so a notice arriving
    /// mid-release goes straight out.
    pub fn hold_if_locked(&self, injection: &Injection) -> bool {
        let mut state = self.state();
        if !state.locked {
            return false;
        }
        let Some(notice) = Notice::from_injection(injection) else {
            return false;
        };
        self.hold(&mut state, injection, notice);
        true
    }

    /// Set the switch. Locking pauses a release in progress rather than
    /// cancelling it — the backlog survives, and so does its remaining wait.
    /// `true` when the switch actually moved.
    pub fn set_locked(&self, locked: bool, now: Instant) -> bool {
        let mut state = self.state();
        if locked == state.locked {
            return false;
        }
        state.locked = locked;
        state.toggled_at = locked.then_some(now);
        if !locked {
            state.next_due = if state.queue.is_empty() {
                None
            } else {
                // The first release is immediate — a drain paused by a re-lock
                // keeps the part of its wait that is left, never less than now.
                Some(state.next_due.map_or(now, |due| due.max(now)))
            };
        }
        true
    }

    /// Flip the switch; returns whether it ended up on.
    pub fn toggle(&self, now: Instant) -> bool {
        let next = !self.is_locked();
        self.set_locked(next, now);
        next
    }

    pub fn is_locked(&self) -> bool {
        self.state().locked
    }

    /// When the switch last changed.
    pub fn locked_since(&self) -> Option<Instant> {
        self.state().toggled_at
    }

    /// Injections waiting to be delivered, coalesced ones counted once.
    pub fn held(&self) -> usize {
        self.state().queue.len()
    }

    /// Injections a cap refused since start.
    pub fn dropped(&self) -> u64 {
        self.state().dropped
    }

    /// Whether a release could still come — false whenever the switch is on, so
    /// an idle or locked lock arms no timer at all.
    pub fn wants_work(&self) -> bool {
        let state = self.state();
        !state.locked && !state.queue.is_empty()
    }

    /// How long until the next release, `None` when none is scheduled.
    pub fn next_release_in(&self, now: Instant) -> Option<Duration> {
        self.state()
            .next_due
            .map(|due| due.saturating_duration_since(now))
    }

    /// Drop the parked backlog and count what it held, leaving the switch where
    /// it is. A stopped or closed agent must not release notices into whatever
    /// turn comes next, and a backlog nothing can drain must not keep the count
    /// the badge shows.
    pub fn discard_held(&self) {
        let mut state = self.state();
        state.dropped += state.queue.len() as u64;
        state.queue.clear();
        state.next_due = None;
    }

    /// Drop the backlog and unlock — the notices belonged to the session that
    /// just closed, and delivering them into another one would be worse than
    /// losing them. The drop is counted, not hidden.
    pub fn reset(&self) {
        self.discard_held();
        let mut state = self.state();
        state.locked = false;
        state.toggled_at = None;
    }

    /// What the indicator should say, `None` when there is nothing to say.
    pub fn badge(&self) -> Option<LockBadge> {
        let state = self.state();
        if state.locked {
            Some(LockBadge::Holding {
                held: state.queue.len(),
                dropped: state.dropped,
            })
        } else if state.queue.is_empty() {
            None
        } else {
            Some(LockBadge::Draining {
                left: state.queue.len(),
            })
        }
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

    /// Release the next parked injection once its turn comes, together with the
    /// entry it was and the deadline that was in force before the take — the
    /// two halves of one reversible pair. `None` while the switch is on, while
    /// the backlog is empty, or before [`DRAIN_INTERVAL_MS`] elapsed.
    pub(crate) fn take_held_due(&self, now: Instant) -> Option<HeldRelease> {
        let mut state = self.state();
        if state.locked {
            return None;
        }
        if state.next_due.is_some_and(|due| now < due) {
            return None;
        }
        let entry = state.queue.pop_front()?;
        let prev_due = state.next_due;
        state.next_due =
            (!state.queue.is_empty()).then(|| now + Duration::from_millis(DRAIN_INTERVAL_MS));
        Some(HeldRelease { entry, prev_due })
    }

    /// Put a released entry back at the front of the backlog and rewind the
    /// deadline. Verbatim, with no cap or coalesce pass: this undoes a
    /// `pop_front`, so the backlog it rebuilds is byte-identical to the one the
    /// take took from.
    pub(crate) fn restore_held_due(&self, entry: Held, prev_due: Option<Instant>) {
        let mut state = self.state();
        state.queue.push_front(entry);
        state.next_due = prev_due;
    }

    /// Park a holdable injection under the caps: merge into an identical
    /// pending entry, else evict the source's oldest on overflow, else drop the
    /// arrival when the backlog is full. The caller has already established
    /// holdability, so there is no hand-back here.
    fn hold(&self, state: &mut State, injection: &Injection, notice: Notice) {
        let slot = notice.slot();
        if let Some(entry) = state
            .queue
            .iter_mut()
            .find(|entry| entry.notice.slot().same_as(&slot))
        {
            entry.count = entry.count.saturating_add(1);
            return;
        }
        let same_source = |entry: &Held| entry.notice.source().same_as(slot.source);
        if state.queue.iter().filter(|e| same_source(e)).count() >= PER_SOURCE_CAP {
            if let Some(idx) = state.queue.iter().position(same_source) {
                state.queue.remove(idx);
                state.dropped += 1;
            }
        } else if state.queue.len() >= QUEUE_CAP {
            state.dropped += 1;
            return;
        }
        state.queue.push_back(Held {
            injection: injection.clone(),
            notice,
            count: 1,
        });
    }
}

/// One release out of the parked backlog: the entry as it was parked, and the
/// deadline that was in force before the take.
pub(crate) struct HeldRelease {
    pub entry: Held,
    pub prev_due: Option<Instant>,
}
