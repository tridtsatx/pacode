use super::*;
use pacode_types::ToastLevel;

fn toast() -> Event {
    Event::Toast {
        level: ToastLevel::Info,
        title: "t".into(),
        detail: None,
    }
}

#[test]
fn seq_gap_requests_exactly_one_snapshot() {
    let mut r = ResyncState::default();
    r.on_snapshot(10);
    assert_eq!(r.classify(11, &toast()), Verdict::Apply);
    assert!(r.take_request().is_none());
    assert_eq!(r.classify(15, &toast()), Verdict::Apply);
    assert_eq!(r.take_request(), Some(Request::GetSnapshot));
    // Debounced: another gap while one is in flight sends nothing yet.
    assert_eq!(r.classify(20, &toast()), Verdict::Apply);
    assert!(r.take_request().is_none());
    assert!(r.take_request().is_none());
}

#[test]
fn stale_events_after_snapshot_are_ignored() {
    let mut r = ResyncState::default();
    r.on_snapshot(100);
    assert_eq!(r.classify(99, &toast()), Verdict::Stale);
    assert_eq!(r.classify(100, &toast()), Verdict::Stale);
    assert_eq!(r.classify(101, &toast()), Verdict::Apply);
    assert!(r.take_request().is_none());
}

#[test]
fn resync_event_and_desynced_request_a_snapshot() {
    let mut r = ResyncState::default();
    r.on_snapshot(5);
    let verdict = r.classify(0, &Event::Resync { reason: "x".into() });
    assert_eq!(verdict, Verdict::Consumed);
    assert_eq!(r.take_request(), Some(Request::GetSnapshot));
    r.on_snapshot(9);
    r.note_desynced();
    assert_eq!(r.take_request(), Some(Request::GetSnapshot));
}

#[test]
fn snapshot_older_than_applied_events_asks_again() {
    let mut r = ResyncState::default();
    r.on_snapshot(10);
    r.classify(14, &toast());
    assert_eq!(r.take_request(), Some(Request::GetSnapshot));
    // Reply covers only up to 12 while 14 was already applied and wiped.
    r.on_snapshot(12);
    assert_eq!(r.take_request(), Some(Request::GetSnapshot));
    r.on_snapshot(14);
    assert!(r.take_request().is_none());
}

#[test]
fn no_gap_check_before_first_snapshot_and_for_global_events() {
    let mut r = ResyncState::default();
    assert_eq!(r.classify(500, &toast()), Verdict::Apply);
    assert!(r.take_request().is_none());
    r.on_snapshot(500);
    let login = Event::LoginProgress {
        provider: "p".into(),
        stage: pacode_types::LoginStage::Waiting,
    };
    assert_eq!(r.classify(2, &login), Verdict::Apply);
    assert!(r.take_request().is_none());
}

#[test]
fn failed_request_allows_retry_on_next_gap() {
    let mut r = ResyncState::default();
    r.on_snapshot(1);
    r.note_desynced();
    assert!(r.take_request().is_some());
    r.on_request_failed();
    assert!(r.take_request().is_none());
    r.note_desynced();
    assert!(r.take_request().is_some());
}

#[test]
fn app_state_gap_yields_one_get_snapshot_action() {
    use crate::state::AppState;
    use pacode_client::ClientEvent;
    use pacode_types::Config;
    use std::time::Instant;

    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    state.resync.on_snapshot(10);
    let now = Instant::now();
    state.apply_client_event(
        ClientEvent::Event {
            seq: 11,
            event: toast(),
        },
        now,
    );
    assert!(state.take_resync_request().is_none());
    state.apply_client_event(
        ClientEvent::Event {
            seq: 30,
            event: toast(),
        },
        now,
    );
    state.apply_client_event(
        ClientEvent::Event {
            seq: 31,
            event: toast(),
        },
        now,
    );
    assert_eq!(state.take_resync_request(), Some(Request::GetSnapshot));
    assert!(state.take_resync_request().is_none());
    // A stale event after the snapshot is dropped without redraw.
    state.resync.on_snapshot(31);
    state.dirty = false;
    assert!(!state.apply_client_event(
        ClientEvent::Event {
            seq: 20,
            event: toast()
        },
        now
    ));
    assert!(!state.dirty);
}
