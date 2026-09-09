use super::*;
use codeapp_types::AgentId;
use std::time::Duration;

#[test]
fn test_transcript_reset_and_prepend() {
    let mut t = Transcript::new(5);
    let items = vec![
        TranscriptItem {
            seq: 1,
            agent: AgentId::main(),
            ts_ms: 100,
            kind: TranscriptKind::User {
                text: "hello".into(),
            },
        },
        TranscriptItem {
            seq: 2,
            agent: AgentId::main(),
            ts_ms: 200,
            kind: TranscriptKind::Assistant {
                text: "world".into(),
                complete: true,
            },
        },
    ];
    t.reset(items, true);
    assert_eq!(t.cells.len(), 2);
    assert_eq!(t.cells[0].id, 1);
    assert_eq!(t.cells[1].id, 2);
    assert!(t.has_more_history);

    let older = vec![TranscriptItem {
        seq: 0,
        agent: AgentId::main(),
        ts_ms: 50,
        kind: TranscriptKind::User {
            text: "older".into(),
        },
    }];
    t.prepend(older, false);
    assert_eq!(t.cells.len(), 3);
    assert_eq!(t.cells[0].id, 0);
    assert_eq!(t.cells[1].id, 1);
    assert_eq!(t.cells[2].id, 2);
    assert!(!t.has_more_history);
}

#[test]
fn test_streaming_and_reveal() {
    let mut t = Transcript::new(10);
    let now = Instant::now();

    let live_item = TranscriptItem {
        seq: 10,
        agent: AgentId::main(),
        ts_ms: 500,
        kind: TranscriptKind::Assistant {
            text: "Hello".into(),
            complete: false,
        },
    };
    t.upsert(live_item, now);
    assert_eq!(t.live_cell, Some(10));
    // Revealed text should start empty
    match &t.cells[0].kind {
        CellKind::Item(TranscriptKind::Assistant { text, complete }) => {
            assert_eq!(text, "");
            assert!(!complete);
        }
        _ => panic!("unexpected cell kind"),
    }

    t.push_delta(10, " there", false, now);
    assert!(t.has_backlog());

    // Advance time to reveal
    let later = now + Duration::from_millis(100);
    let revealed = t.tick_stream(later);
    assert!(revealed);

    // Finalize
    t.flush_stream();
    match &t.cells[0].kind {
        CellKind::Item(TranscriptKind::Assistant { text, .. }) => {
            assert_eq!(text, "Hello there");
        }
        _ => panic!("unexpected cell kind"),
    }
}

#[test]
fn test_enforce_cap_retains_live_cell() {
    let mut t = Transcript::new(2);
    let now = Instant::now();

    t.upsert(
        TranscriptItem {
            seq: 1,
            agent: AgentId::main(),
            ts_ms: 1,
            kind: TranscriptKind::User { text: "1".into() },
        },
        now,
    );
    t.upsert(
        TranscriptItem {
            seq: 2,
            agent: AgentId::main(),
            ts_ms: 2,
            kind: TranscriptKind::User { text: "2".into() },
        },
        now,
    );
    // Add third item which is live
    t.upsert(
        TranscriptItem {
            seq: 3,
            agent: AgentId::main(),
            ts_ms: 3,
            kind: TranscriptKind::Assistant {
                text: "3".into(),
                complete: false,
            },
        },
        now,
    );

    assert_eq!(t.cells.len(), 2);
    assert_eq!(t.cells[0].id, 2);
    assert_eq!(t.cells[1].id, 3);
    assert_eq!(t.live_cell, Some(3));
}

#[test]
fn test_turn_ended_stats_attached() {
    use crate::state::AppState;
    use codeapp_types::stream::Usage;
    use codeapp_types::{Config, Event, TurnStop};

    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    let now = Instant::now();

    // Start turn
    state.apply_event(
        1,
        Event::TurnStarted {
            agent: AgentId::main(),
            turn: codeapp_types::TurnId::new("trn_1"),
        },
        now,
    );

    // Assistant item added
    state.apply_event(
        2,
        Event::ItemAdded(TranscriptItem {
            seq: 2,
            agent: AgentId::main(),
            ts_ms: 1000,
            kind: TranscriptKind::Assistant {
                text: "Hello world".into(),
                complete: true,
            },
        }),
        now,
    );

    let initial_version = state.transcript.cells[0].version;
    assert_eq!(state.transcript.cells[0].stats, None);

    // Turn ended 2 seconds later with usage
    let later = now + Duration::from_secs(2);
    state.apply_event(
        3,
        Event::TurnEnded {
            agent: AgentId::main(),
            turn: codeapp_types::TurnId::new("trn_1"),
            usage: Some(Usage {
                input_tokens: 500,
                output_tokens: 100,
                reasoning_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            stop: TurnStop::Completed,
        },
        later,
    );

    let cell = &state.transcript.cells[0];
    assert!(cell.stats.is_some());
    let stats = cell.stats.as_ref().unwrap();
    // 2s duration (2.0s), 100 tokens / 2.0s = 50.0 tok/s, out 100, in 500
    assert_eq!(stats, "2.0s · 50.0 tok/s · ↑100 ↓500");
    assert_eq!(cell.version, initial_version + 1);
}

#[test]
fn test_turn_ended_zero_tokens_no_stats() {
    use crate::state::AppState;
    use codeapp_types::stream::Usage;
    use codeapp_types::{Config, Event, TurnStop};

    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    let now = Instant::now();

    state.apply_event(
        1,
        Event::ItemAdded(TranscriptItem {
            seq: 1,
            agent: AgentId::main(),
            ts_ms: 1000,
            kind: TranscriptKind::Assistant {
                text: "Hello".into(),
                complete: true,
            },
        }),
        now,
    );

    state.apply_event(
        2,
        Event::TurnEnded {
            agent: AgentId::main(),
            turn: codeapp_types::TurnId::new("trn_1"),
            usage: Some(Usage {
                input_tokens: 500,
                output_tokens: 0,
                reasoning_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            stop: TurnStop::Completed,
        },
        now,
    );

    assert_eq!(state.transcript.cells[0].stats, None);
}
