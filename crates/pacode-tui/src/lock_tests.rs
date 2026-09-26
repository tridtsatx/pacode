//! Tests for the notice lock: the hold/release policy, its caps, and the
//! `alt+l` binding that drives it.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::{AgentId, QuestionId, TaskId};

use super::*;

fn at(base: Instant, ms: u64) -> Instant {
    base + Duration::from_millis(ms)
}

fn locked() -> (NoticeLock, Instant) {
    let now = Instant::now();
    let mut lock = NoticeLock::new();
    lock.set_locked(true, now);
    (lock, now)
}

fn agent_finished(agent: &str, answer: &str) -> Notice {
    Notice::AgentFinished {
        agent: AgentId::new(agent),
        name: agent.to_string(),
        answer: answer.to_string(),
    }
}

fn task_finished(task: &str, tail: &str) -> Notice {
    Notice::TaskFinished {
        task: TaskId::new(task),
        label: task.to_string(),
        exit_code: 0,
        tail: tail.to_string(),
    }
}

/// Empty the backlog one notice per interval, the way the event loop does.
fn drain_all(lock: &mut NoticeLock, from: Instant) -> Vec<Notice> {
    let mut out = Vec::new();
    let mut now = from;
    while let Some(held) = lock.tick(now) {
        out.push(held.notice);
        now += Duration::from_millis(DRAIN_INTERVAL_MS);
    }
    out
}

fn answer_of(notice: &Notice) -> &str {
    match notice {
        Notice::AgentFinished { answer, .. } => answer,
        other => panic!("expected an agent-finished notice, got {other:?}"),
    }
}

fn task_of(notice: &Notice) -> &str {
    match notice {
        Notice::TaskFinished { task, .. } => task.as_str(),
        other => panic!("expected a task-finished notice, got {other:?}"),
    }
}

#[test]
fn holdable_notice_is_held_while_locked_and_not_delivered() {
    let (mut lock, now) = locked();
    assert!(lock.offer(agent_finished("a1", "done")).is_none());
    assert!(lock.offer(task_finished("t1", "ok")).is_none());
    assert_eq!(lock.held(), 2);
    // A tick while locked releases nothing, however long it waits.
    assert!(lock.tick(at(now, 10_000)).is_none());
    assert!(!lock.wants_work());
    assert_eq!(lock.held(), 2);
}

#[test]
fn unlocked_lock_delivers_immediately_and_queues_nothing() {
    let mut lock = NoticeLock::new();
    let held = lock.offer(agent_finished("a1", "done"));
    assert_eq!(held.map(|h| h.count), Some(1));
    assert_eq!(lock.held(), 0);
    assert!(lock.badge().is_none());
}

#[test]
fn a_notice_arriving_mid_drain_is_not_delayed_behind_the_backlog() {
    let (mut lock, now) = locked();
    lock.offer(agent_finished("a1", "old"));
    lock.set_locked(false, now);
    assert!(lock.tick(now).is_some());
    // The backlog is still draining, but a live notice is not a held one.
    let fresh = lock.offer(agent_finished("a2", "new"));
    assert_eq!(fresh.map(|h| h.notice), Some(agent_finished("a2", "new")));
    assert_eq!(lock.held(), 0);
}

#[test]
fn never_holdable_notices_are_never_queued_even_while_locked() {
    let (mut lock, now) = locked();
    let blocking = [
        Notice::PermissionRequest {
            title: "run rm -rf".to_string(),
        },
        Notice::QuestionAsked {
            question: QuestionId::new("qst_1"),
            text: "which branch?".to_string(),
        },
        Notice::InterruptAck {
            reason: "user pressed esc".to_string(),
        },
    ];
    for notice in blocking {
        assert!(!notice.is_holdable(), "{notice:?} must not be holdable");
        assert!(lock.offer(notice.clone()).is_some(), "{notice:?} was held");
    }
    assert_eq!(lock.held(), 0);
    assert_eq!(lock.dropped(), 0);
    // Nothing is waiting, so unlocking schedules no release.
    lock.set_locked(false, now);
    assert!(lock.tick(at(now, 10_000)).is_none());
}

#[test]
fn fifo_order_is_preserved_across_the_whole_drain() {
    let (mut lock, now) = locked();
    lock.offer(task_finished("t1", "first"));
    lock.offer(agent_finished("a1", "second"));
    lock.offer(agent_finished("a2", "third"));
    lock.offer(task_finished("t2", "fourth"));
    lock.set_locked(false, now);

    let drained = drain_all(&mut lock, now);
    assert_eq!(task_of(&drained[0]), "t1");
    assert_eq!(answer_of(&drained[1]), "second");
    assert_eq!(answer_of(&drained[2]), "third");
    assert_eq!(task_of(&drained[3]), "t2");
    assert_eq!(lock.held(), 0);
}

#[test]
fn the_per_source_cap_evicts_that_sources_oldest_and_spares_the_others() {
    let (mut lock, now) = locked();
    for i in 0..=PER_SOURCE_CAP {
        lock.offer(agent_finished("chatty", &format!("answer {i}")));
    }
    lock.offer(agent_finished("quiet", "only one"));
    assert_eq!(lock.held(), PER_SOURCE_CAP + 1);
    assert_eq!(lock.dropped(), 1);

    lock.set_locked(false, now);
    let drained = drain_all(&mut lock, now);
    // The chatty source lost its oldest, not its newest: a newer notice from
    // the same agent supersedes the one before it.
    assert_eq!(answer_of(&drained[0]), "answer 1");
    assert_eq!(
        drained[PER_SOURCE_CAP - 1],
        agent_finished("chatty", &format!("answer {PER_SOURCE_CAP}"))
    );
    // And the other source was not starved by the cap.
    assert!(drained.contains(&agent_finished("quiet", "only one")));
}

#[test]
fn the_overall_cap_drops_the_arrival_and_counts_it() {
    let (mut lock, now) = locked();
    for i in 0..QUEUE_CAP {
        lock.offer(agent_finished(&format!("a{i}"), "same answer"));
    }
    assert_eq!(lock.held(), QUEUE_CAP);
    assert_eq!(lock.dropped(), 0);
    // Identical payload, different agent: the id is what the orchestrator needs,
    // so these never coalesce — and this one is over the cap.
    assert!(
        lock.offer(agent_finished("overflow", "same answer"))
            .is_none()
    );
    assert_eq!(lock.held(), QUEUE_CAP);
    assert_eq!(lock.dropped(), 1);
    lock.set_locked(false, now);
    assert!(lock.tick(now).is_some());
}

#[test]
fn identical_notices_coalesce_into_one_delivery_carrying_their_count() {
    let (mut lock, now) = locked();
    lock.offer(task_finished("t1", "noise"));
    for _ in 0..10 {
        lock.offer(agent_finished("a1", "all done"));
    }
    lock.offer(task_finished("t2", "more noise"));
    assert_eq!(lock.held(), 3);

    lock.set_locked(false, now);
    let first = lock.tick(now).expect("first release is immediate");
    assert_eq!(first.notice, task_finished("t1", "noise"));
    let second = lock
        .tick(at(now, DRAIN_INTERVAL_MS))
        .expect("second release");
    assert_eq!(second.count, 10);
    assert_eq!(second.notice, agent_finished("a1", "all done"));
    // The merge kept the entry's original position, so t2 is still last.
    let third = lock
        .tick(at(now, DRAIN_INTERVAL_MS * 2))
        .expect("third release");
    assert_eq!(third.notice, task_finished("t2", "more noise"));
}

#[test]
fn identical_repeats_do_not_take_extra_queue_slots() {
    let (mut lock, _now) = locked();
    for i in 0..PER_SOURCE_CAP {
        for _ in 0..3 {
            lock.offer(agent_finished("a1", &format!("answer {i}")));
        }
    }
    // 8 distinct payloads from one source, each seen three times: still 8.
    assert_eq!(lock.held(), PER_SOURCE_CAP);
    assert_eq!(lock.dropped(), 0);
}

#[test]
fn the_drain_releases_gradually_rather_than_all_at_once() {
    let (mut lock, now) = locked();
    for i in 0..3 {
        lock.offer(agent_finished(&format!("a{i}"), "done"));
    }
    lock.set_locked(false, now);

    // First one immediately — a single held notice must not pay a delay.
    assert!(lock.tick(now).is_some());
    assert!(lock.wants_work());
    for ms in [1, 100, DRAIN_INTERVAL_MS - 1] {
        assert!(
            lock.tick(at(now, ms)).is_none(),
            "released early at {ms} ms"
        );
    }
    assert_eq!(lock.held(), 2);
    assert!(lock.tick(at(now, DRAIN_INTERVAL_MS)).is_some());
    assert_eq!(lock.held(), 1);
    assert!(lock.tick(at(now, DRAIN_INTERVAL_MS * 2)).is_some());
    assert_eq!(lock.held(), 0);
    // Nothing left: no deadline, no work, and no further releases ever.
    assert!(!lock.wants_work());
    assert_eq!(lock.next_release_in(at(now, DRAIN_INTERVAL_MS * 3)), None);
    assert!(lock.tick(at(now, DRAIN_INTERVAL_MS * 3)).is_none());
}

#[test]
fn toggling_off_starts_the_drain() {
    let (mut lock, now) = locked();
    lock.offer(agent_finished("a1", "done"));
    lock.offer(agent_finished("a2", "done too"));
    assert!(lock.tick(now).is_none());

    assert!(!lock.toggle(now));
    assert!(!lock.is_locked());
    assert!(lock.locked_since().is_none());
    assert!(lock.tick(now).is_some());
    assert!(lock.tick(at(now, DRAIN_INTERVAL_MS)).is_some());
}

#[test]
fn toggling_records_when_the_lock_was_set_and_twice_is_a_no_op() {
    let (mut lock, now) = locked();
    assert!(lock.is_locked());
    assert_eq!(lock.locked_since(), Some(now));
    let later = at(now, 5_000);
    assert!(!lock.toggle(later));
    assert_eq!(lock.locked_since(), None);
    // A second toggle-on must not move the timestamp.
    assert!(lock.toggle(later));
    lock.set_locked(true, at(later, 1_000));
    assert_eq!(lock.locked_since(), Some(later));
}

#[test]
fn relocking_pauses_the_drain_and_resuming_keeps_the_wait_left() {
    let (mut lock, now) = locked();
    lock.offer(agent_finished("a1", "one"));
    lock.offer(agent_finished("a2", "two"));
    lock.set_locked(false, now);
    assert!(lock.tick(now).is_some());

    // Re-locked 100 ms in: the remaining 150 ms of the wait is not forgiven.
    let mid = at(now, 100);
    lock.set_locked(true, mid);
    assert!(lock.tick(at(now, 10_000)).is_none());
    lock.set_locked(false, at(now, 200));
    assert!(lock.tick(at(now, 200)).is_none());
    assert!(lock.tick(at(now, DRAIN_INTERVAL_MS - 1)).is_none());
    assert!(lock.tick(at(now, DRAIN_INTERVAL_MS)).is_some());
}

#[test]
fn a_session_switch_drops_the_backlog_instead_of_delivering_it_elsewhere() {
    let (mut lock, now) = locked();
    lock.offer(agent_finished("a1", "one"));
    lock.offer(agent_finished("a2", "two"));
    lock.reset();
    assert!(!lock.is_locked());
    assert_eq!(lock.held(), 0);
    assert_eq!(lock.dropped(), 2);
    assert!(lock.tick(at(now, 10_000)).is_none());
}

#[test]
fn the_badge_says_what_is_held_and_whether_notices_were_lost() {
    let (mut lock, now) = locked();
    assert_eq!(
        lock.badge(),
        Some(LockBadge::Holding {
            held: 0,
            dropped: 0
        })
    );
    lock.offer(agent_finished("a1", "one"));
    lock.offer(agent_finished("a2", "two"));
    assert_eq!(
        lock.badge(),
        Some(LockBadge::Holding {
            held: 2,
            dropped: 0
        })
    );
    assert_eq!(lock.stats_line(), "notice lock: on, holding 2 notices");
    let theme = pacode_render::Theme::default();
    let text: String = lock
        .footer_spans(&theme)
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.contains("LOCK 2"), "{text}");

    lock.set_locked(false, now);
    assert_eq!(lock.badge(), Some(LockBadge::Draining { left: 2 }));
    assert_eq!(lock.stats_line(), "notice lock: off, draining 2 notices");

    drain_all(&mut lock, now);
    assert!(lock.badge().is_none());
    assert!(lock.footer_spans(&theme).is_empty());
    assert_eq!(lock.stats_line(), "notice lock: off");
}

#[test]
fn the_badge_reports_a_lossy_queue_instead_of_hiding_it() {
    let (mut lock, _now) = locked();
    for i in 0..=PER_SOURCE_CAP {
        lock.offer(agent_finished("a1", &format!("answer {i}")));
    }
    assert_eq!(
        lock.badge(),
        Some(LockBadge::Holding {
            held: PER_SOURCE_CAP,
            dropped: 1
        })
    );
    assert_eq!(
        lock.stats_line(),
        "notice lock: on, holding 8 notices, 1 dropped"
    );
    let text: String = lock
        .footer_spans(&pacode_render::Theme::default())
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.contains("+1 dropped"), "{text}");
}

#[test]
fn alt_l_is_bound_to_the_lock_and_takes_no_chord_from_another_action() {
    use crate::binding::{Action, Binding, Keymap};

    let alt_l = Binding {
        code: KeyCode::Char('l'),
        mods: KeyModifiers::ALT,
    };
    let keymap = Keymap::defaults();
    assert_eq!(keymap.bindings_for(Action::ToggleLock), &[alt_l]);
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::ALT)),
        Some(Action::ToggleLock)
    );
    assert_eq!(keymap.find_conflict(&alt_l, Action::ToggleLock), None);
    for &(name, action) in Keymap::action_names() {
        if action == Action::ToggleLock {
            continue;
        }
        assert!(
            !keymap.bindings_for(action).contains(&alt_l),
            "alt+l is already bound to {name}"
        );
    }
}
