use super::*;
use pacode_types::{ToolStatus, TranscriptKind};

fn call_item(seq: u64, call_id: &CallId, status: ToolStatus) -> TranscriptItem {
    TranscriptItem {
        seq,
        agent: AgentId::main(),
        ts_ms: 0,
        kind: TranscriptKind::ToolCall {
            call_id: call_id.clone(),
            name: "bash".into(),
            title: "bash echo".into(),
            intent: None,
            status,
            preview: String::new(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    }
}

#[test]
fn completed_tool_calls_are_not_retained() {
    let mut t = TranscriptState::new(1000);
    for i in 0..100u64 {
        let id = CallId::generate();
        t.upsert(call_item(i, &id, ToolStatus::Running));
        assert_eq!(t.tool_items.len(), 1);
        t.upsert(call_item(i, &id, ToolStatus::Ok));
        assert!(t.tool_items.is_empty());
    }
}

#[test]
fn running_call_stays_until_terminal() {
    let mut t = TranscriptState::new(10);
    let id = CallId::generate();
    t.upsert(call_item(0, &id, ToolStatus::Running));
    assert!(t.tool_items.contains_key(&id));
    t.upsert(call_item(0, &id, ToolStatus::Denied));
    assert!(!t.tool_items.contains_key(&id));
}

#[test]
fn event_sink_receiver_count_tracks_subscribers() {
    let sink = EventSink::new();
    assert_eq!(sink.receiver_count(), 0);
    let rx = sink.subscribe();
    assert_eq!(sink.receiver_count(), 1);
    drop(rx);
    assert_eq!(sink.receiver_count(), 0);
}

#[test]
fn concurrent_emits_reach_receivers_in_seq_order() {
    use std::sync::Arc;
    let sink = Arc::new(EventSink::new());
    let mut rx = sink.subscribe();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let sink = Arc::clone(&sink);
            std::thread::spawn(move || {
                for _ in 0..100 {
                    sink.emit(pacode_types::Event::Resync {
                        reason: String::new(),
                    });
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let mut last = 0;
    let mut n = 0;
    while let Ok((seq, _)) = rx.try_recv() {
        assert_eq!(seq, last + 1, "seq must arrive in order and without gaps");
        last = seq;
        n += 1;
    }
    assert_eq!(n, 800);
}
