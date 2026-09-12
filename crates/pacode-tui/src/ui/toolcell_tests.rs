//! Tool-cell tests (spec §6). Expected Russian strings are built through
//! `pacode_render::text` — Cyrillic literals are forbidden in this crate.

use super::*;

fn call<'a>(
    name: &'a str,
    title: &'a str,
    status: ToolStatus,
    preview: &'a str,
) -> ToolCallView<'a> {
    ToolCallView {
        name,
        title,
        status,
        preview,
        diff: None,
        duration_ms: None,
    }
}

fn text_of(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

fn lines_preview(n: usize) -> String {
    (0..n)
        .map(|i| format!("output line {i} that is long enough to be cut at a narrow width"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Body rows are the ones hanging on the `│ ` gutter.
fn body_rows<'a>(text: &'a [String], opts: &RenderOptions) -> Vec<&'a str> {
    let g = format!("{} ", opts.glyphs.vline);
    text.iter()
        .filter(|l| l.starts_with(&g))
        .map(String::as_str)
        .collect()
}

#[test]
fn header_is_label_arg_and_the_outcome_sits_flush_right() {
    let opts = RenderOptions::new(80, false);
    let mut c = call(
        "bash",
        "bash docker ps",
        ToolStatus::Ok,
        "container-a\n[exit code 0]",
    );
    c.duration_ms = Some(400);
    let lines = render(&c, 80, &opts, 0, false);
    let text = text_of(&lines);
    let header = &text[0];

    let label = rtext::tool_label("bash").unwrap_or("bash");
    let tool = opts.glyphs.tool;
    let sep = opts.glyphs.dot_sep;
    let expected_left = format!("{tool} {label}{sep}docker ps");
    assert!(header.starts_with(&expected_left), "{header}");

    // Genuinely flush right: the row is exactly `width` wide and ends with it.
    let dur = format_duration_ms(400);
    let ok = opts.glyphs.ok;
    assert_eq!(display_width(header), 80, "{header}");
    assert!(
        header.ends_with(&format!("{ok} exit 0{sep}{dur}")),
        "{header}"
    );

    // `│ $ docker ps`, then the output; the exit-code line stays out of the body.
    let body = body_rows(&text, &opts);
    assert_eq!(body.len(), 2, "{body:?}");
    assert!(body[0].contains("$ docker ps"), "{}", body[0]);
    assert!(body[1].contains("container-a"), "{}", body[1]);
}

#[test]
fn the_outcome_stays_on_the_right_edge_even_when_narrow() {
    let opts = RenderOptions::new(30, false);
    let mut c = call(
        "read_file",
        "a/very/long/path/that/wont/fit/in/thirty/cells.rs",
        ToolStatus::Ok,
        "x",
    );
    c.duration_ms = Some(40);
    let lines = render(&c, 30, &opts, 0, false);
    let text = text_of(&lines);
    let header = &text[0];
    assert_eq!(display_width(header), 30, "{header}");
    assert!(header.ends_with("40ms"), "{header}");
}

#[test]
fn collapsed_success_keeps_head_and_tail_and_counts_the_fold() {
    let opts = RenderOptions::new(60, false);
    let preview = lines_preview(20);
    let c = call("run", "run suite", ToolStatus::Ok, &preview);
    let lines = render(&c, 60, &opts, 0, false);
    let text = text_of(&lines);
    let body = body_rows(&text, &opts);

    // first 6 + fold marker + last 3
    assert_eq!(
        body.len(),
        COLLAPSED_HEAD_LINES + COLLAPSED_TAIL_LINES + 1,
        "{body:?}"
    );
    assert!(body[0].contains("output line 0 "), "{}", body[0]);
    assert!(
        body[COLLAPSED_HEAD_LINES].contains(&rtext::more_lines(11, &opts)),
        "{}",
        body[COLLAPSED_HEAD_LINES]
    );
    assert!(
        body[COLLAPSED_HEAD_LINES + 1].contains("output line 17 "),
        "{}",
        body[COLLAPSED_HEAD_LINES + 1]
    );
    assert!(body[9].contains("output line 19 "), "{}", body[9]);
    let joined = text.join("\n");
    assert!(!joined.contains("output line 6 "), "{joined}");
    assert!(!joined.contains("output line 16 "), "{joined}");
}

#[test]
fn collapsed_failure_keeps_only_the_tail() {
    let opts = RenderOptions::new(60, false);
    let preview = format!("{}\n[exit code 1]", lines_preview(20));
    let mut c = call("run", "run suite", ToolStatus::Error, &preview);
    c.duration_ms = Some(12_000);
    let lines = render(&c, 60, &opts, 0, false);
    let text = text_of(&lines);
    let body = body_rows(&text, &opts);

    // marker on top, then the last 8 lines.
    assert_eq!(body.len(), ERROR_TAIL_LINES + 1, "{body:?}");
    assert!(
        body[0].contains(&rtext::more_lines(12, &opts)),
        "{}",
        body[0]
    );
    assert!(body[1].contains("output line 12 "), "{}", body[1]);
    assert!(body[8].contains("output line 19 "), "{}", body[8]);

    let sep = opts.glyphs.dot_sep;
    let fail = opts.glyphs.fail;
    let dur = format_duration_ms(12_000);
    assert!(
        text[0].ends_with(&format!("{fail} exit 1{sep}{dur}")),
        "{}",
        text[0]
    );
    assert!(
        !text.join("\n").contains("[exit code"),
        "the exit marker is consumed by the header, not the body"
    );
}

#[test]
fn a_short_output_needs_no_marker() {
    let opts = RenderOptions::new(60, false);
    let preview = lines_preview(2);
    let c = call("run", "run suite", ToolStatus::Ok, &preview);
    let text = text_of(&render(&c, 60, &opts, 0, false));
    assert!(text.iter().all(|l| !l.contains("ctrl+o")), "{text:?}");
}

#[test]
fn an_empty_or_whitespace_preview_is_a_single_line() {
    let opts = RenderOptions::new(80, false);
    for preview in ["", "   \n\n  \n"] {
        let c = call("bash", "bash true", ToolStatus::Error, preview);
        let lines = render(&c, 80, &opts, 0, false);
        let text = text_of(&lines);
        let content: Vec<&String> = text.iter().filter(|l| !l.is_empty()).collect();
        assert_eq!(content.len(), 1, "preview {preview:?} must render one row");
        assert!(body_rows(&text, &opts).is_empty(), "{text:?}");
        assert!(content[0].contains("fail"), "{}", content[0]);
    }
}

#[test]
fn ansi_escapes_are_stripped_and_cannot_restyle_the_gutter() {
    let opts = RenderOptions::new(60, false);
    let preview = "\u{1b}[32mgreen text\u{1b}[0m\n\u{1b}[1;31mred bold\u{1b}[0m";
    let c = call("run", "run suite", ToolStatus::Ok, preview);
    let lines = render(&c, 60, &opts, 0, false);
    let text = text_of(&lines);
    let joined = text.join("\n");
    assert!(joined.contains("green text"), "{joined}");
    assert!(joined.contains("red bold"), "{joined}");
    assert!(
        !joined.contains('\u{1b}'),
        "raw escapes must never reach a span"
    );

    // The gutter is painted by US, from the outcome — not by the program.
    assert_eq!(lines[1].spans[0].style, opts.theme.green);
    // …while the program's text itself stays dim.
    assert_eq!(lines[1].spans[1].style, opts.theme.dim);
}

#[test]
fn ascii_mode_falls_back_on_plain_glyphs() {
    let opts = RenderOptions::new(60, true);
    let mut c = call(
        "bash",
        "bash cargo test",
        ToolStatus::Ok,
        "test ok\n[exit code 0]",
    );
    c.duration_ms = Some(40);
    let text = text_of(&render(&c, 60, &opts, 0, false));
    assert!(text[0].starts_with("# "), "{}", text[0]);
    assert!(text[0].ends_with("+ exit 0 . 40ms"), "{}", text[0]);
    let body = body_rows(&text, &opts);
    assert!(body.iter().all(|l| l.starts_with("| ")), "{body:?}");
}

#[test]
fn denied_calls_say_denied_on_a_red_gutter() {
    let opts = RenderOptions::new(80, false);
    let c = call("bash", "bash rm -rf /", ToolStatus::Denied, "Denied");
    let lines = render(&c, 80, &opts, 0, false);
    let text = text_of(&lines);
    assert!(text[0].ends_with("denied"), "{}", text[0]);
    // The gutter is red, and a denied bash call still shows what it would have run.
    assert_eq!(lines[1].spans[0].style, opts.theme.red);
    let body = body_rows(&text, &opts);
    assert!(body[0].contains("$ rm -rf /"), "{}", body[0]);
    assert!(body[1].contains("Denied"), "{}", body[1]);
}

#[test]
fn backgrounded_calls_show_the_background_outcome() {
    let opts = RenderOptions::new(80, false);
    let mut c = call(
        "bash",
        "bash sleeper",
        ToolStatus::Backgrounded,
        "task t1 running in background",
    );
    c.duration_ms = Some(107_000);
    let lines = render(&c, 80, &opts, 0, false);
    let text = text_of(&lines);
    let running = opts.glyphs.running;
    let sep = opts.glyphs.dot_sep;
    let bg = rtext::in_background();
    let dur = format_duration_ms(107_000);
    assert!(
        text[0].ends_with(&format!("{running} {bg}{sep}{dur}")),
        "{}",
        text[0]
    );
    assert_eq!(lines[1].spans[0].style, opts.theme.violet);
    let body = body_rows(&text, &opts);
    assert!(
        body.iter()
            .any(|l| l.contains("task t1 running in background")),
        "{body:?}"
    );
}

#[test]
fn running_calls_spin_on_the_anim_frame() {
    let opts = RenderOptions::new(80, false);
    let mut c = call("bash", "bash cargo test", ToolStatus::Running, "");
    c.duration_ms = Some(12_000);
    let header_at =
        |frame: u64, o: &RenderOptions| text_of(&render(&c, 80, o, frame, false))[0].clone();
    assert!(
        header_at(0, &opts).contains("⠋ running 12s"),
        "{}",
        header_at(0, &opts)
    );
    assert!(
        header_at(1, &opts).contains("⠙ running 12s"),
        "{}",
        header_at(1, &opts)
    );
    assert!(
        header_at(10, &opts).contains("⠋ running 12s"),
        "{}",
        header_at(10, &opts)
    );

    let ascii = RenderOptions::new(80, true);
    assert!(
        header_at(0, &ascii).contains("- running 12s"),
        "{}",
        header_at(0, &ascii)
    );
    assert!(
        header_at(1, &ascii).contains("\\ running 12s"),
        "{}",
        header_at(1, &ascii)
    );
    assert!(
        header_at(2, &ascii).contains("| running 12s"),
        "{}",
        header_at(2, &ascii)
    );
    assert!(
        header_at(3, &ascii).contains("/ running 12s"),
        "{}",
        header_at(3, &ascii)
    );
}

#[test]
fn bash_commands_get_a_dollar_line_with_syntax_highlighting() {
    let opts = RenderOptions::new(80, false);
    let c = call("bash", "bash cargo test --release", ToolStatus::Ok, "ok");
    let lines = render(&c, 80, &opts, 0, false);
    let text = text_of(&lines);
    assert!(text[1].contains("$ cargo test --release"), "{}", text[1]);
    let cmd_span = lines[1]
        .spans
        .iter()
        .find(|s| s.content == "cargo")
        .expect("the command word is its own span");
    assert_eq!(cmd_span.style, opts.theme.bash_command);
    let flag_span = lines[1]
        .spans
        .iter()
        .find(|s| s.content == "--release")
        .expect("the flag is its own span");
    assert_eq!(flag_span.style, opts.theme.bash_flag);
}

#[test]
fn non_bash_args_are_the_title_tail_not_a_dollar_line() {
    let opts = RenderOptions::new(80, false);
    let c = call(
        "read_file",
        "read_file crates/x.rs",
        ToolStatus::Ok,
        "42 lines",
    );
    let lines = render(&c, 80, &opts, 0, false);
    let text = text_of(&lines);
    let label = rtext::tool_label("read_file").unwrap_or("read_file");
    let sep = opts.glyphs.dot_sep;
    assert!(
        text[0].contains(&format!("{label}{sep}crates/x.rs")),
        "{}",
        text[0]
    );
    let body = body_rows(&text, &opts);
    assert!(body.iter().all(|l| !l.contains("$ ")), "{body:?}");
    assert!(body[0].contains("42 lines"), "{}", body[0]);
}

#[test]
fn expanded_shows_everything_wrapped() {
    let opts = RenderOptions::new(60, false);
    let preview = lines_preview(20);
    let c = call("run", "run suite", ToolStatus::Ok, &preview);
    let lines = render(&c, 60, &opts, 0, true);
    let text = text_of(&lines);
    let body = body_rows(&text, &opts);
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
    assert!(body.iter().all(|l| !l.contains("ctrl+o")));
}

#[test]
fn expanded_is_still_bounded() {
    let opts = RenderOptions::new(60, false);
    let preview = lines_preview(5000);
    let c = call("run", "run suite", ToolStatus::Ok, &preview);
    let lines = render(&c, 60, &opts, 0, true);
    let text = text_of(&lines);
    let body = body_rows(&text, &opts);
    assert!(
        body.len() <= EXPANDED_MAX_LINES + 1,
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
fn a_diff_preview_renders_on_the_outcome_gutter() {
    let opts = RenderOptions::new(80, false);
    let diff = DiffStat {
        added: 1,
        removed: 1,
    };
    let mut c = call(
        "edit",
        "edit x.rs",
        ToolStatus::Ok,
        "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-old\n+new\n",
    );
    c.diff = Some(&diff);
    c.duration_ms = Some(40);
    let lines = render(&c, 80, &opts, 0, false);
    let text = text_of(&lines);

    let body = body_rows(&text, &opts);
    assert!(
        body.len() >= 3,
        "diff rows must hang on the gutter: {body:?}"
    );
    let joined = text.join("\n");
    assert!(joined.contains("@@"), "{joined}");
    // The outcome gutter tints the diff renderer's output, and the header
    // still carries the +A −R stat.
    assert_eq!(lines[1].spans[0].style, opts.theme.green);
    assert!(text[0].contains("+1"), "{}", text[0]);
    // No `$` line, no line cap — the diff renderer owns this body.
    assert!(body.iter().all(|l| !l.contains("$ ")), "{body:?}");
}
