use super::*;
use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, ImageContent, PermissionOptionKind, PlanEntryPriority,
    PlanEntryStatus, RequestPermissionOutcome, SelectedPermissionOutcome,
    SessionId as AcpSessionId, SessionUpdate, TextContent, ToolCallContent, ToolCallStatus,
    ToolKind,
};
use pacode_types::ids::{AgentId, CallId, PermissionId, SessionId as PacodeSessionId, TurnId};
use pacode_types::protocol::{Event, Request, TurnStop};
use pacode_types::state::{
    PermissionDecision, PermissionRequest, Plan, PlanItem, PlanStatus, RiskLevel, ToastLevel,
};
use pacode_types::transcript::{ToolStatus, TranscriptItem, TranscriptKind};

#[test]
fn test_session_id_round_trip() {
    let original = PacodeSessionId::new("session-12345");
    let acp_id = session_id_to_acp(&original);
    assert_eq!(acp_id.0.as_ref(), "session-12345");

    let round_tripped = session_id_from_acp(&acp_id);
    assert_eq!(round_tripped.as_str(), original.as_str());
}

#[test]
fn test_assistant_message_streaming_chunks() {
    let mut state = MappingState::new();
    let agent = AgentId::main();

    // 1. Incomplete assistant item added
    let item_start = TranscriptItem {
        seq: 1,
        agent: agent.clone(),
        ts_ms: 1000,
        kind: TranscriptKind::Assistant {
            text: String::new(),
            complete: false,
        },
    };
    let updates = event_to_session_updates(&Event::ItemAdded(item_start), &mut state);
    assert!(
        updates.is_empty(),
        "incomplete item added should emit nothing"
    );

    // 2. Streamed chunks via TextDelta
    let delta1 = Event::TextDelta {
        agent: agent.clone(),
        item_seq: 1,
        text: "Hello, ".to_string(),
    };
    let updates1 = event_to_session_updates(&delta1, &mut state);
    assert_eq!(updates1.len(), 1);
    match &updates1[0] {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(t) => assert_eq!(t.text, "Hello, "),
            _ => panic!("expected text block"),
        },
        _ => panic!("expected agent message chunk"),
    }

    let delta2 = Event::TextDelta {
        agent: agent.clone(),
        item_seq: 1,
        text: "world!".to_string(),
    };
    let updates2 = event_to_session_updates(&delta2, &mut state);
    assert_eq!(updates2.len(), 1);
    match &updates2[0] {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(t) => assert_eq!(t.text, "world!"),
            _ => panic!("expected text block"),
        },
        _ => panic!("expected agent message chunk"),
    }

    // 3. ItemUpdated with complete = true and full text
    let item_end = TranscriptItem {
        seq: 1,
        agent: agent.clone(),
        ts_ms: 1000,
        kind: TranscriptKind::Assistant {
            text: "Hello, world!".to_string(),
            complete: true,
        },
    };
    let updates3 = event_to_session_updates(&Event::ItemUpdated(item_end), &mut state);
    assert!(
        updates3.is_empty(),
        "already streamed item update must not duplicate content chunks"
    );
    assert!(state.streamed_items.is_empty());

    // 4. TurnEnded leaves no active stream
    let turn_ended = Event::TurnEnded {
        agent,
        turn: TurnId::new("t1"),
        usage: None,
        stop: TurnStop::Completed,
    };
    let _ = event_to_session_updates(&turn_ended, &mut state);
    assert!(
        state.streamed_items.is_empty(),
        "no active stream after turn end"
    );
}

#[test]
fn dropped_middle_delta_does_not_replace_divergent_authoritative_text() {
    let mut state = MappingState::new();
    let agent = AgentId::main();

    for text in ["Hello, ", "world!"] {
        let _ = event_to_session_updates(
            &Event::TextDelta {
                agent: agent.clone(),
                item_seq: 7,
                text: text.to_string(),
            },
            &mut state,
        );
    }

    let item = TranscriptItem {
        seq: 7,
        agent,
        ts_ms: 1000,
        kind: TranscriptKind::Assistant {
            text: "Hello, brave world!".to_string(),
            complete: true,
        },
    };
    let updates = event_to_session_updates(&Event::ItemUpdated(item.clone()), &mut state);
    assert!(updates.is_empty());
    assert_eq!(
        state.streamed_items.get(&7).map(String::as_str),
        Some("Hello, brave world!")
    );
    assert!(event_to_session_updates(&Event::ItemUpdated(item), &mut state).is_empty());
}

#[test]
fn reasoning_final_emits_only_the_authoritative_suffix() {
    let mut state = MappingState::new();
    let agent = AgentId::main();

    for text in ["think ", "hard"] {
        let _ = event_to_session_updates(
            &Event::ReasoningDelta {
                agent: agent.clone(),
                item_seq: 8,
                text: text.to_string(),
            },
            &mut state,
        );
    }

    let updates = event_to_session_updates(
        &Event::ItemAdded(TranscriptItem {
            seq: 8,
            agent,
            ts_ms: 1000,
            kind: TranscriptKind::Reasoning {
                text: "think hard!".to_string(),
                complete: true,
            },
        }),
        &mut state,
    );
    assert_eq!(updates.len(), 1);
    let SessionUpdate::AgentThoughtChunk(chunk) = &updates[0] else {
        panic!("expected reasoning suffix")
    };
    let ContentBlock::Text(text) = &chunk.content else {
        panic!("expected text content")
    };
    assert_eq!(text.text, "!");
    assert!(state.streamed_items.is_empty());
}

#[test]
fn main_and_subagent_items_with_the_same_seq_do_not_share_stream_state() {
    let mut state = MappingState::new();
    let main = AgentId::main();
    let subagent = AgentId::new("agt_subagent");

    let main_delta = Event::TextDelta {
        agent: main.clone(),
        item_seq: 17,
        text: "main".to_string(),
    };
    assert_eq!(event_to_session_updates(&main_delta, &mut state).len(), 1);
    let subagent_delta = Event::TextDelta {
        agent: subagent.clone(),
        item_seq: 17,
        text: "subagent".to_string(),
    };
    assert!(event_to_session_updates(&subagent_delta, &mut state).is_empty());
    assert_eq!(state.streamed_items.len(), 1);

    let subagent_item = TranscriptItem {
        seq: 17,
        agent: subagent.clone(),
        ts_ms: 1,
        kind: TranscriptKind::Assistant {
            text: "subagent".to_string(),
            complete: true,
        },
    };
    assert!(event_to_session_updates(&Event::ItemAdded(subagent_item), &mut state).is_empty());

    let main_item = TranscriptItem {
        seq: 17,
        agent: main.clone(),
        ts_ms: 2,
        kind: TranscriptKind::Assistant {
            text: "main final".to_string(),
            complete: true,
        },
    };
    let updates = event_to_session_updates(&Event::ItemAdded(main_item), &mut state);
    assert_eq!(updates.len(), 1);
    let SessionUpdate::AgentMessageChunk(chunk) = &updates[0] else {
        panic!("expected main assistant text");
    };
    let ContentBlock::Text(text) = &chunk.content else {
        panic!("expected main text content");
    };
    assert_eq!(text.text, " final");

    let subagent_tool = TranscriptItem {
        seq: 17,
        agent: subagent,
        ts_ms: 3,
        kind: TranscriptKind::ToolCall {
            call_id: CallId::new("call_subagent"),
            name: "read_file".to_string(),
            title: "Read subagent file".to_string(),
            intent: None,
            status: ToolStatus::Running,
            preview: String::new(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    };
    assert!(event_to_session_updates(&Event::ItemAdded(subagent_tool), &mut state).is_empty());

    let main_tool = TranscriptItem {
        seq: 17,
        agent: main,
        ts_ms: 4,
        kind: TranscriptKind::ToolCall {
            call_id: CallId::new("call_main"),
            name: "read_file".to_string(),
            title: "Read main file".to_string(),
            intent: None,
            status: ToolStatus::Running,
            preview: String::new(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    };
    assert!(matches!(
        event_to_session_updates(&Event::ItemAdded(main_tool), &mut state).as_slice(),
        [SessionUpdate::ToolCall(_)]
    ));
}

#[test]
fn authoritative_item_without_a_streamed_prefix_is_replayed() {
    let mut state = MappingState::new();
    let updates = event_to_session_updates(
        &Event::ItemAdded(TranscriptItem {
            seq: 9,
            agent: AgentId::main(),
            ts_ms: 1000,
            kind: TranscriptKind::Assistant {
                text: "recovered".to_string(),
                complete: true,
            },
        }),
        &mut state,
    );
    assert_eq!(updates.len(), 1);
    let SessionUpdate::AgentMessageChunk(chunk) = &updates[0] else {
        panic!("expected assistant replay")
    };
    let ContentBlock::Text(text) = &chunk.content else {
        panic!("expected text content")
    };
    assert_eq!(text.text, "recovered");
    assert!(state.streamed_items.is_empty());
}

#[test]
fn mapping_state_retention_is_bounded() {
    let mut state = MappingState::new();
    for seq in 0..(MAPPING_RETENTION_LIMIT * 2) {
        let item = TranscriptItem {
            seq: seq as u64,
            agent: AgentId::main(),
            ts_ms: seq as u64,
            kind: TranscriptKind::Assistant {
                text: seq.to_string(),
                complete: true,
            },
        };
        let _ = event_to_session_updates(&Event::ItemAdded(item), &mut state);
    }
    assert!(state.retained_items.len() <= MAPPING_RETENTION_LIMIT);
    assert!(state.replayed_items.len() <= MAPPING_RETENTION_LIMIT);
    assert!(state.completed_items.len() <= MAPPING_RETENTION_LIMIT);
    assert!(state.tool_statuses.len() <= MAPPING_RETENTION_LIMIT);
    assert!(state.tool_views.len() <= MAPPING_RETENTION_LIMIT);
    assert!(state.streamed_items.len() <= MAPPING_RETENTION_LIMIT);
}

#[test]
fn retention_eviction_does_not_replay_dropped_items() {
    let mut state = MappingState::new();
    let items: Vec<TranscriptItem> = (0..=MAPPING_RETENTION_LIMIT as u64)
        .map(|seq| TranscriptItem {
            seq,
            agent: AgentId::main(),
            ts_ms: seq,
            kind: TranscriptKind::Assistant {
                text: format!("line {seq}"),
                complete: true,
            },
        })
        .collect();
    for item in &items {
        let _ = event_to_session_updates(&Event::ItemAdded(item.clone()), &mut state);
    }

    let dropped = &items[0];
    let mut dropped_with_new_text = dropped.clone();
    dropped_with_new_text.kind = TranscriptKind::Assistant {
        text: "rewritten".to_string(),
        complete: true,
    };
    assert!(
        state
            .replay_transcript(&[dropped.clone(), dropped_with_new_text])
            .unwrap()
            .is_empty(),
        "an evicted item is already delivered and must never be emitted again"
    );
    assert!(
        state.replay_transcript(&items).unwrap().is_empty(),
        "replaying the whole window again must stay silent"
    );
}

#[test]
fn evicted_stream_prefix_is_never_re_emitted() {
    let mut state = MappingState::new();
    let items: Vec<TranscriptItem> = (0..=MAPPING_RETENTION_LIMIT as u64)
        .map(|seq| TranscriptItem {
            seq,
            agent: AgentId::main(),
            ts_ms: seq,
            kind: TranscriptKind::Assistant {
                text: format!("{seq}-"),
                complete: false,
            },
        })
        .collect();
    for item in &items {
        let _ = state.replay_transcript(std::slice::from_ref(item)).unwrap();
    }
    assert!(state.streamed_items.contains_key(&1));
    assert!(!state.streamed_items.contains_key(&0));

    let mut finished = items[0].clone();
    finished.kind = TranscriptKind::Assistant {
        text: "0-done".to_string(),
        complete: true,
    };
    assert!(
        state.replay_transcript(&[finished]).unwrap().is_empty(),
        "a forgotten stream prefix must not duplicate or diverge"
    );
    let mut restated = items[1].clone();
    restated.kind = TranscriptKind::Assistant {
        text: "1-done".to_string(),
        complete: true,
    };
    let updates = state.replay_transcript(&[restated]).unwrap();
    assert_eq!(updates.len(), 1, "a tracked prefix still emits its suffix");
    let SessionUpdate::AgentMessageChunk(chunk) = &updates[0] else {
        panic!("expected assistant suffix")
    };
    let ContentBlock::Text(text) = &chunk.content else {
        panic!("expected text content")
    };
    assert_eq!(text.text, "done");
}

fn tool_item(seq: u64, status: ToolStatus, preview: &str) -> TranscriptItem {
    TranscriptItem {
        seq,
        agent: AgentId::main(),
        ts_ms: seq,
        kind: TranscriptKind::ToolCall {
            call_id: CallId::new(format!("call_{seq}")),
            name: "bash".to_string(),
            title: "Bash".to_string(),
            intent: None,
            status,
            preview: preview.to_string(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    }
}

#[test]
fn snapshot_replay_emits_same_status_metadata_change_once() {
    let mut state = MappingState::new();
    let running = tool_item(4, ToolStatus::Running, "");
    assert!(matches!(
        state
            .replay_transcript(std::slice::from_ref(&running))
            .unwrap()
            .as_slice(),
        [SessionUpdate::ToolCall(_)]
    ));
    let streaming_args = tool_item(4, ToolStatus::Running, "cargo test -p pacode-acp");
    let updates = state
        .replay_transcript(std::slice::from_ref(&streaming_args))
        .unwrap();
    let [SessionUpdate::ToolCallUpdate(update)] = updates.as_slice() else {
        panic!("expected one tool call update, got {updates:?}")
    };
    assert_eq!(update.fields.status, Some(ToolCallStatus::InProgress));
    let content = update.fields.content.as_ref().expect("expected content");
    let [ToolCallContent::Content(content)] = content.as_slice() else {
        panic!("expected one text content block")
    };
    let ContentBlock::Text(text) = &content.content else {
        panic!("expected text content")
    };
    assert_eq!(text.text, "cargo test -p pacode-acp");
    assert!(
        state
            .replay_transcript(std::slice::from_ref(&streaming_args))
            .unwrap()
            .is_empty(),
        "an unchanged tool call must not be updated again"
    );

    let mut retitled = tool_item(4, ToolStatus::Ok, "ok");
    let TranscriptKind::ToolCall { title, .. } = &mut retitled.kind else {
        unreachable!()
    };
    *title = "Bash cargo test".to_string();
    assert!(matches!(
        state
            .replay_transcript(std::slice::from_ref(&retitled))
            .unwrap()
            .as_slice(),
        [SessionUpdate::ToolCallUpdate(_)]
    ));
    assert!(state.replay_transcript(&[retitled]).unwrap().is_empty());
}

#[test]
fn live_tool_events_keep_the_next_snapshot_replay_silent() {
    let mut state = MappingState::new();
    let running = tool_item(9, ToolStatus::Running, "");
    assert_eq!(
        event_to_session_updates(&Event::ItemAdded(running), &mut state).len(),
        1
    );
    let finished = tool_item(9, ToolStatus::Ok, "done");
    assert_eq!(
        event_to_session_updates(&Event::ItemUpdated(finished.clone()), &mut state).len(),
        1
    );
    assert!(
        state.replay_transcript(&[finished]).unwrap().is_empty(),
        "a snapshot that repeats what live events already delivered stays silent"
    );
}

#[test]
fn snapshot_replay_uses_item_floor_and_keeps_new_tool_calls() {
    let mut state = MappingState::new();
    state.seed_transcript(1, &[(1, "history".to_string(), true)]);
    let assistant = TranscriptItem {
        seq: 1,
        agent: AgentId::main(),
        ts_ms: 1,
        kind: TranscriptKind::Assistant {
            text: "history".to_string(),
            complete: true,
        },
    };
    assert!(
        state
            .replay_transcript(std::slice::from_ref(&assistant))
            .unwrap()
            .is_empty()
    );

    let tool = TranscriptItem {
        seq: 2,
        agent: AgentId::main(),
        ts_ms: 2,
        kind: TranscriptKind::ToolCall {
            call_id: CallId::new("call_replay"),
            name: "read_file".to_string(),
            title: "Read file".to_string(),
            intent: None,
            status: ToolStatus::Running,
            preview: String::new(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    };
    let updates = state.replay_transcript(&[assistant, tool.clone()]).unwrap();
    assert_eq!(updates.len(), 1);
    assert!(matches!(updates[0], SessionUpdate::ToolCall(_)));
    assert!(state.replay_transcript(&[tool]).unwrap().is_empty());
}

#[test]
fn snapshot_replay_recovers_unseen_items_below_a_seen_item() {
    let mut state = MappingState::new();
    state.seed_transcript_with_seen(11, &[(11, "latest".to_string(), true)], &[11]);
    let tool = TranscriptItem {
        seq: 10,
        agent: AgentId::main(),
        ts_ms: 1,
        kind: TranscriptKind::ToolCall {
            call_id: CallId::new("call_missing"),
            name: "read_file".to_string(),
            title: "Read file".to_string(),
            intent: None,
            status: ToolStatus::Running,
            preview: String::new(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    };
    assert!(matches!(
        state.replay_transcript(&[tool]).unwrap().as_slice(),
        [SessionUpdate::ToolCall(_)]
    ));
}

#[test]
fn snapshot_replay_emits_changed_tool_status_once() {
    let mut state = MappingState::new();
    let mut tool = TranscriptItem {
        seq: 4,
        agent: AgentId::main(),
        ts_ms: 1,
        kind: TranscriptKind::ToolCall {
            call_id: CallId::new("call_status"),
            name: "bash".to_string(),
            title: "Bash".to_string(),
            intent: None,
            status: ToolStatus::Running,
            preview: String::new(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    };
    assert!(matches!(
        state
            .replay_transcript(std::slice::from_ref(&tool))
            .unwrap()
            .as_slice(),
        [SessionUpdate::ToolCall(_)]
    ));
    let TranscriptKind::ToolCall { status, .. } = &mut tool.kind else {
        unreachable!()
    };
    *status = ToolStatus::Ok;
    assert!(matches!(
        state
            .replay_transcript(std::slice::from_ref(&tool))
            .unwrap()
            .as_slice(),
        [SessionUpdate::ToolCallUpdate(_)]
    ));
    assert!(state.replay_transcript(&[tool]).unwrap().is_empty());
}

#[test]
fn snapshot_replay_emits_only_authoritative_suffix() {
    let mut state = MappingState::new();
    state.seed_transcript(3, &[(3, "Hello".to_string(), false)]);
    let partial = TranscriptItem {
        seq: 3,
        agent: AgentId::main(),
        ts_ms: 3,
        kind: TranscriptKind::Assistant {
            text: "Hello world".to_string(),
            complete: false,
        },
    };
    let updates = state
        .replay_transcript(std::slice::from_ref(&partial))
        .unwrap();
    assert_eq!(updates.len(), 1);
    let SessionUpdate::AgentMessageChunk(chunk) = &updates[0] else {
        panic!("expected assistant suffix")
    };
    let ContentBlock::Text(text) = &chunk.content else {
        panic!("expected text content")
    };
    assert_eq!(text.text, " world");
    assert!(state.replay_transcript(&[partial]).unwrap().is_empty());

    let complete = TranscriptItem {
        seq: 3,
        agent: AgentId::main(),
        ts_ms: 4,
        kind: TranscriptKind::Assistant {
            text: "Hello world!".to_string(),
            complete: true,
        },
    };
    let updates = state
        .replay_transcript(std::slice::from_ref(&complete))
        .unwrap();
    assert_eq!(updates.len(), 1);
    let SessionUpdate::AgentMessageChunk(chunk) = &updates[0] else {
        panic!("expected completed assistant suffix")
    };
    let ContentBlock::Text(text) = &chunk.content else {
        panic!("expected text content")
    };
    assert_eq!(text.text, "!");
    assert!(state.replay_transcript(&[complete]).unwrap().is_empty());
}

#[test]
fn authoritative_non_prefix_replay_is_rejected_without_old_plus_new() {
    let item = |complete| TranscriptItem {
        seq: 5,
        agent: AgentId::main(),
        ts_ms: 5,
        kind: TranscriptKind::Assistant {
            text: "new".into(),
            complete,
        },
    };

    for complete in [false, true] {
        let mut state = MappingState::new();
        state.seed_transcript(5, &[(5, "old".into(), false)]);
        let error = state.replay_transcript(&[item(complete)]).unwrap_err();
        assert!(error.contains("diverges"));
        assert_eq!(
            state.streamed_items.get(&5).map(String::as_str),
            Some("old")
        );
    }
}

#[test]
fn test_tool_call_status_transitions() {
    let mut state = MappingState::new();
    let agent = AgentId::main();

    // 1. Tool call started (Running)
    let item_running = TranscriptItem {
        seq: 2,
        agent: agent.clone(),
        ts_ms: 1000,
        kind: TranscriptKind::ToolCall {
            call_id: CallId::new("call_1"),
            name: "read_file".to_string(),
            title: "Read src/main.rs".to_string(),
            intent: None,
            status: ToolStatus::Running,
            preview: String::new(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    };
    let updates1 = event_to_session_updates(&Event::ItemAdded(item_running), &mut state);
    assert_eq!(updates1.len(), 1);
    match &updates1[0] {
        SessionUpdate::ToolCall(tc) => {
            assert_eq!(tc.tool_call_id.0.as_ref(), "call_1");
            assert_eq!(tc.title, "Read src/main.rs");
            assert_eq!(tc.kind, ToolKind::Read);
            assert_eq!(tc.status, ToolCallStatus::InProgress);
        }
        _ => panic!("expected ToolCall"),
    }

    // 2. Tool call completed (Ok) with preview
    let item_ok = TranscriptItem {
        seq: 2,
        agent,
        ts_ms: 1000,
        kind: TranscriptKind::ToolCall {
            call_id: CallId::new("call_1"),
            name: "read_file".to_string(),
            title: "Read src/main.rs".to_string(),
            intent: None,
            status: ToolStatus::Ok,
            preview: "fn main() {}".to_string(),
            diff: None,
            duration_ms: Some(42),
            task: None,
        },
    };
    let updates2 = event_to_session_updates(&Event::ItemUpdated(item_ok), &mut state);
    assert_eq!(updates2.len(), 1);
    match &updates2[0] {
        SessionUpdate::ToolCallUpdate(tcu) => {
            assert_eq!(tcu.tool_call_id.0.as_ref(), "call_1");
            assert_eq!(tcu.fields.status, Some(ToolCallStatus::Completed));
            let content = tcu.fields.content.as_ref().expect("expected content");
            assert_eq!(content.len(), 1);
            match &content[0] {
                ToolCallContent::Content(c) => match &c.content {
                    ContentBlock::Text(t) => assert_eq!(t.text, "fn main() {}"),
                    _ => panic!("expected text block"),
                },
                _ => panic!("expected content variant"),
            }
        }
        _ => panic!("expected ToolCallUpdate"),
    }
}

#[test]
fn turn_stop_outcomes_preserve_failure() {
    assert_eq!(
        turn_stop_to_stop_reason(&TurnStop::Completed),
        Ok(AcpStopReason::EndTurn)
    );
    assert_eq!(
        turn_stop_to_stop_reason(&TurnStop::Interrupted),
        Ok(AcpStopReason::Cancelled)
    );
    assert_eq!(
        turn_stop_to_stop_reason(&TurnStop::Failed {
            message: "provider failed".to_string(),
        }),
        Err("provider failed".to_string())
    );
}

#[test]
fn test_plan_mapping() {
    let plan = Plan {
        version: 1,
        items: vec![
            PlanItem {
                id: "1".to_string(),
                content: "Step 1: Inspect code".to_string(),
                status: PlanStatus::Done,
                progress: None,
            },
            PlanItem {
                id: "2".to_string(),
                content: "Step 2: Add ACP crate".to_string(),
                status: PlanStatus::Active,
                progress: Some(50),
            },
            PlanItem {
                id: "3".to_string(),
                content: "Step 3: Run verification".to_string(),
                status: PlanStatus::Pending,
                progress: None,
            },
            PlanItem {
                id: "4".to_string(),
                content: "Step 4: Cancelled idea".to_string(),
                status: PlanStatus::Cancelled,
                progress: None,
            },
        ],
    };

    let acp_plan = plan_to_acp(&plan);
    assert_eq!(acp_plan.entries.len(), 3, "cancelled item must be excluded");

    assert_eq!(acp_plan.entries[0].content, "Step 1: Inspect code");
    assert_eq!(acp_plan.entries[0].status, PlanEntryStatus::Completed);
    assert_eq!(acp_plan.entries[0].priority, PlanEntryPriority::Medium);

    assert_eq!(acp_plan.entries[1].content, "Step 2: Add ACP crate");
    assert_eq!(acp_plan.entries[1].status, PlanEntryStatus::InProgress);

    assert_eq!(acp_plan.entries[2].content, "Step 3: Run verification");
    assert_eq!(acp_plan.entries[2].status, PlanEntryStatus::Pending);
}

#[test]
fn test_permission_request_and_decision_mapping() {
    let session_id = AcpSessionId::new("sess_test");
    let perm = PermissionRequest {
        id: PermissionId::new("perm_1"),
        agent: AgentId::main(),
        agent_name: "assistant".to_string(),
        call_id: CallId::new("call_bash"),
        tool: "bash".to_string(),
        title: "Run command: rm -rf /tmp/foo".to_string(),
        detail: "rm -rf /tmp/foo".to_string(),
        risk: Some(RiskLevel::Confirm),
        created_at_ms: 1000,
    };

    let acp_req = permission_request_to_acp(&session_id, &perm);
    assert_eq!(acp_req.session_id.0.as_ref(), "sess_test");
    assert_eq!(acp_req.tool_call.tool_call_id.0.as_ref(), "call_bash");
    assert_eq!(acp_req.options.len(), 3);

    assert_eq!(acp_req.options[0].option_id.0.as_ref(), "allow_once");
    assert_eq!(acp_req.options[0].kind, PermissionOptionKind::AllowOnce);

    assert_eq!(acp_req.options[1].option_id.0.as_ref(), "allow_session");
    assert_eq!(acp_req.options[1].kind, PermissionOptionKind::AllowAlways);

    assert_eq!(acp_req.options[2].option_id.0.as_ref(), "deny");
    assert_eq!(acp_req.options[2].kind, PermissionOptionKind::RejectOnce);

    // Test responses
    let resp_allow_once = RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
        SelectedPermissionOutcome::new("allow_once"),
    ));
    assert_eq!(
        permission_response_to_decision(&resp_allow_once),
        PermissionDecision::AllowOnce
    );

    let resp_allow_session = RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
        SelectedPermissionOutcome::new("allow_session"),
    ));
    assert_eq!(
        permission_response_to_decision(&resp_allow_session),
        PermissionDecision::AllowSession
    );

    let resp_deny = RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
        SelectedPermissionOutcome::new("deny"),
    ));
    assert_eq!(
        permission_response_to_decision(&resp_deny),
        PermissionDecision::Deny
    );

    let resp_cancelled = RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled);
    assert_eq!(
        permission_response_to_decision(&resp_cancelled),
        PermissionDecision::Deny
    );

    let resp_unknown = RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
        SelectedPermissionOutcome::new("random_id"),
    ));
    assert_eq!(
        permission_response_to_decision(&resp_unknown),
        PermissionDecision::Deny
    );
}

#[test]
fn test_unmapped_events_dropped_cleanly() {
    let mut state = MappingState::new();

    let unmapped = vec![
        Event::Toast {
            level: ToastLevel::Info,
            title: "Notification".to_string(),
            detail: None,
        },
        Event::PluginToast {
            plugin: "test".to_string(),
            text: "msg".to_string(),
        },
        Event::PluginStatus {
            plugin: "test".to_string(),
            text: "ready".to_string(),
        },
        Event::DaemonShuttingDown,
        Event::TurnStarted {
            agent: AgentId::new("a"),
            turn: TurnId::new("t"),
        },
        Event::PermissionResolved {
            permission: PermissionId::new("p"),
            decision: PermissionDecision::AllowOnce,
            reason: None,
        },
        Event::PermissionExplained {
            permission: PermissionId::new("p"),
            text: "Deletes files.".to_string(),
            level: Some(pacode_types::ExplainLevel::High),
        },
    ];

    for event in unmapped {
        let updates = event_to_session_updates(&event, &mut state);
        assert!(
            updates.is_empty(),
            "unmapped event must produce empty updates"
        );
    }
}

#[test]
fn test_prompt_content_extracts_text_and_images() {
    // "hi" in base64.
    let prompt = vec![
        ContentBlock::Text(TextContent::new("look at this ".to_string())),
        ContentBlock::Image(ImageContent::new("aGk=", "image/png")),
        ContentBlock::Text(TextContent::new("please".to_string())),
    ];
    let (text, images) = prompt_content_to_parts(&prompt);
    assert_eq!(text, "look at this please");
    assert_eq!(images.len(), 1);
    match &images[0] {
        ImageSource::Bytes { mime, data } => {
            assert_eq!(mime, "image/png");
            assert_eq!(data, b"hi");
        }
        ImageSource::Path(_) => panic!("expected bytes image source"),
    }
}

#[test]
fn test_undecodable_image_degrades_to_text_placeholder() {
    let prompt = vec![
        ContentBlock::Text(TextContent::new("before ".to_string())),
        ContentBlock::Image(ImageContent::new("!!!not-base64!!!", "image/png")),
    ];
    let (text, images) = prompt_content_to_parts(&prompt);
    assert!(images.is_empty());
    assert_eq!(text, "before [image: image/png, undecodable]");
}

#[test]
fn test_oversized_image_is_rejected_before_decoding() {
    let huge = "A".repeat(MAX_IMAGE_B64_LEN + 1);
    let prompt = vec![ContentBlock::Image(ImageContent::new(huge, "image/png"))];
    let (text, images) = prompt_content_to_parts(&prompt);
    assert!(images.is_empty());
    assert_eq!(text, "[image: image/png, too large]");
}

#[test]
fn test_mode_descriptions_match_the_mode_picker() {
    for mode in Mode::CYCLE {
        let session_mode = mode_to_session_mode(mode);
        assert_eq!(
            session_mode.description.as_deref(),
            Some(mode_description(mode)),
            "config options and the legacy picker must share mode texts"
        );
    }
}

#[test]
fn test_cancel_notification_maps_to_interrupt() {
    let cancel = CancelNotification::new(AcpSessionId::new("test_sess"));
    let pacode_session = session_id_from_acp(&cancel.session_id);
    assert_eq!(pacode_session.as_str(), "test_sess");
    let daemon_request = Request::Interrupt;
    match daemon_request {
        Request::Interrupt => {}
        Request::Hello(_)
        | Request::Attach(_)
        | Request::Detach
        | Request::CloseSession
        | Request::DetachTurn
        | Request::InterruptAll
        | Request::UserMessage { .. }
        | Request::PermissionReply { .. }
        | Request::ExplainPermission { .. }
        | Request::SetModel(_)
        | Request::SetEffort(_)
        | Request::SetMode(_)
        | Request::Analyze
        | Request::SetZdr(_)
        | Request::SetNoticeLock(_)
        | Request::SetModelFlag { .. }
        | Request::StopAgent(_)
        | Request::KillTask(_)
        | Request::AckTask(_)
        | Request::GetSnapshot
        | Request::GetHistory { .. }
        | Request::GetTaskOutput { .. }
        | Request::RuntimeStats
        | Request::ListSessions { .. }
        | Request::ListModels
        | Request::RefreshModels
        | Request::Compact
        | Request::ClearContext
        | Request::Shutdown { .. }
        | Request::Ping
        | Request::ListMcpServers
        | Request::RestartMcpServer { .. }
        | Request::SetMcpServerEnabled { .. }
        | Request::GetMcpPrompt { .. }
        | Request::ListPlugins
        | Request::RunPluginCommand { .. }
        | Request::AnswerQuestion { .. }
        | Request::BrowseMarketplace { .. }
        | Request::InstallPlugin { .. }
        | Request::UninstallPlugin { .. }
        | Request::ListCronJobs
        | Request::AddCronJob { .. }
        | Request::RemoveCronJob(_)
        | Request::SetCronEnabled { .. }
        | Request::RunCronJobNow(_)
        | Request::ListMonitors
        | Request::StopMonitor(_)
        | Request::ListAuth
        | Request::Login { .. }
        | Request::Logout { .. }
        | Request::SetAuthAccount { .. }
        | Request::GitLog { .. }
        | Request::GitShow { .. }
        | Request::GitDiff { .. } => panic!("expected Request::Interrupt"),
    }
}

#[test]
fn a_cleared_context_empties_the_client_plan() {
    let mut state = MappingState::new();
    let updates = event_to_session_updates(&Event::ContextCleared, &mut state);
    assert_eq!(
        updates,
        vec![SessionUpdate::Plan(plan_to_acp(&Plan::default()))]
    );
}
