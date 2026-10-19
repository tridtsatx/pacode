use std::sync::Arc;

use serde_json::json;

use super::{
    AskParentTool, MAX_OPTION_CHARS, MAX_OPTIONS, MAX_QUESTION_CHARS, NAME, UNANSWERED,
    check_question, clean_options, compose_question,
};
use crate::host::ParentReply;
use crate::test_support::{StubToolHost, stub_ctx};
use crate::{Tool, ToolError, ToolKind};

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

#[test]
fn schema_requires_only_the_question_and_offers_options() {
    let tool = AskParentTool::default();
    let schema = tool.schema();
    assert_eq!(schema["required"], json!(["question"]));
    let props = schema["properties"].as_object().unwrap();
    assert!(props.contains_key("question"));
    assert_eq!(props["options"]["type"], "array");
    assert_eq!(props["options"]["items"]["type"], "string");
    assert_eq!(props["options"]["maxItems"], MAX_OPTIONS);
    assert_eq!(tool.kind(), ToolKind::Control);

    let def = tool.definition();
    assert_eq!(def.name, "ask_parent");
}

#[test]
fn it_is_registered_for_subagents_by_default() {
    let all = crate::builtin_tools(&pacode_types::WebConfig::default());
    assert!(all.get(NAME).is_some());
    assert!(crate::subagent_tool_names(&all).iter().any(|n| n == NAME));
    assert!(crate::orchestrator_tools(&all).get(NAME).is_none());
}

#[test]
fn question_is_trimmed_and_bounded() {
    assert_eq!(check_question("  why?  ").unwrap(), "why?");
    assert!(check_question("   ").is_err());
    assert!(check_question(&"x".repeat(MAX_QUESTION_CHARS)).is_ok());
    let err = check_question(&"x".repeat(MAX_QUESTION_CHARS + 1)).unwrap_err();
    assert!(err.to_string().contains("2000"), "{err}");
}

#[test]
fn options_are_cleaned_and_capped() {
    let many: Vec<String> = (0..10).map(|i| format!(" opt{i} ")).collect();
    let cleaned = clean_options(many);
    assert_eq!(cleaned.len(), MAX_OPTIONS);
    assert_eq!(cleaned[0], "opt0");
    assert_eq!(
        clean_options(vec!["".into(), "  ".into(), "a".into()]),
        vec!["a"]
    );
    let long = clean_options(vec!["y".repeat(MAX_OPTION_CHARS + 50)]);
    assert_eq!(long[0].chars().count(), MAX_OPTION_CHARS);
}

#[tokio::test]
async fn options_reach_the_orchestrator_with_the_question() {
    assert_eq!(compose_question("which?", &[]), "which?");
    let host = Arc::new(StubToolHost::default());
    *host.parent_reply.lock().unwrap() = Some(ParentReply::Answered("2".into()));
    AskParentTool::default()
        .call(
            json!({"question": "which db?", "options": [" sqlite ", "", "postgres"]}),
            &stub_ctx(host.clone()),
        )
        .await
        .unwrap();
    let asked = host.asked.lock().unwrap();
    assert_eq!(asked[0].1, "which db?\n\nOptions:\n1. sqlite\n2. postgres");
}

#[tokio::test]
async fn an_overlong_question_never_reaches_the_host() {
    let host = Arc::new(StubToolHost::default());
    let err = AskParentTool::default()
        .call(
            json!({"question": "x".repeat(MAX_QUESTION_CHARS + 1)}),
            &stub_ctx(host.clone()),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
    assert!(host.asked.lock().unwrap().is_empty());
}
