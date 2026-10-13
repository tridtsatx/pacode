use super::*;
use pacode_types::{ContentBlock, Message, Role, ToastLevel};
use std::sync::Barrier;
use std::thread;

#[test]
fn live_output_finds_its_own_row_and_only_while_it_runs() {
    // Two calls of one step in flight: each task's output goes to the row
    // of the call that started it, never to its neighbour's.
    let (a, b) = (CallId::new("a"), CallId::new("b"));
    let mut transcript = TranscriptState::new(64);
    assert_eq!(transcript.running_tool_call_seq(&a), None);

    transcript.upsert(test_tool_item(1, &a, ToolStatus::Running));
    transcript.upsert(test_tool_item(2, &b, ToolStatus::Running));
    assert_eq!(transcript.running_tool_call_seq(&a), Some(1));
    assert_eq!(transcript.running_tool_call_seq(&b), Some(2));

    // `b` finished: output still in transit for it is dropped rather than
    // appended under its result, and `a` keeps its row.
    transcript.upsert(test_tool_item(2, &b, ToolStatus::Ok));
    assert_eq!(transcript.running_tool_call_seq(&b), None);
    assert_eq!(transcript.running_tool_call_seq(&a), Some(1));
}

fn test_tool_item(seq: u64, call_id: &CallId, status: ToolStatus) -> TranscriptItem {
    TranscriptItem {
        seq,
        agent: AgentId::main(),
        ts_ms: seq,
        kind: TranscriptKind::ToolCall {
            call_id: call_id.clone(),
            name: "bash".into(),
            title: "command".into(),
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
fn test_event_sink_clones_publish_in_sequence_order() {
    const EMITTERS: usize = 8;
    const EVENTS_PER_EMITTER: usize = 128;

    let sink = EventSink::new();
    let clone = sink.clone();
    assert!(Arc::ptr_eq(&sink.publish, &clone.publish));
    drop(clone);

    let mut receiver = sink.subscribe();
    let start = Arc::new(Barrier::new(EMITTERS));
    let handles = (0..EMITTERS)
        .map(|emitter| {
            let sink = sink.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                (0..EVENTS_PER_EMITTER)
                    .map(|index| {
                        let title = format!("{emitter}:{index}");
                        let seq = sink.emit(Event::Toast {
                            level: ToastLevel::Info,
                            title: title.clone(),
                            detail: None,
                        });
                        (title, seq)
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();

    let expected = handles
        .into_iter()
        .flat_map(|handle| handle.join().unwrap())
        .collect::<HashMap<_, _>>();
    assert_eq!(expected.len(), EMITTERS * EVENTS_PER_EMITTER);

    let received = (0..EMITTERS * EVENTS_PER_EMITTER)
        .map(|_| receiver.try_recv().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        received.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
        (1..=u64::try_from(expected.len()).unwrap()).collect::<Vec<_>>()
    );
    for (seq, event) in received {
        let Event::Toast { title, .. } = event else {
            panic!("expected Toast, got {event:?}");
        };
        assert_eq!(expected.get(&title), Some(&seq));
    }
}

#[test]
fn event_sink_recovers_from_poisoned_publish_lock() {
    let sink = EventSink::new();
    let poisoner = sink.clone();
    thread::spawn(move || {
        let _guard = poisoner.publish.lock().unwrap();
        panic!("poison");
    })
    .join()
    .unwrap_err();

    assert_eq!(
        sink.emit(Event::Toast {
            level: ToastLevel::Info,
            title: "recovered".into(),
            detail: None,
        }),
        1
    );
}

#[test]
fn allocated_item_seqs_can_be_upserted_out_of_order() {
    let mut state = TranscriptState::new(10);
    let first = state.next_seq();
    let second = state.next_seq();
    for seq in [second, first] {
        state.upsert(TranscriptItem {
            seq,
            agent: AgentId::main(),
            ts_ms: seq,
            kind: TranscriptKind::User {
                text: format!("item-{seq}"),
            },
        });
    }
    assert_eq!(
        state.tail.iter().map(|item| item.seq).collect::<Vec<_>>(),
        vec![first, second]
    );
    assert!(state.pending_item_seqs.is_empty());
}

#[test]
fn oversized_delta_is_not_rebuffered() {
    let mut coalescer = DeltaCoalescer::new();
    let delta = "x".repeat(1024 * 1024);
    let emitted = coalescer
        .push(&delta, Instant::now())
        .expect("oversized delta");
    assert_eq!(emitted.len(), delta.len());
    assert_eq!(emitted.as_bytes(), delta.as_bytes());
    assert!(coalescer.take().is_none());
}

#[test]
fn tool_item_cache_is_bounded() {
    let mut state = TranscriptState::new(1_000);
    let ids = (0..TOOL_ITEMS_CAP + 10)
        .map(|index| CallId::new(format!("call-{index}")))
        .collect::<Vec<_>>();
    for (index, call_id) in ids.iter().enumerate() {
        state.upsert(test_tool_item(
            u64::try_from(index).unwrap(),
            call_id,
            ToolStatus::Ok,
        ));
    }
    assert_eq!(state.tool_items.len(), TOOL_ITEMS_CAP);
    assert!(!state.tool_items.contains_key(&ids[0]));
    assert!(state.tool_items.contains_key(ids.last().unwrap()));
    assert!(state.tool_items_bytes <= TOOL_ITEMS_BYTES_CAP);
}

#[test]
fn transcript_tail_is_bounded_by_bytes() {
    let mut state = TranscriptState::new(10);
    for seq in 0..2 {
        state.upsert(TranscriptItem {
            seq,
            agent: AgentId::main(),
            ts_ms: seq,
            kind: TranscriptKind::User {
                text: "x".repeat(TAIL_BYTES_CAP / 2 + 1),
            },
        });
    }
    assert_eq!(state.tail.len(), 1);
    assert_eq!(state.tail[0].seq, 1);
    assert!(state.tail_bytes <= TAIL_BYTES_CAP);
}

#[test]
fn tool_item_cache_is_bounded_by_bytes() {
    let mut state = TranscriptState::new(10);
    let ids = [CallId::new("first"), CallId::new("second")];
    for (seq, call_id) in ids.iter().enumerate() {
        let mut item = test_tool_item(u64::try_from(seq).unwrap(), call_id, ToolStatus::Ok);
        let TranscriptKind::ToolCall { preview, .. } = &mut item.kind else {
            unreachable!();
        };
        *preview = "x".repeat(TOOL_ITEMS_BYTES_CAP / 2 + 1);
        state.upsert(item);
    }
    assert_eq!(state.tool_items.len(), 1);
    assert!(!state.tool_items.contains_key(&ids[0]));
    assert!(state.tool_items.contains_key(&ids[1]));
    assert!(state.tool_items_bytes <= TOOL_ITEMS_BYTES_CAP);
}

#[test]
fn running_tool_items_are_not_evicted() {
    let mut state = TranscriptState::new(TOOL_ITEMS_CAP);
    let ids = (0..=TOOL_ITEMS_CAP)
        .map(|index| CallId::new(format!("running-{index}")))
        .collect::<Vec<_>>();
    for (seq, call_id) in ids.iter().enumerate() {
        state.upsert(test_tool_item(
            u64::try_from(seq).unwrap(),
            call_id,
            ToolStatus::Running,
        ));
    }
    assert_eq!(state.tool_items.len(), TOOL_ITEMS_CAP + 1);
    assert!(ids.iter().all(|id| state.tool_items.contains_key(id)));
    assert!(state.tool_items.values().all(|item| {
        matches!(
            &item.kind,
            TranscriptKind::ToolCall {
                status: ToolStatus::Running,
                ..
            }
        )
    }));
}

#[test]
fn late_terminal_update_uses_retained_running_state() {
    let mut state = TranscriptState::new(1);
    let first = CallId::new("first");
    let second = CallId::new("second");
    state.upsert(test_tool_item(0, &first, ToolStatus::Running));
    state.upsert(test_tool_item(1, &second, ToolStatus::Running));
    assert!(!state.tail.iter().any(|item| item.seq == 0));

    let mut terminal = state.tool_item(&first).cloned().unwrap();
    let TranscriptKind::ToolCall {
        status, preview, ..
    } = &mut terminal.kind
    else {
        unreachable!();
    };
    *status = ToolStatus::Ok;
    *preview = "done".into();
    state.upsert(terminal);

    assert!(matches!(
        state.tool_item(&first).map(|item| &item.kind),
        Some(TranscriptKind::ToolCall {
            status: ToolStatus::Ok,
            ..
        })
    ));
    assert!(!state.tail.iter().any(|item| item.seq == 0));
}

#[test]
fn late_terminal_update_does_not_restore_fully_evicted_item() {
    let mut state = TranscriptState::new(1);
    let ids = (0..=TOOL_ITEMS_CAP)
        .map(|index| CallId::new(format!("terminal-{index}")))
        .collect::<Vec<_>>();
    for (seq, call_id) in ids.iter().enumerate() {
        state.upsert(test_tool_item(
            u64::try_from(seq).unwrap(),
            call_id,
            ToolStatus::Ok,
        ));
    }
    assert!(!state.tool_items.contains_key(&ids[0]));

    state.upsert(test_tool_item(0, &ids[0], ToolStatus::Error));

    assert!(!state.tool_items.contains_key(&ids[0]));
    assert_eq!(state.tool_items.len(), TOOL_ITEMS_CAP);
    assert_eq!(state.tail.len(), 1);
    assert_eq!(state.tail[0].seq, u64::try_from(TOOL_ITEMS_CAP).unwrap());
}

#[test]
fn test_history_to_items_pairs_tool_use_and_result() {
    let agent = AgentId::main();
    let call_id = CallId::generate();

    let msgs = vec![
        (0, Arc::new(Message::user("run echo"))),
        (
            1,
            Arc::new(Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::Text {
                        text: "Running echo...".into(),
                    },
                    ContentBlock::ToolUse {
                        id: call_id.clone(),
                        name: "bash".into(),
                        input: serde_json::json!({"command": "echo hi"}),
                    },
                ],
            )),
        ),
        (
            2,
            Arc::new(Message::tool_result(call_id.clone(), "hi\n", false)),
        ),
        (
            3,
            Arc::new({
                let mut m = Message::user("hidden debug");
                m.meta.hidden = true;
                m
            }),
        ),
    ];

    let items = history_to_items(&agent, &msgs);
    assert_eq!(items.len(), 3);

    // Each item reports the message it was projected from, and the item seq
    // is left for the caller: item and message seqs are separate domains.
    assert_eq!(
        items.iter().map(|i| i.message_seq).collect::<Vec<_>>(),
        vec![0, 1, 1]
    );
    assert!(
        items.iter().all(|i| i.item.seq == 0),
        "the projection must not invent item seqs"
    );

    assert!(matches!(&items[0].item.kind, TranscriptKind::User { text } if text == "run echo"));

    assert!(
        matches!(&items[1].item.kind, TranscriptKind::Assistant { text, complete } if text == "Running echo..." && *complete)
    );

    // The tool item is tagged with its `tool_use` message (seq 1) even
    // though the message at seq 2 is what completed it.
    match &items[2].item.kind {
        TranscriptKind::ToolCall {
            call_id: cid,
            name,
            status,
            preview,
            ..
        } => {
            assert_eq!(cid, &call_id);
            assert_eq!(name, "bash");
            assert_eq!(status, &ToolStatus::Ok);
            assert_eq!(preview, "hi\n");
        }
        other => panic!("expected ToolCall, got {:?}", other),
    }
}

#[test]
fn tool_title_prefers_meaningful_keys_over_field_order() {
    let keys = crate::turn::tools::TITLE_KEYS;
    // `write` sends `content` first in JSON order — the title must still
    // come from `path`, not the file body.
    let input = serde_json::json!({
        "content": "#!/usr/bin/env python3\nprint('x')\n",
        "path": "tools/gen.py",
    });
    assert_eq!(tool_title("write", &input, keys), "write tools/gen.py");

    let input = serde_json::json!({"command": "cargo test", "timeout_secs": 30});
    assert_eq!(tool_title("bash", &input, keys), "bash cargo test");

    let input = serde_json::json!({"pattern": "TODO", "path": "src/"});
    assert_eq!(tool_title("grep", &input, keys), "grep TODO");
}

#[test]
fn tool_title_falls_back_and_skips_meta_keys() {
    let keys = crate::turn::tools::TITLE_KEYS;
    // No preferred key: first non-empty string wins; `intent` and
    // `accept_large_output` never do.
    let input = serde_json::json!({
        "intent": "checking the build",
        "action": "status",
    });
    assert_eq!(tool_title("bg", &input, keys), "bg status");

    let input = serde_json::json!({"accept_large_output": true});
    assert_eq!(tool_title("read", &input, keys), "read");

    // A preferred key present but empty loses to the next preferred one.
    let input = serde_json::json!({"command": " ", "url": "https://x.dev"});
    assert_eq!(
        tool_title("webfetch", &input, keys),
        "webfetch https://x.dev"
    );
}

#[test]
fn tool_title_truncates_long_args() {
    let keys = crate::turn::tools::TITLE_KEYS;
    let long = "x".repeat(200);
    let input = serde_json::json!({"command": long});
    let title = tool_title("bash", &input, keys);
    assert_eq!(title, format!("bash {}", "x".repeat(60)));
}
