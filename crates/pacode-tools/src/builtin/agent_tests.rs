use std::sync::Arc;

use pacode_types::AgentId;
use serde_json::json;

use super::{AgentTool, MAX_MESSAGE_CHARS};
use crate::host::SendOutcome;
use crate::test_support::{StubToolHost, stub_ctx};
use crate::{Tool, ToolError};

async fn send(host: &Arc<StubToolHost>, input: serde_json::Value) -> Result<String, ToolError> {
    AgentTool
        .call(input, &stub_ctx(host.clone()))
        .await
        .map(|out| out.content)
}

#[tokio::test]
async fn send_names_what_happened_to_the_message() {
    let cases = [
        (SendOutcome::Delivered, "next step"),
        (SendOutcome::Answered, "answered it"),
        (
            SendOutcome::Resumed {
                previous_report: None,
            },
            "resumed it",
        ),
    ];
    for (outcome, needle) in cases {
        let host = Arc::new(StubToolHost::default());
        *host.send_outcome.lock().unwrap() = Some(outcome.clone());
        let content = send(
            &host,
            json!({"action": "send", "agent_id": "agt_1", "message": "use postgres"}),
        )
        .await
        .unwrap();
        assert!(content.contains(needle), "{outcome:?}: {content}");
        assert_eq!(
            host.sent.lock().unwrap().as_slice(),
            &[(AgentId::new("agt_1"), "use postgres".to_string())]
        );
    }
}

/// The report a resumed subagent ended with, unread by the orchestrator, comes
/// back with the call instead of arriving later as if it answered the message.
#[tokio::test]
async fn a_resume_hands_back_the_unread_report() {
    let host = Arc::new(StubToolHost::default());
    *host.send_outcome.lock().unwrap() = Some(SendOutcome::Resumed {
        previous_report: Some("found three bugs".into()),
    });
    let content = send(
        &host,
        json!({"action": "send", "agent_id": "agt_1", "message": "fix them"}),
    )
    .await
    .unwrap();
    assert!(content.contains("resumed it"), "{content}");
    assert!(content.contains("found three bugs"), "{content}");
}

#[tokio::test]
async fn send_requires_a_target_and_a_message() {
    let host = Arc::new(StubToolHost::default());
    *host.send_outcome.lock().unwrap() = Some(SendOutcome::Delivered);
    for input in [
        json!({"action": "send", "message": "hi"}),
        json!({"action": "send", "agent_id": "agt_1"}),
        json!({"action": "send", "agent_id": "agt_1", "message": "  "}),
    ] {
        let err = send(&host, input.clone()).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{input}");
    }
    assert!(host.sent.lock().unwrap().is_empty());
}

#[test]
fn the_parent_channel_is_subagent_only() {
    let all = crate::builtin_tools(&pacode_types::WebConfig::default());
    let main = crate::orchestrator_tools(&all);
    let sub = crate::subagent_tool_names(&all);
    for name in crate::builtin::SUBAGENT_ONLY {
        assert!(
            main.get(name).is_none(),
            "{name} offered to the orchestrator"
        );
        assert!(
            sub.iter().any(|n| n == name),
            "{name} missing for subagents"
        );
    }
    assert!(main.get(super::NAME).is_some());
    assert!(!sub.iter().any(|n| n == super::NAME));
}

#[tokio::test]
async fn a_refused_send_is_the_tool_error() {
    let host = Arc::new(StubToolHost::default());
    let err = send(
        &host,
        json!({"action": "send", "agent_id": "agt_1", "message": "hi"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::Failed(_)));
}

#[test]
fn result_action_is_in_schema() {
    let schema = AgentTool.schema();
    let action = &schema["properties"]["action"];
    let actions = action["enum"].as_array().expect("enum");
    assert!(actions.iter().any(|a| a == "result"));
    assert!(action["description"].as_str().unwrap().contains("result"));
}

#[tokio::test]
async fn result_requires_a_known_agent_id() {
    let ctx = stub_ctx(Arc::new(StubToolHost::default()));
    let missing = AgentTool
        .call(json!({"action": "result"}), &ctx)
        .await
        .unwrap_err();
    assert!(
        missing.to_string().contains("agent_id is required"),
        "{missing}"
    );

    let unknown = AgentTool
        .call(json!({"action": "result", "agent_id": "ag_nope"}), &ctx)
        .await
        .unwrap_err();
    assert!(unknown.to_string().contains("unknown agent"), "{unknown}");
}

#[tokio::test]
async fn send_bounds_the_message_and_accepts_the_old_text_name() {
    let host = Arc::new(StubToolHost::default());
    *host.send_outcome.lock().unwrap() = Some(SendOutcome::Delivered);
    let long = "x".repeat(MAX_MESSAGE_CHARS + 1);
    let err = send(
        &host,
        json!({"action": "send", "agent_id": "agt_1", "message": long}),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("4000"), "{err}");
    assert!(host.sent.lock().unwrap().is_empty());

    send(
        &host,
        json!({"action": "send", "agent_id": "agt_1", "text": "  go  "}),
    )
    .await
    .unwrap();
    assert_eq!(
        host.sent.lock().unwrap().as_slice(),
        &[(AgentId::new("agt_1"), "go".to_string())]
    );
}
