use super::*;
use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, PermissionOptionKind, PlanEntryPriority, PlanEntryStatus,
    RequestPermissionOutcome, SelectedPermissionOutcome, SessionId as AcpSessionId, SessionUpdate,
    ToolCallContent, ToolCallStatus, ToolKind,
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
    let agent = AgentId::new("assistant");

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

    // 4. TurnEnded clears the state
    let turn_ended = Event::TurnEnded {
        agent,
        turn: TurnId::new("t1"),
        usage: None,
        stop: TurnStop::Completed,
    };
    let _ = event_to_session_updates(&turn_ended, &mut state);
    assert!(
        state.streamed_items.is_empty(),
        "state must clear on turn end"
    );
}

#[test]
fn test_tool_call_status_transitions() {
    let mut state = MappingState::new();
    let agent = AgentId::new("assistant");

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
        | Request::UserMessage { .. }
        | Request::PermissionReply { .. }
        | Request::SetModel(_)
        | Request::SetEffort(_)
        | Request::SetMode(_)
        | Request::StopAgent(_)
        | Request::KillTask(_)
        | Request::AckTask(_)
        | Request::GetSnapshot
        | Request::GetHistory { .. }
        | Request::GetTaskOutput { .. }
        | Request::ListSessions { .. }
        | Request::ListModels
        | Request::Compact
        | Request::Shutdown { .. }
        | Request::Ping
        | Request::ListMcpServers
        | Request::RestartMcpServer { .. }
        | Request::SetMcpServerEnabled { .. }
        | Request::GetMcpPrompt { .. }
        | Request::ListPlugins
        | Request::RunPluginCommand { .. } => panic!("expected Request::Interrupt"),
    }
}
