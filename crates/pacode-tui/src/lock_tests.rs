//! Tests for the client's view of the notice lock: what the badge says, and
//! that `alt+l` is bound without taking a chord from another action.
//!
//! The policy behind the lock — caps, coalescing, the paced release — lives in
//! `pacode-core` next to the injections it governs, and is tested there.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::SessionId;

use super::*;

const SESSION: &str = "pacode-0001abcd00000001";

fn reported(locked: bool, held: usize, dropped: u64) -> LockView {
    let mut view = LockView::default();
    view.apply(SessionId::new(SESSION), locked, held, dropped);
    view
}

fn badge_text(view: &LockView) -> String {
    view.footer_spans(&pacode_render::Theme::default())
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn the_badge_says_what_the_daemon_is_holding() {
    let holding = reported(true, 2, 0);
    assert!(holding.locked());
    assert_eq!(holding.held(), 2);
    assert_eq!(
        holding.badge(),
        Some(LockBadge {
            locked: true,
            held: 2,
            dropped: 0,
        })
    );
    assert_eq!(holding.stats_line(), "notice lock: on, holding 2 notices");
    assert!(
        badge_text(&holding).contains("LOCK 2"),
        "{}",
        badge_text(&holding)
    );

    // Unlocked with a backlog left is a drain in progress, not an idle lock.
    let draining = reported(false, 2, 0);
    assert_eq!(
        draining.badge(),
        Some(LockBadge {
            locked: false,
            held: 2,
            dropped: 0
        })
    );
    assert_eq!(
        draining.stats_line(),
        "notice lock: off, draining 2 notices"
    );

    // Nothing held and nothing on: the badge must disappear, not linger.
    assert!(reported(false, 0, 0).badge().is_none());
    assert!(
        reported(false, 0, 0)
            .footer_spans(&pacode_render::Theme::default())
            .is_empty()
    );
}

#[test]
fn the_badge_reports_a_lossy_queue_instead_of_hiding_it() {
    let view = reported(true, 8, 1);
    assert_eq!(
        view.badge(),
        Some(LockBadge {
            locked: true,
            held: 8,
            dropped: 1
        })
    );
    assert_eq!(
        view.stats_line(),
        "notice lock: on, holding 8 notices, 1 dropped"
    );
    assert!(
        badge_text(&view).contains("+1 dropped"),
        "{}",
        badge_text(&view)
    );
}

#[test]
fn a_report_replaces_every_number_rather_than_adding_to_it() {
    // A later report is the whole truth: a held count left over from an earlier
    // one would show a lock holding something it is not.
    let mut view = reported(true, 7, 3);
    view.apply(SessionId::new(SESSION), true, 1, 3);
    assert_eq!(view.held(), 1);
    assert_eq!(view.dropped(), 3);
    assert_eq!(
        view.badge(),
        Some(LockBadge {
            locked: true,
            held: 1,
            dropped: 3,
        })
    );

    // Draining to empty clears the count; the switch itself is unchanged, and
    // the daemon's drop counter is not the client's to reset.
    view.apply(SessionId::new(SESSION), true, 0, 3);
    assert!(view.locked());
    assert_eq!(
        view.badge(),
        Some(LockBadge {
            locked: true,
            held: 0,
            dropped: 3,
        })
    );
    view.apply(SessionId::new(SESSION), false, 0, 3);
    assert!(view.badge().is_none());
    assert_eq!(view.dropped(), 3);
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
