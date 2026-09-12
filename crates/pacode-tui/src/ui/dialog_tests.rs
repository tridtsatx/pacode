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
