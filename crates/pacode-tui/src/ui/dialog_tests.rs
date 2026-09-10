use super::*;
use pacode_render::RenderOptions;
use pacode_types::TranscriptKind;
use pacode_types::transcript::ToolStatus;

#[test]
fn test_failed_tool_call_shows_exit_code_on_header() {
    let opts = RenderOptions::new(80, false);
    let kind = TranscriptKind::ToolCall {
        call_id: "call_fail".into(),
        name: "bash".into(),
        title: "bash cargo test".into(),
        intent: None,
        status: ToolStatus::Error,
        preview: "[exit code 2]".into(),
        diff: None,
        duration_ms: Some(56),
        task: None,
    };
    let lines = render_item(&kind, None, 80, &opts, 0);
    // Header row is line 0
    let header_text = lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>();
    assert!(
        header_text.contains("exit 2"),
        "header row must contain 'exit 2', got: '{header_text}'"
    );
    assert!(
        header_text.contains("fail 56ms · exit 2"),
        "header row must format duration and exit code, got: '{header_text}'"
    );
    // No gutter row containing │ since preview only had exit code
    assert!(
        !lines
            .iter()
            .any(|l| { l.spans.iter().any(|s| s.content.contains('│')) }),
        "gutter must not render for preview that only has exit code"
    );
}

#[test]
fn test_failed_tool_call_with_empty_preview_renders_one_line_and_no_gutter() {
    let opts = RenderOptions::new(80, false);
    let kind = TranscriptKind::ToolCall {
        call_id: "call_empty".into(),
        name: "bash".into(),
        title: "bash exit 1".into(),
        intent: None,
        status: ToolStatus::Error,
        preview: "".into(),
        diff: None,
        duration_ms: None,
        task: None,
    };
    let lines = render_item(&kind, None, 80, &opts, 0);

    // Filter out the trailing blank separator line (spans is empty)
    let content_lines: Vec<&Line> = lines.iter().filter(|l| !l.spans.is_empty()).collect();
    assert_eq!(
        content_lines.len(),
        1,
        "empty preview must render exactly one line of content"
    );

    let header_text = content_lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>();
    assert!(header_text.contains("fail"));
    assert!(
        !lines
            .iter()
            .any(|l| { l.spans.iter().any(|s| s.content.contains('│')) }),
        "empty preview must produce no gutter row"
    );

    // Also test with whitespace preview
    let kind_ws = TranscriptKind::ToolCall {
        call_id: "call_ws".into(),
        name: "bash".into(),
        title: "bash true".into(),
        intent: None,
        status: ToolStatus::Error,
        preview: "   \n\n  \n".into(),
        diff: None,
        duration_ms: None,
        task: None,
    };
    let lines_ws = render_item(&kind_ws, None, 80, &opts, 0);
    let content_lines_ws: Vec<&Line> = lines_ws.iter().filter(|l| !l.spans.is_empty()).collect();
    assert_eq!(
        content_lines_ws.len(),
        1,
        "whitespace preview must render exactly one line of content"
    );
    assert!(
        !lines_ws
            .iter()
            .any(|l| { l.spans.iter().any(|s| s.content.contains('│')) }),
        "whitespace preview must produce no gutter row"
    );
}

#[test]
fn test_successful_call_is_unchanged() {
    let opts = RenderOptions::new(80, false);
    let kind = TranscriptKind::ToolCall {
        call_id: "call_ok".into(),
        name: "read_file".into(),
        title: "read_file foo.rs".into(),
        intent: None,
        status: ToolStatus::Ok,
        preview: "fn main() {}".into(),
        diff: None,
        duration_ms: Some(40),
        task: None,
    };
    let lines = render_item(&kind, None, 80, &opts, 0);
    let header_text = lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>();
    assert!(header_text.contains("ok 40ms"));
    // Gutter row must be present for non-empty preview
    assert!(
        lines
            .iter()
            .any(|l| { l.spans.iter().any(|s| s.content.contains('│')) }),
        "successful tool call with content must render gutter row"
    );
    assert!(
        lines
            .iter()
            .any(|l| { l.spans.iter().any(|s| s.content.contains("fn main() {}")) }),
        "preview text must be rendered in gutter"
    );
}

fn long_output_call(lines: usize) -> TranscriptKind {
    let preview = (0..lines)
        .map(|i| format!("output line {i} that is long enough to be cut at a narrow width"))
        .collect::<Vec<_>>()
        .join("\n");
    TranscriptKind::ToolCall {
        call_id: pacode_types::CallId::new("call_1"),
        name: "bash".into(),
        title: "bash python3 -c ...".into(),
        intent: None,
        status: pacode_types::ToolStatus::Ok,
        preview,
        diff: None,
        duration_ms: Some(237),
        task: None,
    }
}

fn text_of(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

#[test]
fn a_collapsed_tool_call_shows_a_few_lines_and_says_how_many_are_hidden() {
    let opts = pacode_render::RenderOptions::new(60, false);
    let lines = super::render_item_inner(&long_output_call(20), None, 60, &opts, 0, false);
    let text = text_of(&lines);

    let body: Vec<&String> = text.iter().filter(|l| l.starts_with("  │")).collect();
    assert_eq!(body.len(), super::COLLAPSED_MAX_LINES + 1, "{body:?}");
    assert!(
        body.last().expect("last").contains("+14 more lines"),
        "{body:?}"
    );
    assert!(body.last().expect("last").contains("click to expand"));
    // Collapsed rows are cut to the width rather than wrapped.
    assert!(body[0].contains('…'), "{:?}", body[0]);
}

#[test]
fn an_expanded_tool_call_shows_every_line_wrapped() {
    let opts = pacode_render::RenderOptions::new(60, false);
    let lines = super::render_item_inner(&long_output_call(20), None, 60, &opts, 0, true);
    let text = text_of(&lines);

    let body: Vec<&String> = text.iter().filter(|l| l.starts_with("  │")).collect();
    // Every source line is there, and long ones took more than one row.
    assert!(
        body.len() > 20,
        "expanded output must show it all: {}",
        body.len()
    );
    assert!(body.iter().any(|l| l.contains("output line 19")));
    assert!(
        body.iter().all(|l| !l.contains('…')),
        "expanded rows wrap instead of being cut"
    );
    assert!(body.iter().all(|l| !l.contains("click to expand")));
}

#[test]
fn an_expanded_tool_call_is_still_bounded() {
    let opts = pacode_render::RenderOptions::new(60, false);
    let lines = super::render_item_inner(&long_output_call(5000), None, 60, &opts, 0, true);
    let text = text_of(&lines);

    let body: Vec<&String> = text.iter().filter(|l| l.starts_with("  │")).collect();
    assert!(
        body.len() <= super::EXPANDED_MAX_LINES + 1,
        "an enormous output must stay bounded: {}",
        body.len()
    );
    assert!(
        body.last().expect("last").contains("output truncated"),
        "{:?}",
        body.last()
    );
}

#[test]
fn a_short_output_needs_no_expansion_hint() {
    let opts = pacode_render::RenderOptions::new(60, false);
    let lines = super::render_item_inner(&long_output_call(2), None, 60, &opts, 0, false);
    let text = text_of(&lines);
    assert!(
        text.iter().all(|l| !l.contains("click to expand")),
        "{text:?}"
    );
}

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
