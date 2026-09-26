use pacode_types::SessionId;

use super::*;
use crate::memcap::MemoryPolicy;

const GIB: u64 = 1024 * 1024 * 1024;

fn session(tag: &str) -> SessionId {
    SessionId::new(format!("pacode-test-{tag}"))
}

#[test]
fn a_session_is_counted_while_its_lease_is_held() {
    let registry = SessionRegistry::new();
    assert_eq!(registry.live(), 0);
    let first = registry.open(&session("a"));
    let second = registry.open(&session("b"));
    assert_eq!(registry.live(), 2);
    assert!(registry.is_live(first.session()));
    drop(first);
    assert_eq!(registry.live(), 1);
    assert!(!registry.is_live(&session("a")));
    drop(second);
    assert_eq!(registry.live(), 0);
}

#[test]
fn opening_the_same_session_twice_counts_it_once_and_closes_cleanly() {
    let registry = SessionRegistry::new();
    // A reconnecting client makes the daemon open a session it already has.
    let first = registry.open(&session("a"));
    let second = registry.open(&session("a"));
    assert_eq!(registry.live(), 1);
    // Whichever lease ends first must not take the other one's share away.
    drop(first);
    assert_eq!(registry.live(), 1);
    drop(second);
    assert_eq!(registry.live(), 0);
}

#[test]
fn closing_a_session_nobody_opened_does_not_move_the_divisor() {
    let registry = SessionRegistry::new();
    let held = registry.open(&session("a"));
    // A late close from a path that already gave its lease back must not drive
    // the count below the truth and hand everyone else a bigger share.
    registry.close(&session("a"));
    registry.close(&session("a"));
    registry.close(&session("never-opened"));
    assert_eq!(registry.live(), 0);
    assert!(!registry.is_live(&session("a")));
    drop(held);
    assert_eq!(registry.live(), 0);
}

#[test]
fn a_panicking_session_gives_its_share_back() {
    let registry = SessionRegistry::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _lease = registry.open(&session("boom"));
        panic!("session setup blew up half way through");
    }));
    assert!(result.is_err());
    assert_eq!(
        registry.live(),
        0,
        "an unwind must not leave a session holding a share of the ceiling"
    );
}

#[test]
fn a_crashed_daemon_leaves_no_division_behind() {
    // The count is only ever in RAM, so a process that dies holding leases
    // does not leave a division for the next one to inherit.
    let dead = SessionRegistry::new();
    let leaked = dead.open(&session("a"));
    let leaked_again = dead.open(&session("b"));
    std::mem::forget(leaked);
    std::mem::forget(leaked_again);
    assert_eq!(dead.live(), 2);

    // What a restarted daemon builds.
    let restarted = SessionRegistry::new();
    assert_eq!(restarted.live(), 0);
}

#[test]
fn the_ceiling_goes_back_up_when_a_session_ends() {
    let registry = SessionRegistry::new();
    let policy = MemoryPolicy {
        percent: 80,
        split_sessions: true,
        machine_ram_bytes: 100 * GIB,
    };
    let first = registry.open(&session("a"));
    let second = registry.open(&session("b"));
    assert_eq!(policy.ceiling_for(registry.live()), Some(40 * GIB));
    drop(second);
    // The share is recomputed, not remembered: the survivor gets the ceiling
    // back rather than staying halved for the rest of the daemon's life.
    assert_eq!(policy.ceiling_for(registry.live()), Some(80 * GIB));
    drop(first);
    assert_eq!(policy.ceiling_for(registry.live()), Some(80 * GIB));
}
