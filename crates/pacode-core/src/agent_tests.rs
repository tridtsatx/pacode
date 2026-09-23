use std::sync::Arc;

use pacode_tools::ToolRegistry;
use pacode_types::{
    AgentId, AgentInfo, AgentKind, AgentStatus, CallId, ContentBlock, ModelRoute,
};

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
