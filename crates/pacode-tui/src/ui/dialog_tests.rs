use super::*;

fn question_cell(answer: Option<pacode_types::QuestionAnswer>) -> TranscriptKind {
    let question = pacode_types::Question::new(
        pacode_types::QuestionId::new("qst_1"),
        pacode_types::QuestionOrigin::new(
            pacode_types::AgentId::main(),
            "main",
            pacode_types::CallId::new("call_1"),
        ),
        "Platform",
        "Which platform would you like your weather bot built for?",
        vec![
            pacode_types::QuestionOption::new("Multi-Interface", "everything at once")
                .recommended(),
            pacode_types::QuestionOption::new("Telegram Bot", "native chat"),
        ],
        false,
        0,
    )
    .expect("question");
    TranscriptKind::Question { question, answer }
}

#[test]
fn an_unanswered_question_does_not_repeat_the_picker() {
    let opts = pacode_render::RenderOptions::new(80, false);
    let lines = super::render_item_inner(&question_cell(None), None, 80, &opts, 0, false);
    let text: Vec<String> = lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    let joined = text.join("\n");

    // The question itself stays in the conversation…
    assert!(joined.contains("Platform"), "{joined}");
    assert!(joined.contains("Which platform"), "{joined}");
    assert!(joined.contains("waiting for your answer"), "{joined}");
    // …but the options and the key hints belong to the picker below it.
    assert!(!joined.contains("Multi-Interface"), "{joined}");
    assert!(!joined.contains("recommended"), "{joined}");
    assert!(!joined.contains("esc dismiss"), "{joined}");
}

#[test]
fn an_answered_question_shows_what_was_chosen() {
    let opts = pacode_render::RenderOptions::new(80, false);
    let answered = question_cell(Some(pacode_types::QuestionAnswer::choice(1)));
    let lines = super::render_item_inner(&answered, None, 80, &opts, 0, false);
    let joined: String = lines
        .iter()
        .map(|l| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(joined.contains("Telegram Bot"), "{joined}");
    assert!(joined.contains("Multi-Interface"), "{joined}");
    assert!(!joined.contains("waiting for your answer"), "{joined}");

    let dismissed = question_cell(Some(pacode_types::QuestionAnswer::cancelled()));
    let lines = super::render_item_inner(&dismissed, None, 80, &opts, 0, false);
    let joined: String = lines
        .iter()
        .map(|l| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("dismissed"), "{joined}");
}

#[test]
fn an_answered_question_keeps_the_description_and_the_typed_text() {
    let opts = pacode_render::RenderOptions::new(80, false);
    let mut answer = pacode_types::QuestionAnswer::choice(1);
    answer.free_text = Some("with a twist".to_string());
    let answered = question_cell(Some(answer));
    let lines = super::render_item_inner(&answered, None, 80, &opts, 0, false);
    let joined: String = lines
        .iter()
        .map(|l| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() })
        .collect::<Vec<_>>()
        .join("\n");

    // The chosen option shows its description; the unchosen one does not.
    assert!(joined.contains("native chat"), "{joined}");
    assert!(!joined.contains("everything at once"), "{joined}");
    // Free text renders as the `> ` line, not a `typed:` label.
    assert!(joined.contains("> with a twist"), "{joined}");
    assert!(!joined.contains("typed:"), "{joined}");
}

fn notice_text(level: ToastLevel, text: &str, width: u16) -> String {
    let opts = pacode_render::RenderOptions::new(width, false);
    let kind = TranscriptKind::Notice {
        level,
        text: text.to_string(),
    };
    super::render_item_inner(&kind, None, width, &opts, 0, false)
        .iter()
        .map(|l| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn an_error_notice_is_a_classified_block() {
    let joined = notice_text(
        ToastLevel::Error,
        "turn failed: authentication failed: Invalid x-api-key",
        80,
    );
    assert!(joined.contains("✗ turn failed · auth"), "{joined}");
    assert!(joined.contains("│ Invalid x-api-key"), "{joined}");
    assert!(!joined.contains("authentication failed:"), "{joined}");

    let joined = notice_text(
        ToastLevel::Error,
        "turn failed: rate limited: slow down",
        80,
    );
    assert!(joined.contains("turn failed · rate limit"), "{joined}");

    let joined = notice_text(
        ToastLevel::Error,
        "turn failed: request failed (429): too many requests",
        80,
    );
    assert!(joined.contains("turn failed · http 429"), "{joined}");
    assert!(joined.contains("│ too many requests"), "{joined}");

    let joined = notice_text(
        ToastLevel::Error,
        "turn failed: transport: connection reset by peer",
        80,
    );
    assert!(joined.contains("turn failed · network"), "{joined}");

    let joined = notice_text(ToastLevel::Error, "turn failed: stream idle for 30s", 80);
    assert!(joined.contains("turn failed · timeout"), "{joined}");
    assert!(joined.contains("for 30s"), "{joined}");

    let joined = notice_text(
        ToastLevel::Error,
        "turn failed: malformed response: missing field `type`",
        80,
    );
    assert!(joined.contains("turn failed · malformed"), "{joined}");

    // An unknown inner prefix keeps the wrapper as the title and the rest as
    // the body — nothing is dropped.
    let joined = notice_text(
        ToastLevel::Error,
        "turn failed: provider exploded: details here",
        80,
    );
    assert!(joined.contains("Turn failed"), "{joined}");
    assert!(
        joined.contains("provider exploded: details here"),
        "{joined}"
    );
}

#[test]
fn an_error_notice_pulls_the_message_out_of_a_json_body() {
    let joined = notice_text(
        ToastLevel::Error,
        "turn failed: request failed (400): {\"error\":{\"code\":\"invalid_request_error\",\"message\":\"Invalid model specified: foo\"}}",
        80,
    );
    assert!(joined.contains("turn failed · http 400"), "{joined}");
    assert!(joined.contains("Invalid model specified: foo"), "{joined}");
    assert!(!joined.contains("invalid_request_error"), "{joined}");

    // Unparseable JSON stays raw rather than vanishing.
    let joined = notice_text(
        ToastLevel::Error,
        "turn failed: request failed (500): {not json",
        80,
    );
    assert!(joined.contains("{not json"), "{joined}");
}

#[test]
fn an_error_notice_without_the_turn_wrapper_uses_its_own_prefix() {
    let joined = notice_text(ToastLevel::Error, "Export failed: disk is full", 80);
    assert!(joined.contains("Export failed"), "{joined}");
    assert!(joined.contains("disk is full"), "{joined}");

    // A single line with no label prefix is its own title.
    let joined = notice_text(ToastLevel::Error, "the daemon is gone", 80);
    assert!(joined.contains("The daemon is gone"), "{joined}");

    // A URL does not count as a label prefix.
    let joined = notice_text(ToastLevel::Error, "see http://x/y for details", 80);
    assert!(joined.contains("See http://x/y for details"), "{joined}");
    assert!(!joined.contains("│ "), "{joined}");
}

#[test]
fn a_long_error_body_wraps_onto_the_gutter() {
    let body = "word ".repeat(40);
    let text = format!("turn failed: transport: {}", body.trim());
    let joined = notice_text(ToastLevel::Error, &text, 40);
    let gutter_lines = joined.lines().filter(|l| l.starts_with("│ ")).count();
    assert!(gutter_lines > 1, "{joined}");

    // A multi-line body keeps every line on the gutter.
    let joined = notice_text(
        ToastLevel::Error,
        "turn failed: transport: first\nsecond",
        80,
    );
    assert!(joined.contains("│ first"), "{joined}");
    assert!(joined.contains("│ second"), "{joined}");
}

#[test]
fn non_error_notices_stay_plain_but_wrap() {
    let joined = notice_text(ToastLevel::Info, &"x".repeat(200), 60);
    let lines: Vec<&str> = joined.lines().filter(|l| !l.is_empty()).collect();
    assert!(lines.len() > 1, "{joined}");
    assert!(!joined.contains('✗'), "{joined}");
}
