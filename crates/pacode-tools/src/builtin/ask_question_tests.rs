use serde_json::json;

use super::*;

/// The shared stub host answers with the recommended option, which is exactly
/// the path this tool has to report back correctly.
use crate::test_support::{StubToolHost, stub_ctx};

#[tokio::test]
async fn the_answer_comes_back_as_the_label_the_user_saw() {
    let ctx = stub_ctx(std::sync::Arc::new(StubToolHost::default()));

    let out = AskQuestionTool
        .call(
            json!({
                "question": "Where should the cache live?",
                "header": "Storage",
                "options": [
                    {"label": "Cache dir", "description": "the usual place"},
                    {"label": "Next to the project", "description": "portable", "recommended": true}
                ],
                "intent": "ask where the cache goes"
            }),
            &ctx,
        )
        .await
        .expect("ask");

    assert_eq!(out.content, "the user chose: Next to the project");
    assert!(out.title.contains("Where should the cache live?"));
}

#[tokio::test]
async fn a_question_the_picker_could_not_present_is_refused() {
    let ctx = stub_ctx(std::sync::Arc::new(StubToolHost::default()));

    let err = AskQuestionTool
        .call(
            json!({
                "question": "Only one way?",
                "options": [{"label": "Yes", "description": "the only option"}],
                "intent": "ask"
            }),
            &ctx,
        )
        .await
        .expect_err("one option is not a choice");
    assert!(format!("{err}").contains("at least"), "{err}");

    let err = AskQuestionTool
        .call(
            json!({
                "question": "  ",
                "options": [
                    {"label": "a", "description": ""},
                    {"label": "b", "description": ""}
                ],
                "intent": "ask"
            }),
            &ctx,
        )
        .await
        .expect_err("an empty question is not a question");
    assert!(format!("{err}").contains("empty"), "{err}");
}

#[tokio::test]
async fn a_dismissed_question_reads_as_dismissed_not_as_a_choice() {
    let answer = pacode_types::QuestionAnswer::cancelled();
    let text = answer_text(&answer, &["a".to_string()], "Which one?");
    assert!(text.contains("dismissed"), "{text}");

    let empty = pacode_types::QuestionAnswer::default();
    assert_eq!(
        answer_text(&empty, &["a".to_string()], "Which one?"),
        "The user answered with nothing."
    );

    let typed = pacode_types::QuestionAnswer::typed("something else");
    assert_eq!(
        answer_text(&typed, &["a".to_string()], "Which one?"),
        "the user wrote: something else"
    );
}

#[test]
fn a_long_question_is_shortened_for_the_title() {
    let long = "q".repeat(200);
    let short_title = short(&long);
    assert_eq!(short_title.chars().count(), 48);
    assert!(short_title.ends_with('…'));
    assert_eq!(short("already short"), "already short");
}
