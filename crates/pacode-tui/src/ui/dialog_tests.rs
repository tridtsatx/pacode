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
