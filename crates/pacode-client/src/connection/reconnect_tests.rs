use std::sync::atomic::{AtomicUsize, Ordering};

use pacode_types::{Event, ToastLevel};
use tokio::sync::mpsc;

use super::emit_event;
use crate::ClientEvent;

fn ev(seq: u64) -> ClientEvent {
    ClientEvent::Event {
        seq,
        event: Event::Toast {
            level: ToastLevel::Info,
            title: "t".into(),
            detail: None,
        },
    }
}

#[test]
fn desynced_precedes_first_event_after_a_full_queue() {
    let (tx, mut rx) = mpsc::channel(2);
    let dropped = AtomicUsize::new(0);
    emit_event(&tx, &dropped, ev(1));
    emit_event(&tx, &dropped, ev(2));
    emit_event(&tx, &dropped, ev(3));
    emit_event(&tx, &dropped, ev(4));
    assert_eq!(dropped.load(Ordering::Relaxed), 2);

    assert_eq!(rx.try_recv().ok(), Some(ev(1)));
    assert_eq!(rx.try_recv().ok(), Some(ev(2)));
    emit_event(&tx, &dropped, ev(5));
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
    assert_eq!(rx.try_recv().ok(), Some(ClientEvent::Desynced));
    assert_eq!(rx.try_recv().ok(), Some(ev(5)));
    assert!(rx.try_recv().is_err());
}

#[test]
fn no_desynced_without_loss() {
    let (tx, mut rx) = mpsc::channel(4);
    let dropped = AtomicUsize::new(0);
    emit_event(&tx, &dropped, ev(1));
    assert_eq!(rx.try_recv().ok(), Some(ev(1)));
    assert!(rx.try_recv().is_err());
}
