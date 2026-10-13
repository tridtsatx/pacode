use std::sync::Arc;

use serde_json::json;

use super::{AskParentTool, UNANSWERED};
use crate::host::ParentReply;
use crate::test_support::{StubToolHost, stub_ctx};
use crate::{Tool, ToolError};

#[tokio::test]
async fn the_answer_is_the_call_result() {
    let host = Arc::new(StubToolHost::default());
    *host.parent_reply.lock().unwrap() = Some(ParentReply::Answered("use postgres".into()));
    let ctx = stub_ctx(host.clone());

    let out = AskParentTool::default()
        .call(json!({"question": "  sqlite or postgres?  "}), &ctx)
        .await
        .unwrap();

    assert!(out.content.contains("use postgres"), "{}", out.content);
    let asked = host.asked.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0, ctx.call_id);
    assert_eq!(asked[0].1, "sqlite or postgres?");
}

#[tokio::test]
async fn no_answer_tells_the_subagent_to_proceed() {
    let host = Arc::new(StubToolHost::default());
    *host.parent_reply.lock().unwrap() = Some(ParentReply::Unanswered);
    let out = AskParentTool::default()
        .call(json!({"question": "which?"}), &stub_ctx(host))
        .await
        .unwrap();
    assert_eq!(out.content, UNANSWERED);
}

#[tokio::test]
async fn an_empty_question_never_reaches_the_host() {
    let host = Arc::new(StubToolHost::default());
    let err = AskParentTool::default()
        .call(json!({"question": "   "}), &stub_ctx(host.clone()))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
    assert!(host.asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_host_refusal_is_the_tool_error() {
    // The stub refuses like the core does for the main agent.
    let host = Arc::new(StubToolHost::default());
    let err = AskParentTool::default()
        .call(json!({"question": "which?"}), &stub_ctx(host))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
}

#[tokio::test]
async fn every_way_without_an_answer_tells_the_subagent_to_go_on() {
    for (reply, expected) in [
        (ParentReply::ParentFailed, super::PARENT_FAILED),
        (ParentReply::NotAnswering, super::NOT_ANSWERING),
    ] {
        let host = Arc::new(StubToolHost::default());
        *host.parent_reply.lock().unwrap() = Some(reply);
        let out = AskParentTool::default()
            .call(json!({"question": "which?"}), &stub_ctx(host))
            .await
            .unwrap();
        assert_eq!(out.content, expected);
    }
}
