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
    // A padding row, two wrapped rows, a padding row, the blank separator.
    assert_eq!(lines.len(), 5);
    for row in &lines[..4] {
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
    assert!(text(&lines[1]).contains("make me"));
    for pad in [&lines[0], &lines[3]] {
        assert_eq!(
            text(pad).trim_end(),
            "▌",
            "a padding row is the bar on an empty band"
        );
    }
    assert_eq!(
        lines[4],
        Line::default(),
        "the separator is not part of the band"
    );
}

#[test]
fn the_user_band_padding_rows_copy_as_blank_lines() {
    let opts = opts(30);
    let kind = TranscriptKind::User { text: "hi".into() };
    let lines = render(&kind, &opts, false);
    let cell = CellKind::Item(kind.clone());
    for row in &lines[..3] {
        let lead = decoration_width(&cell, row, &opts);
        assert_eq!(lead, 2, "the bar is decoration on every band row");
    }
    let copied: Vec<String> = lines[..3]
        .iter()
        .map(|row| {
            let t = text(row);
            t.chars().skip(2).collect::<String>().trim_end().to_string()
        })
        .collect();
    assert_eq!(copied, ["", "hi", ""]);
}

#[test]
fn the_user_band_falls_back_to_an_ascii_bar() {
    let opts = RenderOptions::with_theme_color(20, true, Theme::ansi(), ColorMode::Ansi);
    let lines = render(&TranscriptKind::User { text: "hi".into() }, &opts, false);
    assert!(text(&lines[0]).starts_with("| "));
    assert!(text(&lines[1]).starts_with("| hi"));
    // ANSI-16 has no band background.
    for row in &lines {
        assert!(row.spans.iter().all(|s| s.style.bg.is_none()));
    }
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

#[test]
fn a_bang_command_sits_on_the_band_of_its_exit_code() {
    let opts = opts(40);
    for (code, band) in [
        (0, opts.theme.tool_success_bg),
        (2, opts.theme.tool_error_bg),
    ] {
        let lines = render(&bash("make", "out", Some(code), false), &opts, false);
        let (gap, block) = lines.split_last().expect("a block and its gap");
        assert_eq!(*gap, Line::default());
        for row in block {
            assert_eq!(width_of(row), 40);
            assert_eq!(row.spans.last().and_then(|s| s.style.bg), band.bg);
        }
    }
}

#[test]
fn a_finished_background_job_is_tinted_by_its_outcome() {
    use crate::state::{BackgroundKind, BackgroundOutcome, BackgroundResult};

    let job = |outcome| BackgroundResult {
        kind: BackgroundKind::Task,
        label: "cargo build".into(),
        outcome,
        exit_code: None,
        duration_ms: 1500,
    };
    let opts = opts(40);
    for (outcome, band) in [
        (BackgroundOutcome::Completed, opts.theme.tool_success_bg),
        (BackgroundOutcome::Failed, opts.theme.tool_error_bg),
        (BackgroundOutcome::Killed, opts.theme.tool_pending_bg),
    ] {
        let lines = render_background_result(&job(outcome), 40, &opts);
        assert_eq!(width_of(&lines[0]), 40);
        for span in &lines[0].spans {
            assert_eq!(span.style.bg, band.bg, "{outcome:?}: {:?}", span.content);
        }
    }
    let ansi = RenderOptions::with_theme_color(40, false, Theme::ansi(), ColorMode::Ansi);
    let lines = render_background_result(&job(BackgroundOutcome::Failed), 40, &ansi);
    assert!(lines[0].spans.iter().all(|s| s.style.bg.is_none()));
}

/// The daemon's plan-reminder line: one faint row and the separator.
#[test]
fn an_info_notice_is_one_faint_line() {
    let opts = opts(60);
    let lines = render(
        &TranscriptKind::Notice {
            level: ToastLevel::Info,
            text: "↻ 2 plan items open — continuing (1/3)".into(),
        },
        &opts,
        false,
    );
    assert_eq!(lines.len(), 2);
    assert_eq!(text(&lines[0]), "↻ 2 plan items open — continuing (1/3)");
    assert!(lines[0].spans.iter().all(|s| s.style == opts.theme.faint));
    assert!(text(&lines[1]).is_empty());
}
