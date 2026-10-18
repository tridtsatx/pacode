use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::builtin::agent::{AgentTool, MAX_MESSAGE_CHARS};
use crate::test_support::{StubToolHost, stub_ctx};

fn ctx() -> ToolCtx {
    stub_ctx(Arc::new(StubToolHost::default()))
}

#[test]
fn schema_requires_only_the_question_and_defaults_to_waiting() {
    let schema = AskParentTool.schema();
    assert_eq!(schema["required"], json!(["question"]));
    let props = schema["properties"].as_object().unwrap();
    assert!(props.contains_key("question"));
    assert_eq!(props["options"]["type"], "array");
    assert_eq!(props["options"]["items"]["type"], "string");
    assert_eq!(props["options"]["maxItems"], MAX_OPTIONS);
    assert_eq!(props["wait"]["type"], "boolean");
    assert_eq!(props["wait"]["default"], true);
    assert_eq!(AskParentTool.kind(), ToolKind::Control);

    let def = AskParentTool.definition();
    assert_eq!(def.name, "ask_parent");
    assert!(def.input_schema["properties"]["intent"].is_object());
}

#[test]
fn description_steers_towards_assumptions() {
    let d = AskParentTool.description();
    assert!(d.contains("blocked"));
    assert!(d.contains("assume"));
    assert!(d.contains("wait"));
}

#[test]
fn it_is_registered_for_subagents_by_default() {
    let all = crate::builtin_tools(&pacode_types::WebConfig::default());
    assert!(all.get(NAME).is_some());
    assert!(crate::subagent_tool_names(&all).iter().any(|n| n == NAME));
    // The core hides it from the main agent with `without`.
    assert!(all.without(NAME).get(NAME).is_none());
    assert!(all.without(NAME).get("agent").is_some());
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

#[test]
fn reply_texts_tell_the_subagent_what_to_do() {
    assert_eq!(
        reply_text(&ParentReply::Answered {
            question_id: "q3".into(),
            text: "use sqlite".into()
        }),
        "The orchestrator answered q3: use sqlite"
    );
    assert_eq!(
        reply_text(&ParentReply::TimedOut {
            question_id: "q3".into()
        }),
        "no answer from orchestrator; make a reasonable assumption and state it"
    );
    let pending = reply_text(&ParentReply::Pending {
        question_id: "q3".into(),
    });
    assert!(pending.contains("<answer_from_orchestrator question=\"q3\">"));
    let cancelled = reply_text(&ParentReply::Cancelled {
        question_id: "q3".into(),
    });
    assert!(cancelled.contains("q3") && cancelled.contains("assumption"));
}

#[tokio::test]
async fn an_empty_question_is_refused_before_the_host_is_asked() {
    let err = AskParentTool
        .call(json!({"question": "  ", "intent": "ask"}), &ctx())
        .await
        .expect_err("empty");
    assert!(err.to_string().contains("question is required"), "{err}");
}

#[tokio::test]
async fn a_host_without_support_reports_it() {
    let err = AskParentTool
        .call(
            json!({"question": "Which?", "wait": false, "intent": "ask"}),
            &ctx(),
        )
        .await
        .expect_err("stub host");
    assert!(err.to_string().contains("not supported"), "{err}");
}

#[test]
fn agent_tool_offers_message_and_answer() {
    let schema = AgentTool.schema();
    let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
    for wanted in ["spawn", "stop", "list", "ask_status", "message", "answer"] {
        assert!(
            actions.iter().any(|a| a == wanted),
            "missing action {wanted}"
        );
    }
    assert_eq!(schema["properties"]["text"]["type"], "string");
    assert_eq!(schema["properties"]["question_id"]["type"], "string");
    let desc = AgentTool.description();
    assert!(desc.contains("message"));
    assert!(desc.contains("answer"));
    assert!(desc.contains("<question_from_agent"));
    assert!(desc.contains("reasonable assumption"));
}

#[tokio::test]
async fn agent_message_and_answer_validate_their_arguments() {
    let c = ctx();
    for (input, needle) in [
        (
            json!({"action": "message", "text": "hi", "intent": "x"}),
            "agent_id is required",
        ),
        (
            json!({"action": "message", "agent_id": "agt_1", "intent": "x"}),
            "text is required",
        ),
        (
            json!({"action": "message", "agent_id": "agt_1", "text": "   ", "intent": "x"}),
            "text is required",
        ),
        (
            json!({"action": "answer", "agent_id": "agt_1", "text": "hi", "intent": "x"}),
            "question_id is required",
        ),
        (
            json!({"action": "answer", "text": "hi", "question_id": "q1", "intent": "x"}),
            "agent_id is required",
        ),
    ] {
        let err = AgentTool.call(input, &c).await.expect_err("invalid");
        assert!(err.to_string().contains(needle), "{needle}: {err}");
    }

    let long = "x".repeat(MAX_MESSAGE_CHARS + 1);
    let err = AgentTool
        .call(
            json!({"action": "message", "agent_id": "agt_1", "text": long, "intent": "x"}),
            &c,
        )
        .await
        .expect_err("too long");
    assert!(err.to_string().contains("4000"), "{err}");
}

#[tokio::test]
async fn agent_message_goes_to_the_host() {
    // The stub host does not implement messaging, so the call reaches it and fails there.
    let err = AgentTool
        .call(
            json!({"action": "message", "agent_id": "agt_1", "text": "go", "intent": "x"}),
            &ctx(),
        )
        .await
        .expect_err("stub host");
    assert!(err.to_string().contains("not supported"), "{err}");
}
