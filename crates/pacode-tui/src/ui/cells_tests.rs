use pacode_render::{ColorMode, Theme};

use super::*;

fn opts(width: u16) -> RenderOptions {
    RenderOptions::with_theme_color(width, false, Theme::truecolor(), ColorMode::Truecolor)
}

fn text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn width_of(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| display_width(&s.content)).sum()
}

fn bash(command: &str, output: &str, exit_code: Option<i32>, truncated: bool) -> TranscriptKind {
    TranscriptKind::BashCommand {
        command: command.into(),
        output: output.into(),
        exit_code,
        truncated,
    }
}

fn render(kind: &TranscriptKind, opts: &RenderOptions, expanded: bool) -> Vec<Line<'static>> {
    render_item_inner(kind, None, None, None, opts, 0, expanded)
}

#[test]
fn the_user_message_is_a_full_width_band_with_a_bar_and_bold_text() {
    let opts = opts(30);
    let lines = render(
        &TranscriptKind::User {
            text: "make me a weather bot that also tells jokes".into(),
        },
        &opts,
        false,
    );
    // Two wrapped rows and the blank separator.
    assert_eq!(lines.len(), 3);
    for row in &lines[..2] {
        assert!(text(row).starts_with("▌ "));
        assert_eq!(width_of(row), 30, "the band spans the whole row");
        for span in &row.spans {
            assert_eq!(span.style.bg, opts.theme.surface.bg);
        }
        assert!(
            row.spans[1]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }
    assert_eq!(
        lines[2],
        Line::default(),
        "the separator is not part of the band"
    );
}

#[test]
fn the_user_band_falls_back_to_an_ascii_bar() {
    let opts = RenderOptions::with_theme_color(20, true, Theme::ansi(), ColorMode::Ansi);
    let lines = render(&TranscriptKind::User { text: "hi".into() }, &opts, false);
    assert!(text(&lines[0]).starts_with("| hi"));
    // ANSI-16 has no band background.
    assert!(lines[0].spans.iter().all(|s| s.style.bg.is_none()));
}

#[test]
fn the_answer_carries_a_marker_and_a_hanging_indent() {
    let opts = opts(24);
    let lines = render(
        &TranscriptKind::Assistant {
            text: "Sure, I will scaffold a command line tool first.".into(),
            complete: true,
        },
        &opts,
        false,
    );
    assert!(text(&lines[0]).starts_with("● Sure"));
    let body: Vec<&Line<'_>> = lines.iter().filter(|l| !l.spans.is_empty()).collect();
    assert!(body.len() > 1, "the answer wraps at this width");
    for row in &body[1..] {
        assert!(
            text(row).starts_with("  "),
            "continuation rows hang under the text"
        );
    }
    for row in &lines {
        assert!(
            width_of(row) <= 24,
            "the indent never pushes a row past the edge"
        );
    }
}

#[test]
fn a_live_tail_takes_the_marker_only_when_nothing_above_has_it() {
    let opts = opts(40);
    let first = assistant_markdown("hello", &opts, true);
    let tail = assistant_markdown("world", &opts, false);
    assert!(text(&first[0]).starts_with("● "));
    assert!(text(&tail[0]).starts_with("  world"));
}

/// B24: a long `!cmd` header used to run off the unwrapped paragraph.
#[test]
fn a_long_bang_command_is_cut_to_the_header_width() {
    let opts = opts(40);
    let command = format!("echo {}", "x".repeat(200));
    let lines = render(&bash(&command, "ok", Some(0), false), &opts, false);
    for row in &lines {
        assert!(width_of(row) <= 40, "row too wide: {:?}", text(row));
    }
    assert!(text(&lines[0]).contains("BASH"));
}

/// B24: a multi-line command used to put raw newlines into one span.
#[test]
fn a_multi_line_bang_command_never_puts_a_newline_in_a_row() {
    let opts = opts(60);
    let lines = render(&bash("echo a\necho b", "a\nb", Some(0), false), &opts, true);
    for row in &lines {
        assert!(!text(row).contains('\n'));
    }
    let all: Vec<String> = lines.iter().map(text).collect();
    assert!(all.iter().any(|l| l.contains("echo a")));
    assert!(
        all.iter().any(|l| l.contains("echo b")),
        "expanded shows the whole command"
    );
}

/// B24: every output line used to render, with no fold and no ctrl+o.
#[test]
fn bang_command_output_folds_and_expands_like_a_tool_call() {
    let opts = opts(60);
    let output: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
    let kind = bash("seq 40", &output.join("\n"), Some(0), false);
    let folded = render(&kind, &opts, false);
    let expanded = render(&kind, &opts, true);
    let folded_text: Vec<String> = folded.iter().map(text).collect();
    assert!(folded_text.iter().any(|l| l.contains("more lines")));
    assert!(!folded_text.iter().any(|l| l.contains("line 20")));
    let expanded_text: Vec<String> = expanded.iter().map(text).collect();
    assert!(expanded_text.iter().any(|l| l.contains("line 20")));
    assert!(expanded.len() > folded.len());
}

/// B24: the `truncated` flag used to be ignored.
#[test]
fn a_capped_bang_command_says_its_output_was_cut() {
    let opts = opts(60);
    let lines = render(&bash("yes", "y\ny", Some(0), true), &opts, false);
    assert!(lines.iter().any(|l| text(l).contains("output truncated")));
    let lines = render(&bash("yes", "y\ny", Some(0), false), &opts, false);
    assert!(!lines.iter().any(|l| text(l).contains("output truncated")));
}

#[test]
fn a_failed_bang_command_gets_a_failed_badge_and_its_exit_code() {
    let opts = opts(60);
    let lines = render(&bash("false", "", Some(1), false), &opts, false);
    let head = text(&lines[0]);
    assert!(head.starts_with(&format!(" {} BASH ", opts.glyphs.fail)));
    assert_eq!(
        lines[0].spans[0].style,
        opts.theme.badge(opts.theme.red),
        "failure is a red plaque"
    );
    assert!(lines.iter().any(|l| text(l).contains("Exited with code 1")));
}
