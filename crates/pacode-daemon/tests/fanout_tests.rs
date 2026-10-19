use std::time::Duration;

use pacode_daemon::fanout::run_forwarder;
use pacode_types::ToastLevel;
use pacode_types::protocol::{Event, ServerMessage};
use tokio::sync::{broadcast, mpsc};

fn toast() -> Event {
    Event::Toast {
        level: ToastLevel::Info,
        title: "t".into(),
        detail: None,
    }
}

#[tokio::test]
async fn slow_reader_sees_resync_before_newer_events() {
    let (btx, brx) = broadcast::channel::<(u64, Event)>(1024);
    let (tx, mut rx) = mpsc::channel::<String>(8);
    let handle = tokio::spawn(run_forwarder(brx, tx, "session"));

    // The client does not read while 3000 events go by.
    for seq in 1..=3000u64 {
        let _ = btx.send((seq, toast()));
        if seq % 100 == 0 {
            tokio::task::yield_now().await;
        }
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The first thing past the queued backlog is a Resync (seq 0), never silence.
    let mut events_before = 0;
    loop {
        let line = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out")
            .expect("closed");
        match serde_json::from_str::<ServerMessage>(line.trim()).expect("valid") {
            ServerMessage::Event {
                event: Event::Resync { .. },
                seq,
            } => {
                assert_eq!(seq, 0);
                break;
            }
            ServerMessage::Event { .. } => events_before += 1,
            ServerMessage::Reply { .. } => panic!("unexpected reply"),
        }
    }
    assert!(events_before >= 8, "queued backlog precedes the Resync");
    handle.abort();
}
