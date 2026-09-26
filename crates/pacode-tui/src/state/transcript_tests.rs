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
fn cap_does_not_evict_the_new_tail_to_make_room_for_a_pinned_live_cell() {
    let mut t = Transcript::new(1);
    let now = Instant::now();
    t.upsert(
        TranscriptItem {
            seq: 1,
            agent: AgentId::main(),
            ts_ms: 1,
            kind: TranscriptKind::Assistant {
                text: String::new(),
                complete: false,
            },
        },
        now,
    );
    t.upsert(
        TranscriptItem {
            seq: 2,
            agent: AgentId::main(),
            ts_ms: 2,
            kind: TranscriptKind::User {
                text: "tail".into(),
            },
        },
        now,
    );

    assert_eq!(
        t.cells.iter().map(|cell| cell.id).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(t.live_cell, Some(1));
}

#[test]
fn prepend_over_cap_advances_history_and_keeps_the_live_tail() {
    let mut t = Transcript::new(3);
    let now = Instant::now();
    for seq in 10..=11 {
        t.upsert(
            TranscriptItem {
                seq,
                agent: AgentId::main(),
                ts_ms: seq,
                kind: TranscriptKind::User {
                    text: seq.to_string(),
                },
            },
            now,
        );
    }
    t.upsert(
        TranscriptItem {
            seq: 12,
            agent: AgentId::main(),
            ts_ms: 12,
            kind: TranscriptKind::Assistant {
                text: String::new(),
                complete: false,
            },
        },
        now,
    );

    let shift = t.prepend(
        (7..=9)
            .map(|seq| TranscriptItem {
                seq,
                agent: AgentId::main(),
                ts_ms: seq,
                kind: TranscriptKind::User {
                    text: seq.to_string(),
                },
            })
            .collect(),
        false,
    );

    assert_eq!(shift, 0);
    assert_eq!(
        t.cells.iter().map(|cell| cell.id).collect::<Vec<_>>(),
        vec![7, 8, 9, 12]
    );
    assert_eq!(t.oldest_seq(), Some(7));
    assert_eq!(t.live_cell, Some(12));
}

#[test]
fn a_late_update_cannot_resurrect_an_evicted_cell() {
    let mut t = Transcript::new(2);
    let now = Instant::now();
    for seq in 1..=3 {
        t.upsert(
            TranscriptItem {
                seq,
                agent: AgentId::main(),
                ts_ms: seq,
                kind: TranscriptKind::User {
                    text: seq.to_string(),
                },
            },
            now,
        );
    }

    t.upsert(
        TranscriptItem {
            seq: 1,
            agent: AgentId::main(),
            ts_ms: 4,
            kind: TranscriptKind::Notice {
                level: pacode_types::ToastLevel::Info,
                text: "late".into(),
            },
        },
        now,
    );

    assert_eq!(
        t.cells.iter().map(|cell| cell.id).collect::<Vec<_>>(),
        vec![2, 3]
    );
}

#[test]
fn a_late_update_cannot_resurrect_a_tail_evicted_by_pagination() {
    let mut t = Transcript::new(3);
    let now = Instant::now();
    for seq in 10..=12 {
        t.upsert(
            TranscriptItem {
                seq,
                agent: AgentId::main(),
                ts_ms: seq,
                kind: TranscriptKind::User {
                    text: seq.to_string(),
                },
            },
            now,
        );
    }
    t.prepend(
        (7..=9)
            .map(|seq| TranscriptItem {
                seq,
                agent: AgentId::main(),
                ts_ms: seq,
                kind: TranscriptKind::User {
                    text: seq.to_string(),
                },
            })
            .collect(),
        false,
    );
    t.upsert(
        TranscriptItem {
            seq: 11,
            agent: AgentId::main(),
            ts_ms: 11,
            kind: TranscriptKind::Notice {
                level: pacode_types::ToastLevel::Info,
                text: "late".into(),
            },
        },
        now,
    );

    assert_eq!(
        t.cells.iter().map(|cell| cell.id).collect::<Vec<_>>(),
        vec![7, 8, 9]
    );
}

#[test]
fn prepend_keeps_a_multiline_anchor_at_the_same_viewport_position() {
    let page: Vec<_> = (7..9)
        .map(|seq| TranscriptItem {
            seq,
            agent: AgentId::main(),
            ts_ms: seq,
            kind: TranscriptKind::User {
                text: format!("{seq} first\n{seq} second"),
            },
        })
        .collect();
    let opts = pacode_render::RenderOptions::new(30, false);
    let mut expected = Transcript::new(16);
    expected.reset(page.clone(), false);
    let expected_shift = crate::ui::dialog::plain_lines(
        &mut expected,
        30,
        &opts,
        0,
        0,
        crate::ui::dialog::PlainWindow {
            lines: None,
            max_bytes: crate::state::selection::MAX_SELECTION_BYTES,
        },
    )
    .lines
    .len();

    let mut t = Transcript::new(16);
    t.reset(
        (10..13)
            .map(|seq| TranscriptItem {
                seq,
                agent: AgentId::main(),
                ts_ms: seq,
                kind: TranscriptKind::User {
                    text: format!("{seq} first\n{seq} second"),
                },
            })
            .collect(),
        true,
    );
    let area = ratatui::layout::Rect::new(0, 0, 30, 2);
    crate::ui::dialog::prepare(area, &mut t, &opts, 0, 0);
    t.scroll_to_top();
    let before = t.cell_screen_position(10, 2);
    assert_eq!(before, Some(0));

    let page_len = page.len();
    let shift = t.prepend(page, false);
    assert_eq!(shift, expected_shift);
    assert!(shift > page_len);

    crate::ui::dialog::prepare(area, &mut t, &opts, 0, 0);
    assert_eq!(t.cell_screen_position(10, 2), before);
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
fn reset_clears_render_and_expansion_metadata() {
    let mut t = Transcript::new(8);
    t.cells.push_back(tool_cell(1, "a\nb"));
    t.toggle_expanded(1);
    t.cell_lines = vec![(1, 0, 2)];
    t.record_render(20, 5);
    t.scroll_by(10);
    t.first_visible_line = 7;

    t.reset(Vec::new(), false);

    assert!(t.expanded.is_empty());
    assert!(t.cell_lines.is_empty());
    assert!(t.cell_line_index.is_empty());
    assert!(t.cells.is_empty());
    assert_eq!(t.bytes(), 0);
    assert_eq!(t.scroll_from_bottom, 0);
    assert_eq!(t.first_visible_line, 0);
    assert_eq!(t.rendered_lines, None);
    assert_eq!(t.viewport_height, None);
}

#[test]
fn test_lazy_loader_prepend_keeps_anchored_cell_position() {
    let mut t = Transcript::new(500);
    let initial_items: Vec<TranscriptItem> = (10..30)
        .map(|i| TranscriptItem {
            seq: i,
            agent: AgentId::main(),
            ts_ms: i * 10,
            kind: TranscriptKind::Assistant {
                text: String::new(),
                complete: true,
            },
        })
        .collect();

    t.reset(initial_items, true);
    let opts = pacode_render::RenderOptions::new(80, false);
    let area = ratatui::layout::Rect::new(0, 0, 80, 10);
    crate::ui::dialog::prepare(area, &mut t, &opts, 0, 0);
    t.scroll_to_top();

    let anchor_id = 10;
    let pos_before = t.cell_screen_position(anchor_id, area.height as usize);
    assert_eq!(pos_before, Some(0));

    let mid_id = 15;
    let pos_mid_before = t.cell_screen_position(mid_id, area.height as usize);
    assert_eq!(pos_mid_before, Some(5));

    let older: Vec<TranscriptItem> = (0..10)
        .map(|i| TranscriptItem {
            seq: i,
            agent: AgentId::main(),
            ts_ms: i * 10,
            kind: TranscriptKind::Assistant {
                text: String::new(),
                complete: true,
            },
        })
        .collect();
    t.prepend(older, false);
    crate::ui::dialog::prepare(area, &mut t, &opts, 0, 0);

    assert_eq!(
        t.cell_screen_position(anchor_id, area.height as usize),
        pos_before
    );
    assert_eq!(
        t.cell_screen_position(mid_id, area.height as usize),
        pos_mid_before
    );
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

fn tool_item(seq: u64, agent: &AgentId) -> TranscriptItem {
    TranscriptItem {
        seq,
        agent: agent.clone(),
        ts_ms: seq,
        kind: TranscriptKind::ToolCall {
            call_id: pacode_types::CallId::new(format!("call_{seq}")),
            name: "bash".into(),
            title: "bash echo".into(),
            intent: None,
            status: pacode_types::ToolStatus::Running,
            preview: String::new(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    }
}

/// A tool item arriving while the paced reveal still holds buffered text used
/// to be appended below the unrevealed backlog, which split the answer around
/// the tool. A terminal cell now flushes the backlog first, so the on-screen
/// order matches the wire order.
#[test]
fn a_tool_cell_lands_after_the_answer_text_it_follows_on_the_wire() {
    let mut t = Transcript::new(64);
    let now = Instant::now();
    let agent = AgentId::main();

    t.upsert(
        TranscriptItem {
            seq: 1,
            agent: agent.clone(),
            ts_ms: 0,
            kind: TranscriptKind::Assistant {
                text: String::new(),
                complete: false,
            },
        },
        now,
    );
    // Buffered into the paced stream but not yet revealed into the cell.
    t.push_delta(1, "part one", false, now);
    assert!(t.has_backlog());

    t.upsert(tool_item(2, &agent), now);

    assert_eq!(t.cells.len(), 2);
    assert_eq!(t.cells[0].id, 1);
    assert_eq!(t.cells[1].id, 2);
    // The backlog was flushed into the answer cell, not dropped.
    match &t.cells[0].kind {
        CellKind::Item(TranscriptKind::Assistant { text, .. }) => {
            assert_eq!(text, "part one");
        }
        other => panic!("unexpected cell {other:?}"),
    }
}

/// Deltas that arrive after the tool cell still belong to the answer cell
/// above it — the tool keeps its position and the answer keeps growing.
#[test]
fn deltas_after_a_tool_cell_still_reach_the_answer_above_it() {
    let mut t = Transcript::new(64);
    let now = Instant::now();
    let agent = AgentId::main();

    t.upsert(
        TranscriptItem {
            seq: 1,
            agent: agent.clone(),
            ts_ms: 0,
            kind: TranscriptKind::Assistant {
                text: String::new(),
                complete: false,
            },
        },
        now,
    );
    t.push_delta(1, "part one", false, now);
    t.upsert(tool_item(2, &agent), now);

    t.push_delta(1, " and part two", false, now);
    t.flush_stream();

    assert_eq!(t.cells.len(), 2);
    assert_eq!(t.cells[0].id, 1);
    assert_eq!(t.cells[1].id, 2);
    match &t.cells[0].kind {
        CellKind::Item(TranscriptKind::Assistant { text, .. }) => {
            assert_eq!(text, "part one and part two");
        }
        other => panic!("unexpected cell {other:?}"),
    }
}

/// A completed answer waiting behind a drained-looking backlog (`pending_final`)
/// is flushed by a terminal cell too, not just raw stream text.
#[test]
fn a_terminal_cell_flushes_a_pending_final() {
    let mut t = Transcript::new(64);
    let now = Instant::now();
    let agent = AgentId::main();

    t.upsert(
        TranscriptItem {
            seq: 1,
            agent: agent.clone(),
            ts_ms: 0,
            kind: TranscriptKind::Assistant {
                text: String::new(),
                complete: false,
            },
        },
        now,
    );
    t.push_delta(1, "part one", false, now);
    // The finished item waits for the backlog instead of cutting in.
    t.upsert(
        TranscriptItem {
            seq: 1,
            agent: agent.clone(),
            ts_ms: 1,
            kind: TranscriptKind::Assistant {
                text: "part one".into(),
                complete: true,
            },
        },
        now,
    );
    assert!(t.has_pending_final());

    t.push_cell(tool_cell(2, "done"));

    assert_eq!(t.cells.len(), 2);
    assert_eq!(t.cells[0].id, 1);
    assert!(
        is_client_id(t.cells[1].id),
        "a client row carries a client id, never a daemon seq"
    );
    match &t.cells[0].kind {
        CellKind::Item(TranscriptKind::Assistant { text, complete }) => {
            assert_eq!(text, "part one");
            assert!(complete);
        }
        other => panic!("unexpected cell {other:?}"),
    }
}

/// Updating a cell that already exists replaces it in place: it cannot jump
/// the queue, so it must not flush the backlog either.
#[test]
fn updating_an_existing_cell_does_not_flush_or_reorder() {
    let mut t = Transcript::new(64);
    let now = Instant::now();
    let agent = AgentId::main();

    t.upsert(
        TranscriptItem {
            seq: 1,
            agent: agent.clone(),
            ts_ms: 0,
            kind: TranscriptKind::Assistant {
                text: String::new(),
                complete: false,
            },
        },
        now,
    );
    t.push_delta(1, "part one", false, now);
    t.upsert(tool_item(2, &agent), now);
    assert_eq!(t.cells.len(), 2);

    // More text streams in after the tool landed — backlog again.
    t.push_delta(1, " more", false, now);
    assert!(t.has_backlog());

    // An `ItemUpdated` for the existing tool cell replaces it in place and
    // leaves the pending backlog alone.
    let mut updated = tool_item(2, &agent);
    updated.kind = TranscriptKind::ToolCall {
        call_id: pacode_types::CallId::new("call_2"),
        name: "bash".into(),
        title: "bash echo".into(),
        intent: None,
        status: pacode_types::ToolStatus::Ok,
        preview: "done".into(),
        diff: None,
        duration_ms: Some(10),
        task: None,
    };
    t.upsert(updated, now);

    assert_eq!(t.cells.len(), 2);
    assert_eq!(t.cells[0].id, 1);
    assert_eq!(t.cells[1].id, 2);
    assert!(
        t.has_backlog(),
        "an in-place update must not drain the stream"
    );
    match &t.cells[0].kind {
        CellKind::Item(TranscriptKind::Assistant { text, .. }) => {
            assert_eq!(text, "part one");
        }
        other => panic!("unexpected cell {other:?}"),
    }
    match &t.cells[1].kind {
        CellKind::Item(TranscriptKind::ToolCall {
            preview, status, ..
        }) => {
            assert_eq!(preview, "done");
            assert_eq!(*status, pacode_types::ToolStatus::Ok);
        }
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
        perm_explain: None,
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
fn ctrl_o_expands_the_newest_tool_cell_and_collapses_it_back() {
    let mut t = Transcript::new(8);
    let long: String = (1..=12)
        .map(|i| format!("row {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    t.cells.push_back(tool_cell(1, "short"));
    t.cells.push_back(tool_cell(2, &long));
    t.cells.push_back(tool_cell(3, "tiny"));

    // The newest tool cell is always the target — expansion also reveals the
    // call's full argument, so a short preview is still worth expanding.
    assert!(t.toggle_last_expandable());
    assert!(t.is_expanded(3));
    assert!(!t.is_expanded(1));
    assert!(!t.is_expanded(2));

    // Second press toggles the same cell back.
    assert!(t.toggle_last_expandable());
    assert!(!t.is_expanded(3));
}

#[test]
fn ctrl_o_expands_short_and_denied_cells_too() {
    // A single-line output still expands — the expanded cell also spells out
    // the call's full argument.
    let mut t = Transcript::new(8);
    t.cells.push_back(tool_cell(1, "only one line"));
    assert!(t.toggle_last_expandable());
    assert!(t.is_expanded(1));

    // A denied call expands too: its one-line "Denied" preview used to make
    // the cell look unexpandable, but the refused command is exactly what the
    // reader wants to see.
    let mut denied_cell = tool_cell(1, "Denied");
    if let CellKind::Item(TranscriptKind::ToolCall { ref mut status, .. }) = denied_cell.kind {
        *status = pacode_types::ToolStatus::Denied;
    }
    let mut t = Transcript::new(8);
    t.cells.push_back(denied_cell);
    assert!(t.toggle_last_expandable());
    assert!(t.is_expanded(1));

    // With no tool cell at all there is nothing to toggle.
    let mut t = Transcript::new(8);
    assert!(!t.toggle_last_expandable());
}

#[test]
fn an_evicted_cell_takes_its_expansion_with_it() {
    let mut t = Transcript::new(2);
    t.cells.push_back(tool_cell(1, "a"));
    t.cells.push_back(tool_cell(2, "b"));
    t.toggle_expanded(1);
    assert!(t.is_expanded(1));

    t.cells.push_back(tool_cell(3, "c"));
    t.set_max_cells(2);

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

fn perm_request(id: &str, title: &str) -> PermissionRequest {
    PermissionRequest {
        id: PermissionId::new(id),
        agent: AgentId::main(),
        agent_name: "main".into(),
        call_id: pacode_types::CallId::new("call_1"),
        tool: "bash".into(),
        title: title.into(),
        detail: String::new(),
        risk: Some(pacode_types::RiskLevel::Confirm),
        created_at_ms: 1,
    }
}

/// The daemon emits the same request as an event AND as a transcript item
/// (and again in a snapshot's `pending_permissions`) — all three writes must
/// land on one prompt cell.
#[test]
fn a_permission_request_lands_once_no_matter_how_often_it_arrives() {
    let mut t = Transcript::new(16);
    t.upsert_permission(perm_request("perm_1", "Bash: rm -rf /tmp/x"), 1, 10);
    // The ItemAdded echo — a different seq, the same request id.
    t.upsert(
        TranscriptItem {
            seq: 42,
            agent: AgentId::main(),
            ts_ms: 2,
            kind: TranscriptKind::Permission(perm_request("perm_1", "Bash: rm -rf /tmp/x")),
        },
        Instant::now(),
    );
    // …and the snapshot's pending_permissions repeat.
    t.upsert_permission(perm_request("perm_1", "Bash: rm -rf /tmp/x"), 3, 99);

    let prompts: Vec<&Cell> = t
        .cells
        .iter()
        .filter(|c| matches!(c.kind, CellKind::Item(TranscriptKind::Permission(_))))
        .collect();
    assert_eq!(prompts.len(), 1, "one request must render one prompt");
    // The first write keeps the slot; the echoes refresh it in place. The slot
    // is filed under a client id: the seqs handed to `upsert_permission` come
    // from the event counter, not from the transcript.
    assert!(is_client_id(prompts[0].id));
    assert_eq!(prompts[0].ts_ms, 3);

    // A different request is a different prompt.
    t.upsert_permission(perm_request("perm_2", "Bash: ls"), 4, 12);
    assert_eq!(t.cells.len(), 2);
}

#[test]
fn set_perm_explain_marks_the_prompt_and_reports_a_miss() {
    let mut t = Transcript::new(16);
    t.upsert_permission(perm_request("perm_1", "Bash: rm -rf /tmp/x"), 1, 10);

    assert!(t.set_perm_explain(&PermissionId::new("perm_1"), &PermExplain::Requested));
    assert_eq!(t.cells[0].perm_explain, Some(PermExplain::Requested));

    // A landing answer replaces the mark rather than stacking.
    let done = PermExplain::Done {
        text: "deletes the dir".into(),
        level: Some(ExplainLevel::High),
    };
    assert!(t.set_perm_explain(&PermissionId::new("perm_1"), &done));
    assert_eq!(t.cells[0].perm_explain, Some(done));

    // An unknown id lands nowhere.
    assert!(!t.set_perm_explain(&PermissionId::new("perm_zzz"), &PermExplain::Requested));

    // Once the verdict swapped the prompt for a notice, a late answer has no
    // target — the caller falls back to a notice of its own.
    t.cells[0].kind = CellKind::Item(TranscriptKind::Notice {
        level: pacode_types::ToastLevel::Warn,
        text: "Denied".into(),
    });
    assert!(!t.set_perm_explain(&PermissionId::new("perm_1"), &PermExplain::Requested));
}

fn user_item(seq: u64, text: &str) -> TranscriptItem {
    TranscriptItem {
        seq,
        agent: AgentId::main(),
        ts_ms: seq,
        kind: TranscriptKind::User {
            text: text.to_string(),
        },
    }
}

fn notice(ts_ms: u64, text: &str) -> Cell {
    Cell {
        id: ts_ms,
        kind: CellKind::Item(TranscriptKind::Notice {
            level: pacode_types::ToastLevel::Info,
            text: text.to_string(),
        }),
        version: 0,
        ts_ms,
        stats: None,
        perm_explain: None,
    }
}

#[test]
fn two_client_rows_in_the_same_millisecond_are_two_rows() {
    let mut t = Transcript::new(16);
    t.upsert(user_item(1, "hello"), Instant::now());
    t.push_cell(notice(1_700_000_000_000, "first"));
    t.push_cell(notice(1_700_000_000_000, "second"));

    assert_eq!(t.cells.len(), 3, "a shared clock must not merge two rows");
    let ids: Vec<u64> = t.cells.iter().map(|cell| cell.id).collect();
    assert_eq!(ids[0], 1);
    assert!(is_client_id(ids[1]) && is_client_id(ids[2]));
    assert_ne!(ids[1], ids[2]);
}

#[test]
fn a_client_row_never_lands_on_a_transcript_item() {
    let mut t = Transcript::new(16);
    t.upsert(user_item(7, "kept"), Instant::now());
    // The event counter hands the client the same number the item counter uses.
    t.push_cell(notice(7, "event 7"));

    assert_eq!(t.cells.len(), 2);
    assert_eq!(t.cells[0].id, 7);
    match &t.cells[0].kind {
        CellKind::Item(TranscriptKind::User { text }) => assert_eq!(text, "kept"),
        other => panic!("the item was replaced: {other:?}"),
    }
}

#[test]
fn a_client_row_never_moves_the_eviction_floor_over_a_real_seq() {
    let mut t = Transcript::new(3);
    let now = Instant::now();
    for seq in 10..=12 {
        t.upsert(user_item(seq, &seq.to_string()), now);
    }
    // A notice whose id is a wall clock: eviction must not read it as a seq.
    t.push_cell(notice(1_700_000_000_000, "paged"));
    t.prepend(
        (7..=9)
            .map(|seq| user_item(seq, &seq.to_string()))
            .collect(),
        false,
    );

    t.upsert(user_item(13, "late"), now);
    t.upsert(notice_item(14, "late notice"), now);

    let ids: Vec<u64> = t.cells.iter().map(|cell| cell.id).collect();
    assert!(
        ids.contains(&13) && ids.contains(&14),
        "a client row must not push the floor past a real seq: {ids:?}"
    );
}

fn notice_item(seq: u64, text: &str) -> TranscriptItem {
    TranscriptItem {
        seq,
        agent: AgentId::main(),
        ts_ms: seq,
        kind: TranscriptKind::Notice {
            level: pacode_types::ToastLevel::Info,
            text: text.to_string(),
        },
    }
}

#[test]
fn a_history_cursor_never_points_at_a_client_row() {
    let mut t = Transcript::new(16);
    t.upsert(user_item(4, "oldest daemon row"), Instant::now());
    t.push_cell(notice(1_700_000_000_000, "notice"));
    assert_eq!(
        t.oldest_seq(),
        Some(4),
        "GetHistory must be asked for the oldest item seq, not a clock"
    );
}

#[test]
fn the_byte_cap_drops_the_oldest_rows_but_keeps_the_stream_and_the_prompt() {
    let mut t = Transcript::new(64);
    let now = Instant::now();
    t.set_max_bytes(4_000);
    t.upsert(
        TranscriptItem {
            seq: 1,
            agent: AgentId::main(),
            ts_ms: 1,
            kind: TranscriptKind::Assistant {
                text: "x".repeat(1_000),
                complete: false,
            },
        },
        now,
    );
    t.upsert_permission(perm_request("perm_1", "Bash: ls"), 2, 1_700_000_000_000);
    for seq in 2..8 {
        t.upsert(
            TranscriptItem {
                seq,
                agent: AgentId::main(),
                ts_ms: seq,
                kind: TranscriptKind::ToolCall {
                    call_id: pacode_types::CallId::new(format!("call_{seq}")),
                    name: "bash".into(),
                    title: "bash".into(),
                    intent: None,
                    status: pacode_types::ToolStatus::Ok,
                    preview: "y".repeat(1_000),
                    diff: None,
                    duration_ms: None,
                    task: None,
                },
            },
            now,
        );
    }

    assert!(t.bytes() > 4_000 || t.cells.len() < 8, "the cap must bite");
    assert_eq!(t.live_cell, Some(1), "the streaming row is pinned");
    assert!(
        t.cells
            .iter()
            .any(|cell| matches!(&cell.kind, CellKind::Item(TranscriptKind::Permission(_)))),
        "a prompt waiting for a verdict is pinned"
    );
    assert_eq!(
        t.oldest_seq(),
        Some(1),
        "the oldest row is the one that goes"
    );
}

#[test]
fn an_update_after_an_eviction_replaces_the_row_instead_of_copying_it() {
    let mut t = Transcript::new(2);
    let now = Instant::now();
    t.upsert(user_item(1, "one"), now);
    t.upsert(user_item(2, "two"), now);
    t.upsert(user_item(3, "three"), now);
    assert_eq!(
        t.cells.iter().map(|cell| cell.id).collect::<Vec<_>>(),
        vec![2, 3]
    );

    t.upsert(user_item(2, "two again"), now);
    t.upsert(user_item(3, "three again"), now);
    assert_eq!(
        t.cells.iter().map(|cell| cell.id).collect::<Vec<_>>(),
        vec![2, 3],
        "an eviction that left the length unchanged must not stale the lookup"
    );
    assert_eq!(t.cells.len(), 2);
}

#[test]
fn a_repeated_permission_in_a_history_page_does_not_prompt_twice() {
    let mut t = Transcript::new(16);
    t.upsert_permission(perm_request("perm_1", "Bash: ls"), 1, 1_700_000_000_000);
    t.prepend(
        vec![TranscriptItem {
            seq: 3,
            agent: AgentId::main(),
            ts_ms: 3,
            kind: TranscriptKind::Permission(perm_request("perm_1", "Bash: ls")),
        }],
        false,
    );

    let prompts = t
        .cells
        .iter()
        .filter(|cell| matches!(cell.kind, CellKind::Item(TranscriptKind::Permission(_))))
        .count();
    assert_eq!(prompts, 1);
}

/// The same tool item once its call is over: the daemon's preview is the body.
fn finished_tool_item(seq: u64, preview: &str) -> TranscriptItem {
    let mut item = tool_item(seq, &AgentId::main());
    if let TranscriptKind::ToolCall {
        status,
        preview: body,
        ..
    } = &mut item.kind
    {
        *status = pacode_types::ToolStatus::Ok;
        *body = preview.to_string();
    }
    item
}

/// A live body belongs to the cell that is running. When the call finishes
/// the daemon's preview is the body, and a chunk that lands afterwards must
/// not resurrect the old one.
#[test]
fn a_tool_output_chunk_after_the_call_finished_is_dropped() {
    let mut t = Transcript::new(8);
    let now = Instant::now();
    t.upsert(tool_item(1, &AgentId::main()), now);
    t.push_tool_output(1, "Compiling\n");
    assert_eq!(t.tool_streams.get(1).map(|s| s.text), Some("Compiling\n"));

    t.upsert(finished_tool_item(1, "Compiled"), now);
    assert_eq!(t.tool_streams.get(1), None);

    t.push_tool_output(1, "late chunk\n");
    assert_eq!(
        t.tool_streams.get(1),
        None,
        "a finished cell keeps no live body"
    );
}

/// An evicted cell's live body goes with it: the buffer is capped, but a
/// session that runs and scrolls thousands of calls must not sit on the
/// bodies of calls that left the screen.
#[test]
fn evicting_a_running_tool_cell_frees_its_live_body() {
    let mut t = Transcript::new(1);
    let now = Instant::now();
    t.upsert(tool_item(1, &AgentId::main()), now);
    t.push_tool_output(1, "one\n");
    t.upsert(tool_item(2, &AgentId::main()), now);
    t.push_tool_output(2, "two\n");

    assert_eq!(
        t.tool_streams.get(1),
        None,
        "the evicted call's body is gone"
    );
    assert_eq!(t.tool_streams.get(2).map(|s| s.text), Some("two\n"));
}

/// A delta for a cell that is not on screen at all (evicted, or a different
/// agent's transcript) is refused rather than buffered.
#[test]
fn a_tool_output_chunk_for_an_unknown_cell_is_refused() {
    let mut t = Transcript::new(8);
    t.push_tool_output(99, "orphan\n");
    assert_eq!(t.tool_streams.get(99), None);
}
