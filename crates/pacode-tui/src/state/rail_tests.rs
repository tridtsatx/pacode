use super::*;
use crate::state::IDLE_DEBOUNCE_MS;
use std::time::Duration;

#[test]
fn test_rail_idle_debounce() {
    let mut rail = RailState::default();
    let t0 = Instant::now();

    // Turn active -> not idle
    assert!(!rail.update_idle(true, t0));
    assert!(!rail.show_session_stats);
    assert!(rail.idle_since.is_none());

    // Turn finishes at t1
    let t1 = t0 + Duration::from_millis(100);
    assert!(!rail.update_idle(false, t1));
    assert!(!rail.show_session_stats);
    assert_eq!(rail.idle_since, Some(t1));

    // After 1 second (less than 2s debounce)
    let t2 = t1 + Duration::from_millis(1000);
    assert!(!rail.update_idle(false, t2));
    assert!(!rail.show_session_stats);

    // After 2.1 seconds (> 2s debounce)
    let t3 = t1 + Duration::from_millis(IDLE_DEBOUNCE_MS + 100);
    assert!(rail.update_idle(false, t3));
    assert!(rail.show_session_stats);

    // Subsequent idle call does not trigger change again
    assert!(!rail.update_idle(false, t3 + Duration::from_millis(100)));

    // New turn starts -> immediately reverts to AGENTS
    let t4 = t3 + Duration::from_millis(200);
    assert!(rail.update_idle(true, t4));
    assert!(!rail.show_session_stats);
    assert!(rail.idle_since.is_none());
}
