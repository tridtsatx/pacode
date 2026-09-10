use super::*;
use pacode_types::AgentId;
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
    use pacode_types::stream::Usage;
    use pacode_types::{Config, Event, TurnStop};

    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    let now = Instant::now();

    // Start turn
    state.apply_event(
        1,
        Event::TurnStarted {
            agent: AgentId::main(),
            turn: pacode_types::TurnId::new("trn_1"),
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
            turn: pacode_types::TurnId::new("trn_1"),
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
    assert!(stats.contains("2.0s"));
    assert_eq!(cell.version, initial_version + 1);
}

#[test]
fn test_header_is_not_a_cell_and_survives_a_seq_zero_item() {
    let mut t = Transcript::new(2);
    t.set_header(HeaderInfo {
        version: "0.1.0".into(),
        day: 0,
        mascot: crate::ui::mascot::MascotKind::Pacman,
        truecolor: true,
    });
    assert!(t.cells.is_empty(), "header must not occupy a cell");

    let now = Instant::now();
    for i in 0..=3 {
        t.upsert(
            TranscriptItem {
                seq: i,
                agent: AgentId::main(),
                ts_ms: i,
                kind: TranscriptKind::User {
                    text: format!("{i}"),
                },
            },
            now,
        );
    }

    // Item seq 0 no longer collides with the header, and the cap counts items only.
    assert_eq!(t.cells.len(), 2);
    assert_eq!(t.cells[0].id, 2);
    assert_eq!(t.cells[1].id, 3);
    assert_eq!(
        t.header,
        Some(HeaderInfo {
            version: "0.1.0".into(),
            day: 0,
            mascot: crate::ui::mascot::MascotKind::Pacman,
            truecolor: true,
        })
    );
}

#[test]
fn test_set_header_is_idempotent() {
    let mut t = Transcript::new(4);
    let info = HeaderInfo {
        version: "0.1.0".into(),
        day: 0,
        mascot: crate::ui::mascot::MascotKind::Pacman,
        truecolor: true,
    };
    t.set_header(info.clone());
    let v = t.header_version;
    t.set_header(info);
    assert_eq!(
        t.header_version, v,
        "identical header must not bump version"
    );
    t.set_header(HeaderInfo {
        version: "0.2.0".into(),
        day: 0,
        mascot: crate::ui::mascot::MascotKind::Pacman,
        truecolor: true,
    });
    assert_ne!(t.header_version, v);
}

#[test]
fn test_turn_ended_zero_tokens_no_stats() {
    use crate::state::AppState;
    use pacode_types::stream::Usage;
    use pacode_types::{Config, Event, TurnStop};

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
            turn: pacode_types::TurnId::new("trn_1"),
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

#[test]
fn test_reset_keeps_the_daemon_page_whole() {
    let mut t = Transcript::new(500);
    let items: Vec<TranscriptItem> = (0..150)
        .map(|i| TranscriptItem {
            seq: i,
            agent: AgentId::main(),
            ts_ms: i * 10,
            kind: TranscriptKind::User {
                text: format!("msg {i}"),
            },
        })
        .collect();

    // The daemon bounds the page it sends and says whether anything older exists;
    // the client keeps that page whole rather than capping it again.
    t.reset(items, false);
    assert_eq!(t.cells.len(), 150);
    assert!(!t.has_more_history);
    assert_eq!(t.cells.front().map(|c| c.id), Some(0));
    assert_eq!(t.cells.back().map(|c| c.id), Some(149));
}

#[test]
fn test_lazy_loader_prepend_keeps_anchored_cell_position() {
    let mut t = Transcript::new(500);
    let initial_items: Vec<TranscriptItem> = (10..30)
        .map(|i| TranscriptItem {
            seq: i,
            agent: AgentId::main(),
            ts_ms: i * 10,
            kind: TranscriptKind::User {
                text: format!("msg {i}"),
            },
        })
        .collect();

    t.reset(initial_items, true);
    let viewport = 10;
    // Scroll to the top (max_scroll = 20 - 10 = 10)
    t.record_render(20, viewport);
    t.scroll_by(100);
    assert_eq!(t.scroll_from_bottom, 10);

    // Anchor cell is cell id 10 at the top of the viewport
    let anchor_id = 10;
    let pos_before = t.cell_screen_position(anchor_id, viewport);
    assert_eq!(pos_before, Some(0));

    // Also check another cell in the viewport (cell 15 at screen line 5)
    let mid_id = 15;
    let pos_mid_before = t.cell_screen_position(mid_id, viewport);
    assert_eq!(pos_mid_before, Some(5));

    // Prepend 10 older items (ids 0..10)
    let older: Vec<TranscriptItem> = (0..10)
        .map(|i| TranscriptItem {
            seq: i,
            agent: AgentId::main(),
            ts_ms: i * 10,
            kind: TranscriptKind::User {
                text: format!("msg {i}"),
            },
        })
        .collect();
    t.prepend(older, false);

    // The anchored cells must remain at the exact same screen position
    let pos_after = t.cell_screen_position(anchor_id, viewport);
    assert_eq!(pos_after, pos_before);
    assert_eq!(pos_after, Some(0));

    let pos_mid_after = t.cell_screen_position(mid_id, viewport);
    assert_eq!(pos_mid_after, pos_mid_before);
    assert_eq!(pos_mid_after, Some(5));
}

#[test]
fn test_scroll_fewer_lines_than_viewport_does_not_move() {
    let mut t = Transcript::new(100);
    t.record_render(10, 25);
    assert_eq!(t.scroll_from_bottom, 0);

    t.scroll_by(5);
    assert_eq!(t.scroll_from_bottom, 0);

    t.scroll_by(100);
    assert_eq!(t.scroll_from_bottom, 0);

    t.scroll_to_top();
    assert_eq!(t.scroll_from_bottom, 0);
}

#[test]
fn test_scroll_up_stops_exactly_at_first_line_and_repeated_presses_do_not_move() {
    let mut t = Transcript::new(100);
    // 50 total lines, 20 viewport -> max scroll is 30
    t.record_render(50, 20);
    assert_eq!(t.scroll_from_bottom, 0);

    t.scroll_by(10);
    assert_eq!(t.scroll_from_bottom, 10);

    t.scroll_by(15);
    assert_eq!(t.scroll_from_bottom, 25);

    // Reaches exact ceiling (30)
    t.scroll_by(10);
    assert_eq!(t.scroll_from_bottom, 30);

    // Repeated presses do not move further
    t.scroll_by(5);
    assert_eq!(t.scroll_from_bottom, 30);

    t.scroll_by(1000);
    assert_eq!(t.scroll_from_bottom, 30);

    t.scroll_to_top();
    assert_eq!(t.scroll_from_bottom, 30);
}

#[test]
fn test_is_at_top_true_only_when_oldest_line_is_on_screen() {
    let mut t = Transcript::new(100);

    // Before first draw, rendered total is unknown: is_at_top must be false
    assert!(!t.is_at_top());

    // 50 total lines, 20 viewport -> max scroll is 30
    t.record_render(50, 20);
    assert!(!t.is_at_top());

    t.scroll_by(15);
    assert!(!t.is_at_top());

    t.scroll_by(14);
    // At scroll 29, line 1 is visible at top, but oldest line (line 0) is not yet
    assert_eq!(t.scroll_from_bottom, 29);
    assert!(!t.is_at_top());

    // At scroll 30, line 0 is on screen
    t.scroll_by(1);
    assert_eq!(t.scroll_from_bottom, 30);
    assert!(t.is_at_top());

    // With fewer lines than viewport, all lines fit on screen so oldest line is on screen
    let mut t_small = Transcript::new(100);
    t_small.record_render(10, 25);
    assert!(t_small.is_at_top());

    // With 0 lines rendered, is_at_top is false
    let mut t_empty = Transcript::new(100);
    t_empty.record_render(0, 25);
    assert!(!t_empty.is_at_top());
}

#[test]
fn test_scroll_down_always_returns_to_bottom() {
    let mut t = Transcript::new(100);
    t.record_render(50, 20);
    t.scroll_to_top();
    assert_eq!(t.scroll_from_bottom, 30);

    t.scroll_by(-10);
    assert_eq!(t.scroll_from_bottom, 20);

    t.scroll_by(-15);
    assert_eq!(t.scroll_from_bottom, 5);

    // Negative scroll past 0 clamps to 0
    t.scroll_by(-10);
    assert_eq!(t.scroll_from_bottom, 0);

    // Repeated downward scrolls remain at 0
    t.scroll_by(-5);
    assert_eq!(t.scroll_from_bottom, 0);

    // scroll_to_bottom returns to 0
    t.scroll_by(20);
    assert_eq!(t.scroll_from_bottom, 20);
    t.scroll_to_bottom();
    assert_eq!(t.scroll_from_bottom, 0);
}

/// Reasoning and the answer are separate cells fed by one paced stream. When the
/// live cell moved, the previous cell's undrained tail used to be replayed into
/// the new cell, which left the first grapheme of the answer stranded in the
/// reasoning cell above it.
#[test]
fn switching_the_live_cell_does_not_move_text_between_cells() {
    use pacode_types::{AgentId, TranscriptItem, TranscriptKind};

    let mut t = Transcript::new(64);
    let now = Instant::now();
    let agent = AgentId::main();

    // Reasoning starts and buffers some text that has not been revealed yet.
    t.upsert(
        TranscriptItem {
            seq: 1,
            agent: agent.clone(),
            ts_ms: 0,
            kind: TranscriptKind::Reasoning {
                text: String::new(),
                complete: false,
            },
        },
        now,
    );
    t.push_delta(1, "weighing the options", true, now);

    // The answer begins before that backlog drained.
    t.upsert(
        TranscriptItem {
            seq: 2,
            agent: agent.clone(),
            ts_ms: 1,
            kind: TranscriptKind::Assistant {
                text: String::new(),
                complete: false,
            },
        },
        now,
    );
    t.push_delta(2, "No game running, safe to build.", false, now);
    t.flush_stream();

    let reasoning = t
        .cells
        .iter()
        .find(|c| c.id == 1)
        .map(|c| c.kind.clone())
        .expect("reasoning cell");
    let answer = t
        .cells
        .iter()
        .find(|c| c.id == 2)
        .map(|c| c.kind.clone())
        .expect("answer cell");

    match reasoning {
        CellKind::Item(TranscriptKind::Reasoning { text, .. }) => {
            assert_eq!(text, "weighing the options");
        }
        other => panic!("the reasoning cell was rewritten: {other:?}"),
    }
    match answer {
        CellKind::Item(TranscriptKind::Assistant { text, .. }) => {
            assert_eq!(text, "No game running, safe to build.");
        }
        other => panic!("expected an answer cell, got {other:?}"),
    }
}

#[test]
fn a_delta_for_an_unknown_cell_is_ignored_and_leaves_the_live_cell_alone() {
    use pacode_types::{AgentId, TranscriptItem, TranscriptKind};

    let mut t = Transcript::new(64);
    let now = Instant::now();
    t.upsert(
        TranscriptItem {
            seq: 7,
            agent: AgentId::main(),
            ts_ms: 0,
            kind: TranscriptKind::Assistant {
                text: String::new(),
                complete: false,
            },
        },
        now,
    );
    t.push_delta(7, "hello", false, now);
    t.push_delta(999, "stray", false, now);
    t.flush_stream();

    assert_eq!(t.cells.len(), 1);
    match &t.cells[0].kind {
        CellKind::Item(TranscriptKind::Assistant { text, .. }) => assert_eq!(text, "hello"),
        other => panic!("unexpected cell {other:?}"),
    }
}

fn tool_cell(id: u64, preview: &str) -> Cell {
    Cell {
        id,
        kind: CellKind::Item(pacode_types::TranscriptKind::ToolCall {
            call_id: pacode_types::CallId::new(format!("call_{id}")),
            name: "bash".into(),
            title: "bash echo".into(),
            intent: None,
            status: pacode_types::ToolStatus::Ok,
            preview: preview.to_string(),
            diff: None,
            duration_ms: Some(10),
            task: None,
        }),
        version: 0,
        ts_ms: id,
        stats: None,
    }
}

#[test]
fn clicking_a_cell_toggles_its_expansion_and_an_unknown_cell_is_ignored() {
    let mut t = Transcript::new(8);
    t.cells.push_back(tool_cell(1, "line one\nline two"));

    assert!(!t.is_expanded(1));
    assert!(t.toggle_expanded(1));
    assert!(t.is_expanded(1));
    assert!(t.toggle_expanded(1));
    assert!(!t.is_expanded(1));

    // A cell that is not there cannot be expanded.
    assert!(!t.toggle_expanded(99));
    assert!(!t.is_expanded(99));
}

#[test]
fn an_evicted_cell_takes_its_expansion_with_it() {
    let mut t = Transcript::new(2);
    t.cells.push_back(tool_cell(1, "a"));
    t.cells.push_back(tool_cell(2, "b"));
    t.toggle_expanded(1);
    assert!(t.is_expanded(1));

    t.cells.push_back(tool_cell(3, "c"));
    t.enforce_cap();

    assert_eq!(t.cells.len(), 2);
    assert!(
        !t.is_expanded(1),
        "an evicted cell must not leave its id behind"
    );
}

#[test]
fn a_content_line_maps_back_to_the_cell_drawn_there() {
    let mut t = Transcript::new(8);
    t.cell_lines = vec![(10, 0, 3), (11, 3, 9), (12, 9, 10)];

    assert_eq!(t.cell_at_line(0), Some(10));
    assert_eq!(t.cell_at_line(2), Some(10));
    assert_eq!(t.cell_at_line(3), Some(11));
    assert_eq!(t.cell_at_line(8), Some(11));
    assert_eq!(t.cell_at_line(9), Some(12));
    assert_eq!(t.cell_at_line(10), None);
}
