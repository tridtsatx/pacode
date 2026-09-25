use std::sync::Arc;

use pacode_tools::ToolRegistry;
use pacode_types::{AgentId, AgentInfo, AgentKind, AgentStatus, CallId, ContentBlock, ModelRoute};

use super::*;

fn dummy_agent_info(id: AgentId) -> AgentInfo {
    AgentInfo {
        id,
        name: "test".to_string(),
        kind: AgentKind::Main,
        status: AgentStatus::Idle,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("provider", "model"),
        effort: pacode_types::Effort::Low,
        parent: None,
        summary: None,
        error: None,
    }
}

#[test]
fn test_agent_id_accessor_and_swap() {
    let main_id = AgentId::main();
    let agent = Agent::new(
        main_id.clone(),
        dummy_agent_info(main_id.clone()),
        History::default(),
        ToolRegistry::new(),
        50,
        None,
    );

    assert_eq!(agent.id(), main_id);
    assert_eq!(*agent.id_arc(), main_id);
    assert!(agent.with_id(|id| id.is_main()));

    let new_id = AgentId::new("agt_sub123");
    agent.set_id(new_id.clone());

    assert_eq!(agent.id(), new_id);
    assert_eq!(*agent.id_arc(), new_id);
    assert!(agent.with_id(|id| !id.is_main()));
}

#[test]
fn test_agent_concurrent_id_read() {
    let agent = Arc::new(Agent::new(
        AgentId::main(),
        dummy_agent_info(AgentId::main()),
        History::default(),
        ToolRegistry::new(),
        50,
        None,
    ));

    let agent_clone = agent.clone();
    let handle = std::thread::spawn(move || {
        for _ in 0..1000 {
            let _ = agent_clone.id();
            let _ = agent_clone.id_arc();
            agent_clone.with_id(|id| {
                assert!(!id.as_str().is_empty());
            });
        }
    });

    for i in 0..100 {
        agent.set_id(AgentId::new(format!("agt_{i}")));
    }

    handle.join().unwrap();
}

fn tool_use_msg(call_id: &str) -> Message {
    Message::new(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: CallId::new(call_id),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
        }],
    )
}

/// `tool_use` ids still unanswered in a message list, in order — mirrors what
/// providers 400 on (`tool_use ids found without tool_result`).
fn unanswered_tool_uses(messages: &[Arc<Message>]) -> Vec<CallId> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        if messages[i].role != Role::Assistant {
            i += 1;
            continue;
        }
        let uses: Vec<CallId> = messages[i]
            .tool_uses()
            .map(|(id, _, _)| id.clone())
            .collect();
        i += 1;
        let mut answered: Vec<CallId> = Vec::new();
        while i < messages.len() && messages[i].role == Role::Tool {
            for block in &messages[i].content {
                if let ContentBlock::ToolResult { call_id, .. } = block {
                    answered.push(call_id.clone());
                }
            }
            i += 1;
        }
        out.extend(uses.into_iter().filter(|id| !answered.contains(id)));
    }
    out
}

#[test]
fn for_model_shares_history_messages() {
    let mut hist = History::default();
    let (seq, shared) = hist.push(Message::user("hello"));
    assert_eq!(seq, 0);
    let model_msgs = hist.for_model();
    assert_eq!(model_msgs.len(), 1);
    assert!(
        Arc::ptr_eq(&model_msgs[0], &shared),
        "for_model must share, not deep-clone, history messages"
    );
}

#[test]
fn for_model_repairs_orphan_tool_use() {
    let mut hist = History::default();
    hist.push(Message::user("run ls"));
    hist.push(tool_use_msg("call_orphan"));

    let model_msgs = hist.for_model();
    assert_eq!(model_msgs.len(), 3, "synthetic result must be injected");
    assert_eq!(model_msgs[2].role, Role::Tool);
    match &model_msgs[2].content[0] {
        ContentBlock::ToolResult {
            call_id,
            content,
            is_error,
        } => {
            assert_eq!(call_id.as_str(), "call_orphan");
            assert_eq!(content, INTERRUPTED_TOOL_RESULT);
            assert!(*is_error, "synthetic stub is an error result");
        }
        other => panic!("expected tool_result, got {other:?}"),
    }
    assert!(unanswered_tool_uses(&model_msgs).is_empty());
    // The store-side history is untouched: repair is wire-only.
    assert_eq!(hist.messages.len(), 2);
}

#[test]
fn for_model_preserves_paired_tool_calls() {
    let mut hist = History::default();
    hist.push(Message::user("go"));
    hist.push(tool_use_msg("call_a"));
    hist.push(Message::tool_result(
        CallId::new("call_a"),
        "file.rs",
        false,
    ));
    hist.push(Message::assistant_text("done"));

    let model_msgs = hist.for_model();
    assert_eq!(model_msgs.len(), 4, "a paired exchange needs no repair");
    assert!(unanswered_tool_uses(&model_msgs).is_empty());
}

#[test]
fn for_model_repairs_partial_answer_run() {
    let mut hist = History::default();
    let mut msg = tool_use_msg("call_a");
    msg.content.push(ContentBlock::ToolUse {
        id: CallId::new("call_b"),
        name: "read".to_string(),
        input: serde_json::json!({}),
    });
    hist.push(msg);
    hist.push(Message::tool_result(CallId::new("call_a"), "ok", false));

    let model_msgs = hist.for_model();
    assert_eq!(model_msgs.len(), 3);
    match &model_msgs[2].content[0] {
        ContentBlock::ToolResult { call_id, .. } => {
            assert_eq!(call_id.as_str(), "call_b", "only the missing id is stubbed")
        }
        other => panic!("expected tool_result, got {other:?}"),
    }
    assert!(unanswered_tool_uses(&model_msgs).is_empty());
}

#[test]
fn for_model_repairs_orphan_before_user_message() {
    let mut hist = History::default();
    hist.push(tool_use_msg("call_orphan"));
    hist.push(Message::user("never mind"));

    let model_msgs = hist.for_model();
    // Stub lands right after the assistant message, before the user turn —
    // the wire order providers require.
    assert_eq!(model_msgs.len(), 3);
    assert_eq!(model_msgs[1].role, Role::Tool);
    assert_eq!(model_msgs[2].role, Role::User);
    assert!(unanswered_tool_uses(&model_msgs).is_empty());
}

#[test]
fn for_model_degrades_stray_tool_result() {
    let mut hist = History::default();
    hist.push(Message::user("hi"));
    // A tool result with no assistant tool_use behind it — e.g. the assistant
    // message was compacted away or never persisted.
    hist.push(Message::tool_result(CallId::new("call_ghost"), "x", false));

    let model_msgs = hist.for_model();
    assert_eq!(model_msgs.len(), 2);
    match &model_msgs[1].content[0] {
        ContentBlock::Text { text } => {
            assert!(text.contains("call_ghost"), "stray result degrades to text");
        }
        other => panic!("expected text, got {other:?}"),
    }
}

#[test]
fn for_model_degrades_duplicate_tool_result() {
    let mut hist = History::default();
    hist.push(tool_use_msg("call_a"));
    hist.push(Message::tool_result(CallId::new("call_a"), "first", false));
    hist.push(Message::tool_result(CallId::new("call_a"), "dup", false));

    let model_msgs = hist.for_model();
    assert_eq!(model_msgs.len(), 3);
    match &model_msgs[2].content[0] {
        ContentBlock::Text { text } => {
            assert!(text.contains("call_a"), "duplicate result degrades to text");
        }
        other => panic!("expected text, got {other:?}"),
    }
    assert!(unanswered_tool_uses(&model_msgs).is_empty());
}

#[test]
fn rolled_back_reservation_seq_is_reused_without_reordering_messages() {
    let mut history = History::default();
    let first = history.reserve(Message::user("first"));
    let second = history.reserve(Message::user("second"));
    history.commit_reserved(&second);
    history.rollback_reserved(&first);
    let third = history.reserve(Message::user("third"));
    history.commit_reserved(&third);

    assert_eq!(third.seq(), 0);
    assert_eq!(
        history
            .rows()
            .into_iter()
            .map(|(seq, message)| (seq, message.text()))
            .collect::<Vec<_>>(),
        vec![(0, "third".to_string()), (1, "second".to_string()),]
    );
}

#[test]
fn clear_resets_history_side_tables_and_preserves_next_seq() {
    let mut history = History::default();
    history.push(Message::user("first"));
    history.push(Message::user("second"));
    let first_hole = history.reserve(Message::user("hole"));
    let _tail = history.reserve(Message::user("tail"));
    history.rollback_reserved(&first_hole);
    history.summary = Some("summary".into());
    history.summary_upto = 1;

    history.clear();

    assert!(history.messages.is_empty());
    assert!(history.message_seqs.is_empty());
    assert!(history.free_seqs.is_empty());
    assert_eq!(history.estimated_tokens, 0);
    assert!(history.summary.is_none());
    assert_eq!(history.summary_upto, 0);
    assert_eq!(history.next_seq, 4);
    assert_eq!(history.push(Message::user("after clear")).0, 4);
}

#[test]
fn bulk_restore_keeps_sorted_rows_and_adds_tokens_once() {
    let mut history = History {
        summary: Some("summary".into()),
        ..History::default()
    };
    let rows = vec![
        (1, Arc::new(Message::user("one"))),
        (3, Arc::new(Message::user("three"))),
        (5, Arc::new(Message::user("five"))),
    ];
    let expected_tokens = pacode_types::estimate_tokens("summary")
        + rows
            .iter()
            .map(|(_, message)| message.estimate_tokens())
            .sum::<u32>();

    history.restore_rows(rows);

    assert_eq!(
        history
            .rows()
            .into_iter()
            .map(|(seq, message)| (seq, message.text()))
            .collect::<Vec<_>>(),
        vec![
            (1, "one".to_string()),
            (3, "three".to_string()),
            (5, "five".to_string()),
        ]
    );
    assert_eq!(history.next_seq, 6);
    assert_eq!(history.estimated_tokens, expected_tokens);
}

#[test]
fn bulk_restore_orders_out_of_order_rows_without_reusing_occupied_seqs() {
    let mut history = History::default();
    let (_, existing) = history.push(Message::user("existing"));
    let first_hole = history.reserve(Message::user("first hole"));
    let _tail = history.reserve(Message::user("tail"));
    history.rollback_reserved(&first_hole);
    assert!(history.free_seqs.contains(&1));

    let row_one = Arc::new(Message::user("one"));
    let row_three = Arc::new(Message::user("three"));
    let row_five = Arc::new(Message::user("five"));
    let row_seven = Arc::new(Message::user("seven"));
    let expected_tokens = existing.estimate_tokens()
        + row_one.estimate_tokens()
        + row_three.estimate_tokens()
        + row_five.estimate_tokens()
        + row_seven.estimate_tokens();
    history.restore_rows(vec![
        (5, row_five),
        (1, row_one),
        (0, Arc::new(Message::user("replacement"))),
        (3, row_three),
        (1, Arc::new(Message::user("one"))),
        (7, row_seven),
    ]);

    assert_eq!(
        history
            .rows()
            .into_iter()
            .map(|(seq, message)| (seq, message.text()))
            .collect::<Vec<_>>(),
        vec![
            (0, "existing".to_string()),
            (1, "one".to_string()),
            (3, "three".to_string()),
            (5, "five".to_string()),
            (7, "seven".to_string()),
        ]
    );
    assert_eq!(history.next_seq, 8);
    assert_eq!(history.estimated_tokens, expected_tokens);
    assert!(history.free_seqs.is_empty());
}

#[test]
fn discarded_user_reservation_cannot_reenter_free_seq_pool() {
    let agent = Agent::new(
        AgentId::main(),
        dummy_agent_info(AgentId::main()),
        History::default(),
        ToolRegistry::new(),
        50,
        None,
    );
    let reservation = agent.reserve_user_message(Message::user("discarded"));
    reservation.persist();

    assert!(agent.discard_pending_user_messages().is_empty());
    assert!(agent.discarded_message_seqs().contains(&0));
    agent.rollback_user_message(&reservation);

    let mut history = agent.history.lock().unwrap();
    assert!(history.free_seqs.is_empty());
    assert_eq!(history.reserve(Message::user("next")).seq(), 1);
}

#[test]
fn finished_admission_does_not_block_follow_up_turn() {
    let agent = Agent::new(
        AgentId::main(),
        dummy_agent_info(AgentId::main()),
        History::default(),
        ToolRegistry::new(),
        50,
        None,
    );
    let first_turn = TurnId::new("trn_first");
    assert!(agent.begin_admission_turn("first".into(), first_turn.clone()));
    agent.mark_admission_progressed("first", &first_turn);
    let first = agent
        .finish_admission_turn(&first_turn, &TurnStop::Completed)
        .unwrap();
    assert!(first.terminal);
    assert!(agent.admission_turn("first").is_none());

    let next_turn = TurnId::new("trn_next");
    assert!(agent.begin_admission_turn("next".into(), next_turn.clone()));
    agent.mark_admission_progressed("next", &next_turn);
    let next = agent
        .finish_admission_turn(&next_turn, &TurnStop::Completed)
        .unwrap();
    assert_eq!(next.message_id, "next");
    assert!(next.terminal);
    assert!(agent.admission_turn("next").is_none());
}

#[test]
fn user_message_reservation_stays_hidden_until_commit() {
    let agent = Agent::new(
        AgentId::main(),
        dummy_agent_info(AgentId::main()),
        History::default(),
        ToolRegistry::new(),
        50,
        None,
    );
    let message = Message::new(
        Role::User,
        vec![
            ContentBlock::Text {
                text: "steer".into(),
            },
            ContentBlock::Image {
                source: pacode_types::ImageSource::Bytes {
                    mime: "image/png".into(),
                    data: vec![1, 2, 3],
                },
            },
        ],
    );
    let reservation = agent.reserve_user_message(message.clone());
    reservation.persist();

    assert_eq!(reservation.seq(), 0);
    assert!(agent.has_pending_user_messages());
    assert!(agent.history.lock().unwrap().for_model().is_empty());
    assert!(agent.commit_pending_user_messages());
    assert!(!agent.has_pending_user_messages());

    let history = agent.history.lock().unwrap();
    assert_eq!(history.messages.len(), 1);
    assert_eq!(*history.messages[0], message);
    assert_eq!(history.next_seq, 1);
    assert_eq!(
        history.estimated_tokens,
        reservation.message().estimate_tokens()
    );
}

/// A store page is positioned by a message-seq floor the agent owns. The tail
/// item seqs and the store's are separate domains, and this is the pair of
/// defects that came from treating them as one: a message-seq cursor derived
/// from a transcript item seq, and store items numbered up from a message seq so
/// they collided with the tail.
mod history_cursor {
    use super::*;
    use pacode_types::{Message, Role, SessionId};

    fn agent_with_tail(tail_messages: &[(u64, Message)], history_page: u32) -> Agent {
        let main_id = AgentId::main();
        let rows: Vec<(u64, Arc<Message>)> = tail_messages
            .iter()
            .map(|(seq, msg)| (*seq, Arc::new(msg.clone())))
            .collect();
        let history = History {
            message_seqs: rows.iter().map(|(seq, _)| *seq).collect(),
            free_seqs: Default::default(),
            messages: rows.iter().map(|(_, msg)| msg.clone()).collect(),
            next_seq: rows.last().map(|(seq, _)| seq + 1).unwrap_or(0),
            summary: None,
            summary_upto: 0,
            estimated_tokens: 0,
        };
        let agent = Agent::new(
            main_id.clone(),
            dummy_agent_info(main_id.clone()),
            history,
            ToolRegistry::new(),
            history_page,
            None,
        );
        let mut state = crate::transcript::TranscriptState::new(history_page as usize * 2);
        state.reserve_history_band();
        for entry in crate::transcript::history_to_items(&main_id, &rows) {
            let mut item = entry.item;
            item.seq = state.next_seq();
            state.upsert(item);
        }
        *agent.transcript.lock().unwrap() = state;
        agent
    }

    fn user(text: &str) -> Message {
        Message::user(text.to_string())
    }

    /// Messages are foreign-keyed to their session, so the row goes in first.
    async fn seed(store: &pacode_store::Store, session: &SessionId, rows: &[(u64, Message)]) {
        store
            .upsert_session(&pacode_types::SessionMeta {
                id: session.clone(),
                name: None,
                cwd: std::path::PathBuf::from("/tmp"),
                git_branch: None,
                created_at_ms: 1,
                updated_at_ms: 1,
                model: pacode_types::ModelRoute::new("mock", "mock"),
                effort: pacode_types::Effort::Low,
                mode: pacode_types::Mode::Build,
                first_prompt: None,
                zdr: false,
            })
            .await
            .unwrap();
        for (seq, message) in rows {
            store
                .append_message(session, &AgentId::main(), *seq, message)
                .await
                .unwrap();
        }
    }

    /// Message seqs have gaps: cancelled and failed admissions never land. A
    /// cursor built by counting rows instead of reading seqs walks off the
    /// window — this pins that paging follows the real seqs.
    #[tokio::test]
    async fn paging_follows_real_message_seqs_across_gaps() {
        let store = pacode_store::Store::open_in_memory().unwrap();
        let session = SessionId::new("cursor-gaps");
        // 3, 4 and 5 were never persisted: the tail starts at 9.
        seed(
            &store,
            &session,
            &[
                (0, user("zero")),
                (1, user("one")),
                (9, user("nine")),
                (10, user("ten")),
            ],
        )
        .await;

        let agent = agent_with_tail(&[(9, user("nine")), (10, user("ten"))], 50);
        agent.seed_history_floor(Some(9));

        let (older, has_more) = agent.load_older_history(&store, &session, 10).await;
        let texts: Vec<&str> = older
            .iter()
            .map(|item| match &item.kind {
                pacode_types::TranscriptKind::User { text } => text.as_str(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            texts,
            vec!["zero", "one"],
            "must skip the gap, not stop at it"
        );
        assert!(!has_more, "seq 0 is the oldest message");

        let (tail, _) = agent.transcript_tail(Some(u64::MAX), 10);
        assert_eq!(tail.len(), 2, "the tail is untouched by a store page");

        // The store boundary is the agent's own message-seq floor. Echoing a
        // transcript item seq back must not move it: the two numberings are
        // unrelated, and a page positioned by the item cursor reads the wrong
        // window — here, nothing at all, because tail item seqs sit in their own
        // band, far above any message seq.
        let exhausted = agent.history_floor();
        let (again, _) = agent.load_older_history(&store, &session, 10).await;
        assert!(again.is_empty(), "the floor is exhausted, not restarted");
        assert_eq!(agent.history_floor(), exhausted);
        assert!(
            tail.iter().map(|item| item.seq).min().unwrap() > 1_000_000,
            "tail item seqs live in their own band, not the message domain"
        );
    }

    /// One assistant message can project into several items. The budget is in
    /// items while the store pages in messages, so a page must not overrun the
    /// budget — and must not drop items either.
    #[tokio::test]
    async fn an_item_budget_is_respected_when_messages_are_wider_than_one_item() {
        let store = pacode_store::Store::open_in_memory().unwrap();
        let session = SessionId::new("cursor-wide");

        let wide = |text: &str| {
            let mut message = Message::new(
                Role::Assistant,
                vec![
                    pacode_types::ContentBlock::Reasoning {
                        text: format!("{text}-thinking"),
                        signature: None,
                    },
                    pacode_types::ContentBlock::Text {
                        text: text.to_string(),
                    },
                ],
            );
            message.meta.timestamp_ms = 1;
            message
        };
        // Six messages, two items each (reasoning + text): twelve in total.
        let older: Vec<(u64, Message)> = (0..6).map(|i| (i, wide(&format!("m{i}")))).collect();
        let tail = vec![(6, user("tail"))];
        seed(&store, &session, &[older.clone(), tail.clone()].concat()).await;

        let agent = agent_with_tail(&tail, 50);
        agent.seed_history_floor(Some(6));

        let (items, has_more) = agent.load_older_history(&store, &session, 4).await;
        assert!(
            items.len() <= 4,
            "the item budget must hold, got {}",
            items.len()
        );
        assert!(has_more, "a bounded budget leaves older items behind");

        // Every returned item is strictly older than the tail, so a client that
        // prepends them cannot collide with a live cell id.
        let (tail_items, _) = agent.transcript_tail(None, 10);
        let lowest_tail = tail_items.iter().map(|i| i.seq).min().unwrap();
        assert!(
            items.iter().all(|item| item.seq < lowest_tail),
            "store items must number below the tail"
        );

        // Walking to the end must reach every older item exactly once.
        let mut seen: Vec<u64> = items.iter().map(|item| item.seq).collect();
        let mut more = has_more;
        while more {
            let (page, next) = agent.load_older_history(&store, &session, 4).await;
            if page.is_empty() {
                more = false;
                continue;
            }
            assert!(page.iter().all(|item| item.seq < lowest_tail));
            seen.extend(page.iter().map(|item| item.seq));
            more = next;
        }
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), before, "a store item was handed out twice");
        assert_eq!(seen.len(), 12, "every older item is reachable exactly once");
    }

    /// A conversation that fits in the tail has nothing older to page, and must
    /// not claim otherwise — the old heuristic reported `has_more` from the
    /// first item's seq, which is 0 for the oldest row.
    #[tokio::test]
    async fn a_tail_that_covers_everything_reports_no_older_history() {
        let agent = agent_with_tail(&[(0, user("only"))], 50);
        agent.seed_history_floor(None);

        let (items, has_more) = agent.transcript_tail(None, 10);
        assert_eq!(items.len(), 1);
        assert!(!has_more, "the tail is the whole conversation");
        let (older, older_more) = agent
            .load_older_history(
                &pacode_store::Store::open_in_memory().unwrap(),
                &SessionId::new("cursor-none"),
                10,
            )
            .await;
        assert!(older.is_empty() && !older_more);
    }
}
